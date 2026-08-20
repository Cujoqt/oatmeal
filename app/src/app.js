// Oatmeal — native recorder UI.
//
// Talks to the Rust commands: session start/stop (mic + system audio lanes,
// on-device Whisper), live transcription events, the meeting library, the local
// language model behind notes and recaps, and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core
const { listen, emit } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)

import { CHUNK_KEY, DEFAULT_CHUNK_SECS, EVENTS, chunkSeconds, getLang, setLang } from '/shared.js'
import { createDatePicker } from '/datepicker.js'
import { toMathML } from '/mathml.js'

const btn = $('btn')
const cluster = $('cluster')
const titleEl = $('title')
const notesEl = $('notes')
const saveHintEl = $('saveHint')
const introEl = $('intro')
const okayBtn = $('okay')
const revealBtn = $('reveal')
const expandBtn = $('expand')
const homeAsk = $('homeAsk')
const timerEl = $('timer')
const statusEl = $('status')
const themeBtn = $('theme')
const themeIcon = $('themeIcon')
const hideEl = $('hide')
const hideLabel = $('hideLabel')
const searchEl = $('search')
const sideList = $('sideList')
const sideCount = $('sideCount')
const folderList = $('folderList')
const folderNote = $('folderNote')
const newFolderBtn = $('newFolder')
const meetingsLabel = $('meetingsLabel')
const sortModeEl = $('sortMode')
const navHome = $('navHome')
const navSettings = $('navSettings')
const navHomework = $('navHomework')
const viewHomework = $('viewHomework')
const hwTitleEl = $('hwTitle')
const hwNoteInputEl = $('hwNoteInput')
const hwAddBtn = $('hwAdd')
const hwStatusEl = $('hwStatus')
const hwListEl = $('hwList')
const hwCountEl = $('hwCount')
const railToggle = $('railToggle')
const viewDash = $('viewDash')
const viewSettings = $('viewSettings')
const agendaEl = $('agenda')
const rangeEl = $('agRange')
const agPrev = $('agPrev')
const agNext = $('agNext')
const notesListEl = $('notesList')
const recallInput = $('recallInput')
const recallSend = $('recallSend')
const recallAnswers = $('recallAnswers')
const newNoteBtn = $('newNote')
const displayNameEl = $('displayName')
const greetingEl = $('greeting')
const chipWhoEl = $('chipWho')
const calNote = $('calNote')
const languageEl = $('language')
const followupStyleEl = $('followupStyle')
const followupCustomEl = $('followupCustom')
const followupCustomField = $('followupCustomField')
const chunkSecondsEl = $('chunkSeconds')
const modelPathEl = $('modelPath')
const settingsNote = $('settingsNote')
const acctEl = $('acct')
const acctLabel = $('acctLabel')
const modelChip = $('modelChip')
const modelTxt = $('modelTxt')
const viewHome = $('viewHome')
const viewNote = $('viewNote')
const noteTitle = $('noteTitle')
const noteBody = $('noteBody')
const chipWhen = $('chipWhen')
const chipFinish = $('chipFinish')
const chipContinue = $('chipContinue')
const chipExport = $('chipExport')
const chipFollowup = $('chipFollowup')
const chipVideo = $('chipVideo')
const videoPanel = $('videoPanel')
const videoUrl = $('videoUrl')
const videoMeta = $('videoMeta')
const videoStart = $('videoStart')
const videoEnd = $('videoEnd')
const videoImport = $('videoImport')
const chipDelete = $('chipDelete')
const tmplChips = $('tmplChips')
const tabNotes = $('tabNotes')
const tabRaw = $('tabRaw')
const answersEl = $('answers')
const suggestEl = $('suggest')
const askInput = $('askInput')
const askSend = $('askSend')
const updateStrip = $('updateStrip')
const updateStripText = $('updateStripText')
const updateGetBtn = $('updateGet')
const updateLaterBtn = $('updateLater')
const updateGate = $('updateGate')
const gateTitle = $('gateTitle')
const gateBody = $('gateBody')
const gateGetBtn = $('gateGet')
const toastEl = $('toast')
const versionLabel = $('versionLabel')
const updateCheckBtn = $('updateCheck')
const updateOpenBtn = $('updateOpen')
const brandVersionEl = $('brandVersion')

const DRAFT_KEY = 'oatmeal.draft'
const INTRO_KEY = 'oatmeal.introSeen'
/// Which version the "Later" button waved away, so an optional update nags once
/// per release rather than on every launch.
const UPDATE_SNOOZE_KEY = 'oatmeal.updateSnoozed'
const SUGGESTIONS = ['What did I miss?', 'What are the action items?', 'Summarize the decisions', 'What should I follow up on?']
// Matches chat::Template on the Rust side, serialized snake_case.
const TEMPLATES = [
  { id: 'general', label: 'General' },
  { id: 'standup', label: 'Standup' },
  { id: 'one_on_one', label: '1:1' },
  { id: 'interview', label: 'Interview' },
  { id: 'lecture', label: 'Lecture' },
]

let recording = false
let busy = false
let tick = null
let startedAt = 0
let meetings = []
let filter = ''
/// Content search results (title + transcript + notes) for the current
/// `filter`, or null while none has come back yet — `visibleMeetings()` falls
/// back to a title-only match until it does, so typing never shows an empty
/// list while the backend scan is in flight.
let searchResults = null
/// `id → [{ source, text }]` for the same results: the excerpts that made each
/// meeting match. Set from the same reply as `searchResults`, so the sidebar's
/// filter and the dashboard's result list can never disagree about what matched.
let searchHits = null
let searchTimer = null
let searchSeq = 0
let folders = []
let currentFolder = null

const SORT_KEY = 'oatmeal.sort'
/// 'new' | 'old' | 'az'. One sort for whatever list is on screen, folder or
/// not — a per-folder setting would need a stored map and a rule for folders
/// never visited, to answer a question nobody asked.
let sortMode = localStorage.getItem(SORT_KEY) || 'new'
let openId = null
let noteTab = 'notes'
let liveLines = []
let saveTimer = null
let hintTimer = null
let sessionDir = ''
/// The meeting a continuation is recording into, or null when the take in
/// progress (if any) is a brand-new meeting. Stopping has to know: a
/// continuation's folder already holds that meeting's typed notes, which the
/// draft on the new-note page must not be written over.
let continuingId = null
/// Element a streamed chat answer is being written into, or null between asks.
let streamingAnswer = null
let homework = []
let toastTimer = null

const hwDatePicker = createDatePicker($('hwDatePicker'))

/// `#status` lives inside the draft view, so on every other surface it is a
/// hidden element — which is how a failed export, a failed rename, and a failed
/// model download all came to look like nothing happening at all. The line still
/// belongs in the draft (it sits above the dock, and carries the recording
/// state), so anywhere else the same message goes to a toast instead.
function setStatus(msg, isErr = false) {
  statusEl.textContent = msg
  statusEl.classList.toggle('err', isErr)
  if (!viewHome.classList.contains('on')) showToast(msg, isErr)
}

function showToast(msg, isErr = false) {
  clearTimeout(toastTimer)
  if (!msg) {
    toastEl.classList.remove('show')
    return
  }
  toastEl.textContent = msg
  toastEl.classList.toggle('err', isErr)
  toastEl.classList.add('show')
  // Errors are worth reading twice; progress notes can go quietly.
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), isErr ? 7000 : 4000)
}

// ── tiny markdown renderer ───────────────────────────────────────────────────
//
// The model writes Markdown; only headings, bullets, bold and paragraphs are
// worth supporting. Everything goes through textContent, so model output can
// never inject markup.

function renderMarkdown(md, target) {
  // The prompt asks for a displayed equation's \[ and \] on the same line as
  // the equation, but nothing enforces that the model obeys — belt and
  // braces. The loop below is line-based (MATH doesn't match across a `\n`),
  // so a \[...\] pair broken onto three lines would otherwise render as two
  // literal-backslash paragraphs bracketing a raw-LaTeX paragraph. Join any
  // such pair onto one line first.
  md = md.replace(/\\\[([\s\S]*?)\\\]/g, (_, body) => `\\[${body.replace(/\s*\n\s*/g, ' ')}\\]`)

  target.innerHTML = ''
  let list = null

  // \(…\) is inline math, \[…\] is displayed. Dollar signs are deliberately not
  // a delimiter: this renderer is shared with every other template, where $50
  // is a price and not an equation.
  const MATH = /\\\((.+?)\\\)|\\\[(.+?)\\\]/g

  const bold = (el, text) => {
    // **bold** is the only inline form the prompts ask for.
    const parts = text.split(/\*\*(.+?)\*\*/g)
    parts.forEach((part, i) => {
      if (!part) return
      if (i % 2 === 1) {
        const b = document.createElement('strong')
        b.textContent = part
        el.appendChild(b)
      } else {
        el.appendChild(document.createTextNode(part))
      }
    })
  }

  const inline = (el, text) => {
    let last = 0
    for (const m of text.matchAll(MATH)) {
      if (m.index > last) bold(el, text.slice(last, m.index))
      el.appendChild(toMathML(m[1] ?? m[2], m[2] !== undefined))
      last = m.index + m[0].length
    }
    if (last < text.length) bold(el, text.slice(last))
  }

  for (const raw of md.split('\n')) {
    const line = raw.trim()
    if (!line) { list = null; continue }

    // A Lecture note follows each review question with this line, but the
    // model has been observed nesting it as a list item (`- > Solution:
    // ...`) instead of the bare blockquote the prompt asks for. Match with
    // an optional leading bullet marker, and check this *before* the bullet
    // branch below, so a bulleted solution can't be captured as an ordinary
    // list item first.
    const solution = line.match(/^(?:[-*]\s+)?>\s*Solution:\s*(.*)$/i)
    if (solution) {
      list = null
      const d = document.createElement('details')
      d.className = 'solution'
      const s = document.createElement('summary')
      s.textContent = 'Show solution'
      const body = document.createElement('p')
      inline(body, solution[1])
      d.append(s, body)
      target.appendChild(d)
      continue
    }

    const bullet = line.match(/^[-*]\s+(.*)$/)
    if (bullet) {
      if (!list) { list = document.createElement('ul'); target.appendChild(list) }
      const li = document.createElement('li')
      inline(li, bullet[1])
      list.appendChild(li)
      continue
    }
    list = null

    const heading = line.match(/^(#{1,6})\s+(.*)$/)
    if (heading) {
      const el = document.createElement(heading[1].length <= 2 ? 'h2' : 'h3')
      inline(el, heading[2])
      target.appendChild(el)
      continue
    }

    const p = document.createElement('p')
    inline(p, line)
    target.appendChild(p)
  }
}

// ── recording ────────────────────────────────────────────────────────────────

function fmtElapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const pad = (n) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s % 60)}` : `${pad(m)}:${pad(s % 60)}`
}

/// `elapsedMs` seeds the clock for a meeting that was already running — after a
/// relaunch, starting from zero would under-report it.
function startTimer(elapsedMs = 0) {
  startedAt = Date.now() - elapsedMs
  timerEl.textContent = fmtElapsed(elapsedMs)
  tick = setInterval(() => { timerEl.textContent = fmtElapsed(Date.now() - startedAt) }, 500)
}

function stopTimer() { clearInterval(tick); tick = null }

function toRecordButton() {
  document.body.classList.remove('recording')
  btn.title = 'Start recording'
  cluster.title = 'Start recording'
  btn.disabled = false
  titleEl.setAttribute('data-placeholder', 'New note')
}

function toStopButton() {
  document.body.classList.add('recording')
  btn.title = 'Stop recording'
  cluster.title = 'Stop recording'
  btn.disabled = false
  titleEl.setAttribute('data-placeholder', 'Untitled meeting')
}

// ── your own notes ───────────────────────────────────────────────────────────
//
// Distinct from the notes the model writes: this is what you type on the page,
// kept verbatim in notes.md next to the audio.

function draft() {
  return { title: titleEl.textContent.trim(), body: notesEl.textContent }
}

function flashHint(msg) {
  saveHintEl.textContent = msg
  saveHintEl.classList.add('show')
  clearTimeout(hintTimer)
  hintTimer = setTimeout(() => saveHintEl.classList.remove('show'), 1800)
}

function queueSave() {
  const d = draft()
  // Keep a local copy immediately so a crash or restart never loses typing.
  localStorage.setItem(DRAFT_KEY, JSON.stringify(d))
  clearTimeout(saveTimer)
  saveTimer = setTimeout(async () => {
    try {
      // Before any meeting has happened the backend holds the notes in memory
      // and returns null — there is nothing on disk to report yet.
      if (await invoke('save_notes', d)) flashHint('Saved to notes.md')
    } catch (e) {
      flashHint(String(e))
    }
  }, 800)
}

titleEl.addEventListener('input', queueSave)
notesEl.addEventListener('input', queueSave)

async function showTranscriptWindow(visible) {
  try {
    await invoke('set_transcript_window_visible', { visible })
  } catch (e) {
    setStatus(String(e), true)
  }
}

async function startRecording() {
  busy = true
  btn.disabled = true
  // Stay on the note being written: the dashboard has neither the editor nor the
  // dock, so switching there mid-record hides everything the meeting needs.
  if (!viewHome.classList.contains('on')) showDraft()
  liveLines = []

  try {
    setStatus('Preparing the transcription model (first run downloads it once)…')
    await invoke('ensure_model')

    setStatus('Starting capture…')
    const paths = await invoke('start_session', { title: draft().title, language: getLang() })
    sessionDir = paths.dir

    recording = true
    startTimer()
    toStopButton()
    // Flush whatever was typed before the folder existed, then open the panel.
    invoke('save_notes', draft()).catch(() => {})
    emit(EVENTS.session, { active: true })
    showTranscriptWindow(true)
    setStatus('Recording your mic and the other side of the call, locally.')
  } catch (e) {
    setStatus(String(e), true)
    toRecordButton()
  } finally {
    busy = false
  }
}

/// Keep recording into a meeting that was already stopped. The new audio lands
/// in fresh lane files in the same folder and is appended to that meeting's
/// transcript when this take stops, so the meeting keeps its id and its notes.
async function continueRecording(id) {
  busy = true
  try {
    setStatus('Preparing the transcription model (first run downloads it once)…')
    await invoke('ensure_model')

    setStatus('Starting capture…')
    const paths = await invoke('continue_session', { id, language: getLang() })
    sessionDir = paths.dir
    continuingId = id
    liveLines = []

    recording = true
    startTimer()
    toStopButton()
    emit(EVENTS.session, { active: true })
    showTranscriptWindow(true)
    setStatus('Recording again into this meeting.')
  } catch (e) {
    setStatus(String(e), true)
    toRecordButton()
  } finally {
    busy = false
    renderNoteActions()
  }
}

async function stopRecording() {
  busy = true
  btn.disabled = true
  stopTimer()
  document.body.classList.remove('recording')
  setStatus('Transcribing on-device — this can take a moment…')

  emit(EVENTS.state, { state: 'finishing', message: 'Writing the final transcript…' })

  // A continuation is recording into a meeting that already exists, so the
  // draft sitting on the new-note page is somebody else's writing: saving it
  // would overwrite that meeting's notes.md, and clearing it would throw away
  // a draft the user never finished.
  const continued = continuingId
  let landedId = continued
  try {
    if (!continued) await invoke('save_notes', draft()).catch(() => {})
    const res = await invoke('stop_session', { modelPath: '', language: getLang() })
    if (!continued) landedId = (res.dir || '').split('/').pop()
    setStatus('Done. Notes saved locally.')
    // Start writing the notes now rather than when the meeting is next opened.
    // Deliberately not awaited: the transcript is already saved, so this is
    // ahead-of-time work, and a failure here is not a failed recording — the
    // note view asks for the same thing again and will surface it there.
    //
    // Only for a meeting that just started life, where General is by definition
    // the right template. A continuation already has one chosen, and picking it
    // wrong here would spend a model run on notes nobody asked for.
    if (landedId && !continued) {
      invoke('write_notes', { id: landedId, template: 'general', force: false }).catch(() => {})
    }
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    recording = false
    continuingId = null
    liveLines = []
    if (!continued) {
      titleEl.textContent = ''
      notesEl.textContent = ''
      localStorage.removeItem(DRAFT_KEY)
    }
    emit(EVENTS.session, { active: false })
    toRecordButton()
    busy = false
    await loadMeetings()
    // Land in the note that was just recorded — that's the thing worth reading.
    if (landedId && meetings.some((m) => m.id === landedId)) openNote(landedId)
  }
  return landedId
}

/// Transcribe audio that was recorded but never written up — Oatmeal was killed
/// mid-meeting, or a continuation never got stopped cleanly. Always a deliberate
/// press: Whisper is the heaviest thing the app does.
async function finishMeeting(id) {
  if (busy) return
  busy = true
  setStatus('Transcribing the rest of this recording on-device — this can take a moment…')
  setModelChip('busy', 'Transcribing…')
  try {
    await invoke('finish_meeting', { id, modelPath: '', language: getLang() })
    await loadMeetings()
    setStatus('Transcript written.')
    if (openId === id) openNote(id)
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    busy = false
    refreshModelChip()
  }
}

// The whole cluster toggles recording, not just the 13px square inside it: the
// dots read as part of the same control, and clicking them used to do nothing.
// The chevron is the one thing in the pill with its own job.
cluster.addEventListener('click', (e) => {
  if (e.target.closest('#expand')) return
  if (busy) return
  recording ? stopRecording() : startRecording()
})

function isTypingTarget(target) {
  if (!(target instanceof Element)) return false
  return target.isContentEditable || Boolean(target.closest('input, textarea, select, button, [contenteditable="true"]'))
}

document.addEventListener('keydown', (e) => {
  if (e.code !== 'Space' || e.repeat || isTypingTarget(e.target)) return
  e.preventDefault()
  if (busy) return
  recording ? stopRecording() : startRecording()
})

// ── live transcript ──────────────────────────────────────────────────────────

// The floating transcript window renders these; here they only tell us whether
// there is anything to ask about yet.
listen('oatmeal://live-line', (event) => {
  const line = event.payload
  if (line && line.text) liveLines.push(line)
})

// The transcript window has the same stop button and hide control; it asks us to
// act so there is exactly one owner of the session state.
listen(EVENTS.toggleRecord, () => {
  if (busy) return
  recording ? stopRecording() : startRecording()
})
listen(EVENTS.hideTranscript, () => showTranscriptWindow(false))
listen(EVENTS.quitBlockedNotes, () => setStatus('Still writing notes — let it finish before quitting.', true))

expandBtn.addEventListener('click', async () => {
  const visible = await invoke('is_transcript_window_visible').catch(() => false)
  showTranscriptWindow(!visible)
})

// ── the dock's ask box ───────────────────────────────────────────────────────

homeAsk.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') askLive()
})

async function askLive() {
  const question = homeAsk.value.trim()
  if (!question) return
  if (!liveLines.length) {
    setStatus(recording ? 'Nothing has been transcribed yet.' : 'Open a meeting to ask about it.')
    return
  }
  homeAsk.value = ''
  setStatus('Thinking…')
  // Stream into the status line so an answer mid-meeting starts appearing at once.
  streamingAnswer = statusEl
  try {
    // An empty id means "the meeting happening right now".
    // This answer lands in the one-line status, where the caveat that sits under
    // a `.qa` answer would not fit — so it gets the short form of the same point.
    setStatus(`${await invoke('ask_meeting', { id: '', question })}  (AI — check the recording)`)
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    streamingAnswer = null
  }
}

// ── odds and ends ────────────────────────────────────────────────────────────

revealBtn.addEventListener('click', () => {
  setStatus(sessionDir ? `Saved in ${sessionDir}` : 'The folder is created when you start recording.')
})

/// The dock stays out of the way while the onboarding card is up: the card is
/// taller than the frame, so a bottom-pinned dock would lie across its text.
function setIntroVisible(visible) {
  introEl.hidden = !visible
  document.body.classList.toggle('intro', visible)
}

okayBtn.addEventListener('click', () => {
  setIntroVisible(false)
  localStorage.setItem(INTRO_KEY, '1')
  notesEl.focus()
})

// ── updates ──────────────────────────────────────────────────────────────────
//
// The check runs in Rust (the CSP lets this page talk to nothing but the IPC
// bridge). Three possible outcomes, and the quiet one is the default: a check
// that failed looks the same as no update, because being unable to reach GitHub
// must never cost somebody a meeting.

let update = null
/// When the last check finished, so the app can notice a release published
/// while it was running. Checking only at launch meant a release put out in the
/// afternoon was invisible to anyone who had opened Oatmeal that morning — and
/// Oatmeal is an app people leave open for days.
let lastUpdateCheck = 0
const UPDATE_RECHECK_MS = 6 * 60 * 60 * 1000
/// Coming back to the window is the moment someone is most likely to be waiting
/// on a release, so re-check then too — but not on every alt-tab.
const UPDATE_FOCUS_RECHECK_MS = 30 * 60 * 1000

async function refreshUpdate({ manual = false } = {}) {
  lastUpdateCheck = Date.now()
  if (manual) note(versionLabel, 'Checking…')
  try {
    update = await invoke('check_for_update')
  } catch (e) {
    update = null
    if (manual) note(versionLabel, String(e))
    return
  }
  // A newer format on disk than this build understands is the other way the
  // app can be too old to be safe to use.
  const data = await invoke('data_status').catch(() => null)
  applyUpdateState(data)
}

function downloadLink() {
  return update?.downloadUrl || update?.releaseUrl || null
}

function applyUpdateState(data) {
  renderVersionCard()

  if (data?.writesLocked) {
    // storedVersion is null when the stamp exists but couldn't be read, which is
    // a different sentence from "a newer version wrote this".
    const why = data.storedVersion
      ? `The notes in this folder were written by a newer version of Oatmeal
         (format ${data.storedVersion}, this build reads ${data.dataVersion}).`
      : `Oatmeal can't tell which format the notes in this folder are in, so it is
         treating them as newer than this build.`
    showGate(
      'Update Oatmeal to edit these notes',
      `<p>${why} Nothing has been changed — editing is turned off so an older build
       can't rewrite them into a shape it understands.</p>
       <p class="ver">Installed ${update?.current || ''}</p>`,
    )
    return
  }

  if (update?.mandatory) {
    showGate(
      'This version is out of date',
      `<p>Oatmeal ${update.latest} is required. This build can't be used until
       it's updated.</p>
       <p class="ver">Installed ${update.current} · required ${update.minimum}</p>`,
    )
    return
  }

  updateGate.classList.remove('show')
  const snoozed = localStorage.getItem(UPDATE_SNOOZE_KEY)
  const show = Boolean(update?.updateAvailable) && snoozed !== update.latest
  updateStrip.classList.toggle('show', show)
  if (show) {
    updateStripText.innerHTML = `<strong>Oatmeal ${update.latest}</strong> is available — you're on ${update.current}.`
  }
}

function showGate(title, bodyHtml) {
  gateTitle.textContent = title
  gateBody.innerHTML = bodyHtml
  gateGetBtn.hidden = !downloadLink()
  updateStrip.classList.remove('show')
  updateGate.classList.add('show')
}

function renderVersionCard() {
  if (!update) {
    note(versionLabel, 'Could not reach GitHub — you can keep working.')
    updateOpenBtn.hidden = true
    return
  }
  if (!update.checked) {
    note(versionLabel, `Oatmeal ${update.current} — could not check for updates.`)
    updateOpenBtn.hidden = true
    return
  }
  const newer = update.updateAvailable
  note(
    versionLabel,
    newer
      ? `Oatmeal ${update.current} — ${update.latest} is available.`
      : `Oatmeal ${update.current} — up to date.`,
    newer ? '' : 'ok',
  )
  updateOpenBtn.hidden = !newer || !downloadLink()
}

/// Install without leaving the app. Oatmeal fetches the disk image itself,
/// swaps the bundle and reopens — so this call only ever *returns* on failure,
/// because success takes the window with it.
///
/// Everything that can go wrong (no write access to /Applications, an image
/// that won't mount) falls back to the old behaviour: open the download in a
/// browser and let the person drag it across.
async function installUpdate(btn) {
  const url = update?.downloadUrl
  if (!url) return openDownload()
  const label = btn?.textContent
  if (btn) {
    btn.disabled = true
    btn.textContent = 'Installing…'
  }
  setStatus(`Downloading Oatmeal ${update.latest || ''} — it will restart when it's ready.`)
  try {
    await invoke('install_update', { url })
  } catch (e) {
    if (btn) {
      btn.disabled = false
      btn.textContent = label
    }
    setStatus(`${e} — opening the download instead.`, true)
    note(versionLabel, String(e), 'err')
    await openDownload()
  }
}

/// The fallback, and the only path for a release with no image attached: hand
/// the link to the browser.
async function openDownload() {
  const url = downloadLink()
  if (!url) return
  try {
    await invoke('open_update_download', { url })
  } catch (e) {
    note(versionLabel, String(e))
  }
}

updateGetBtn.addEventListener('click', () => installUpdate(updateGetBtn))
gateGetBtn.addEventListener('click', () => installUpdate(gateGetBtn))
updateOpenBtn.addEventListener('click', () => installUpdate(updateOpenBtn))
updateCheckBtn.addEventListener('click', () => refreshUpdate({ manual: true }))
updateLaterBtn.addEventListener('click', () => {
  if (update?.latest) localStorage.setItem(UPDATE_SNOOZE_KEY, update.latest)
  updateStrip.classList.remove('show')
})

window.addEventListener('focus', () => {
  if (Date.now() - lastUpdateCheck > UPDATE_FOCUS_RECHECK_MS) refreshUpdate()
})

// ── meeting library ──────────────────────────────────────────────────────────

function fmtDuration(secs) {
  if (!secs) return null
  return secs < 60 ? `${secs} sec` : `${Math.round(secs / 60)} min`
}

function fmtWhen(date) {
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate())
  const days = Math.round((startOfDay(new Date()) - startOfDay(date)) / 86400000)
  if (days === 0) return 'Today'
  if (days === 1) return 'Yesterday'
  if (days < 7) return date.toLocaleDateString(undefined, { weekday: 'long' })
  return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function fmtDueDate(iso) {
  const [y, m, d] = iso.split('-').map(Number)
  const date = new Date(y, m - 1, d)
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate())
  const days = Math.round((startOfDay(date) - startOfDay(new Date())) / 86400000)
  if (days === 0) return 'Today'
  if (days === 1) return 'Tomorrow'
  if (days < 0) return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) + ' (overdue)'
  if (days < 7) return date.toLocaleDateString(undefined, { weekday: 'long' })
  return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function visibleMeetings() {
  let list
  if (!filter) {
    list = meetings
  } else if (searchResults) {
    const ids = new Set(searchResults.map((m) => m.id))
    list = meetings.filter((m) => ids.has(m.id))
  } else {
    const q = filter.toLowerCase()
    list = meetings.filter((m) => m.title.toLowerCase().includes(q))
  }
  if (currentFolder) list = list.filter((m) => m.folder === currentFolder)
  return sortMeetings(list)
}

/// Applies the sidebar's sort mode. Sorting here rather than in each renderer
/// is what keeps the sidebar and the dashboard note list in the same order —
/// both read this one function.
function sortMeetings(list) {
  const sorted = list.slice()
  if (sortMode === 'az') {
    sorted.sort((a, b) => a.title.localeCompare(b.title))
  } else {
    // `list_meetings` already comes back newest-first, but a search reorders it
    // by relevance, so newest-first has to be asked for rather than assumed.
    sorted.sort((a, b) => new Date(b.started_at) - new Date(a.started_at))
    if (sortMode === 'old') sorted.reverse()
  }
  return sorted
}

/// Runs `filter` against transcript and notes content too, not just titles, and
/// keeps the excerpts that matched so the dashboard can show them.
/// Debounced so a full scan doesn't happen on every keystroke.
async function runSearch() {
  const q = filter
  const seq = ++searchSeq
  let hits
  try {
    hits = await invoke('search_snippets', { query: q })
  } catch (e) {
    // A search that fails silently looks exactly like a search with no matches.
    if (seq === searchSeq) setStatus(`Search failed: ${e}`, true)
    return
  }
  if (seq !== searchSeq || q !== filter) return
  searchResults = hits.map((h) => h.meeting)
  searchHits = new Map(hits.map((h) => [h.meeting.id, h.snippets]))
  renderSidebar()
  renderNotesList()
}

function renderSidebar() {
  const list = visibleMeetings()
  sideCount.textContent = meetings.length ? String(meetings.length) : ''
  sideList.innerHTML = ''

  if (!list.length) {
    const empty = document.createElement('div')
    empty.className = 'side-empty'
    empty.textContent = filter ? 'No matches.' : 'No meetings yet.'
    sideList.appendChild(empty)
    return
  }

  for (const m of list) {
    const item = document.createElement('button')
    item.className = 'side-item'
    if (m.id === openId) item.classList.add('on')
    if (m.has_notes) item.classList.add('done')

    const dot = document.createElement('span')
    dot.className = 'dot'

    const txt = document.createElement('span')
    txt.className = 'txt'
    const t = document.createElement('div')
    t.className = 't'
    t.textContent = m.title
    const s = document.createElement('div')
    s.className = 's'
    s.textContent = [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)].filter(Boolean).join(' · ')
    txt.append(t, s)

    const more = el('button', 'row-more', '⋯')
    more.title = 'Meeting options'
    more.setAttribute('aria-label', `Options for ${m.title}`)
    more.addEventListener('click', (e) => { e.stopPropagation(); openMeetingMenu(more, m) })

    item.append(dot, txt, more)
    item.addEventListener('click', () => openNote(m.id))
    item.draggable = true
    item.addEventListener('dragstart', (e) => {
      e.dataTransfer.effectAllowed = 'move'
      e.dataTransfer.setData('text/plain', m.id)
      liftGhost(e, item)
    })
    item.addEventListener('dragend', dropGhost)
    sideList.appendChild(item)
  }
}


/// Everything you can do to a meeting without opening it. The move items are
/// the click path to what dragging a row onto a folder does, so filing a
/// meeting no longer depends on a drag landing.
function openMeetingMenu(anchor, m) {
  const items = [{ head: 'Move to' }]
  const targets = folders.filter((f) => f.name !== m.folder)
  if (!targets.length) items.push({ hint: folders.length ? 'Already filed here' : 'No folders yet' })
  for (const f of targets) items.push({ label: f.name, run: () => dropMeetingOn(f.name, m.id) })
  if (m.folder) items.push({ label: 'Unsorted', run: () => dropMeetingOn(null, m.id) })
  items.push({ sep: true })
  items.push({ label: 'Delete', danger: true, confirm: true, run: () => deleteMeeting(m.id) })
  openMenu(anchor, items)
}

async function deleteMeeting(id) {
  try {
    await invoke('delete_meeting', { id })
    await refreshLibrary()
    if (openId === id) showHome()
  } catch (e) {
    setStatus(String(e), true)
  }
}

/// One row menu at a time, anchored under the button that opened it.
///
/// Items are `{ label, danger, confirm, run }`, plus `{ head }`, `{ hint }` and
/// `{ sep }` for the non-clickable parts. `confirm` arms the item on the first
/// click and runs it on the second — the two-step the note view's delete chip
/// uses, rather than a native `confirm()`, which blocks the webview and looks
/// like a browser rather than Oatmeal.
let menuEl = null

function closeMenu() {
  if (!menuEl) return
  menuEl.remove()
  menuEl = null
  document.removeEventListener('mousedown', onMenuOutside, true)
  document.removeEventListener('keydown', onMenuKey, true)
  document.removeEventListener('scroll', closeMenu, true)
  window.removeEventListener('resize', closeMenu)
}

function onMenuOutside(e) {
  if (menuEl && !menuEl.contains(e.target)) closeMenu()
}

function onMenuKey(e) {
  if (e.key === 'Escape') closeMenu()
}

function openMenu(anchorEl, items) {
  closeMenu()
  const menu = el('div', 'menu')
  for (const it of items) {
    if (it.head) { menu.appendChild(el('div', 'head', it.head)); continue }
    if (it.hint) { menu.appendChild(el('div', 'hint', it.hint)); continue }
    if (it.sep) { menu.appendChild(el('div', 'sep')); continue }
    const b = el('button', it.danger ? 'danger' : '', it.label)
    b.addEventListener('click', (e) => {
      e.stopPropagation()
      if (it.confirm && b.dataset.armed !== '1') {
        b.dataset.armed = '1'
        b.textContent = `Really ${it.label.toLowerCase()}?`
        return
      }
      closeMenu()
      it.run()
    })
    menu.appendChild(b)
  }
  document.body.appendChild(menu)

  // Measured after it is in the document — an unrendered menu has no size, and
  // a menu opened from the bottom of the list would hang off the window.
  const r = anchorEl.getBoundingClientRect()
  const w = menu.offsetWidth
  const h = menu.offsetHeight
  menu.style.left = `${Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8))}px`
  menu.style.top = `${r.bottom + 6 + h <= window.innerHeight ? r.bottom + 6 : Math.max(8, r.top - h - 6)}px`

  menuEl = menu
  menu.querySelector('button')?.focus()
  document.addEventListener('mousedown', onMenuOutside, true)
  document.addEventListener('keydown', onMenuKey, true)
  // Capture: the sidebar's lists scroll, not the window.
  document.addEventListener('scroll', closeMenu, true)
  window.addEventListener('resize', closeMenu)
}

/// The tilted thing that follows the cursor during a drag.
///
/// A drag image is snapshotted once, from whatever the element looks like when
/// `dragstart` returns — so tilting the row itself would tilt the row sitting
/// in the list and leave the cursor dragging an upright copy. Instead a clone
/// is rendered offscreen, rotated, and handed over as the drag image. The
/// clone can't be removed here: the snapshot happens after this returns, and
/// removing it first drags nothing at all. `dropGhost` clears it on `dragend`.
let ghostEl = null
let ghostSource = null

function liftGhost(e, item) {
  dropGhost()
  const rect = item.getBoundingClientRect()
  const ghost = item.cloneNode(true)
  ghost.classList.add('drag-ghost')
  ghost.classList.remove('on')
  // The clone is out of the sidebar's flex context, so it has no width of its own.
  ghost.style.width = `${rect.width}px`
  document.body.appendChild(ghost)
  if (e.dataTransfer.setDragImage) {
    e.dataTransfer.setDragImage(ghost, e.clientX - rect.left, e.clientY - rect.top)
  }
  ghostEl = ghost
  ghostSource = item
  item.classList.add('dragging')
}

function dropGhost() {
  ghostEl?.remove()
  ghostEl = null
  ghostSource?.classList.remove('dragging')
  ghostSource = null
}

async function loadMeetings() {
  try {
    meetings = await invoke('list_meetings')
  } catch {
    meetings = []
  }
  renderSidebar()
  renderNotesList()
}

async function loadFolders() {
  try {
    folders = await invoke('list_folders')
  } catch {
    folders = []
  }
  renderFolders()
}

async function refreshLibrary() {
  await Promise.all([loadMeetings(), loadFolders()])
}

/// `folderName` is a folder name to file into, or `null` to unfile back to
/// Unsorted. Reads which meeting is being dragged off the dataTransfer set in
/// `renderSidebar`'s `dragstart` handler.
async function dropMeetingOn(folderName, id) {
  if (!id) return
  try {
    await invoke('move_meeting_to_folder', { id, folder: folderName })
    setFolderNote('')
    await refreshLibrary()
  } catch (e) {
    setFolderNote(String(e))
  }
}

/// Folder errors go beside the folder list, not through `setStatus`: `#status`
/// lives inside the draft view, so a message written there is invisible while
/// the user is working in the sidebar.
function setFolderNote(msg) {
  folderNote.textContent = msg
}

function renderFolders() {
  meetingsLabel.classList.toggle('on', !currentFolder)
  folderList.innerHTML = ''
  for (const f of folders) {
    const row = el('button', 'folder-row' + (f.name === currentFolder ? ' on' : ''))
    const name = el('span', 'name', f.name)
    const count = el('span', 'count', String(f.count))
    const more = el('button', 'row-more', '⋯')
    more.title = 'Folder options'
    more.setAttribute('aria-label', `Options for ${f.name}`)
    more.addEventListener('click', (e) => {
      e.stopPropagation()
      openMenu(more, [
        { label: 'Rename', run: () => startRenameFolder(row, name, f.name) },
        { sep: true },
        { label: 'Delete', danger: true, run: () => askDeleteFolder(row, f.name) },
      ])
    })
    row.append(name, count, more)

    row.addEventListener('click', () => selectFolder(f.name))
    name.addEventListener('dblclick', (e) => { e.stopPropagation(); startRenameFolder(row, name, f.name) })

    row.addEventListener('dragover', (e) => { e.preventDefault(); row.classList.add('drag-over') })
    row.addEventListener('dragleave', () => row.classList.remove('drag-over'))
    row.addEventListener('drop', (e) => {
      e.preventDefault()
      row.classList.remove('drag-over')
      dropMeetingOn(f.name, e.dataTransfer.getData('text/plain'))
    })

    folderList.appendChild(row)
  }
}

/// Swaps a folder row's name span for a text input, committing the rename on
/// blur/Enter and reverting on Escape or an empty/unchanged value — same
/// idiom as the note title's `commitTitle`.
function startRenameFolder(row, nameEl, oldName) {
  const input = document.createElement('input')
  input.className = 'name'
  input.value = oldName
  row.replaceChild(input, nameEl)
  input.focus()
  input.select()

  const finish = async (commit) => {
    input.removeEventListener('blur', onBlur)
    input.removeEventListener('keydown', onKey)
    const next = input.value.trim()
    if (commit && next && next !== oldName) {
      try {
        await invoke('rename_folder', { old: oldName, new: next })
        if (currentFolder === oldName) currentFolder = next
        setFolderNote('')
        await loadFolders()
        return
      } catch (e) {
        setFolderNote(String(e))
      }
    }
    row.replaceChild(nameEl, input)
  }
  const onBlur = () => finish(true)
  const onKey = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); input.blur() }
    if (e.key === 'Escape') { finish(false) }
  }
  input.addEventListener('blur', onBlur)
  input.addEventListener('keydown', onKey)
}

/// Swaps the folder row's contents for a Delete/Cancel prompt — the same
/// swap-in-place `startRenameFolder` does, rather than a native `confirm()`,
/// which blocks the webview and looks like a browser rather than Oatmeal.
/// Escape cancels. Re-rendering the folder list is what restores the row, so
/// there is no saved-children bookkeeping to get wrong.
function askDeleteFolder(row, name) {
  const kids = [...row.children]
  const wrap = el('div', 'confirm')
  const q = el('span', 'q', `Delete "${name}"?`)
  const yes = el('button', 'yes', 'Delete')
  const no = el('button', '', 'Cancel')
  wrap.append(q, yes, no)
  row.replaceChildren(wrap)

  const cancel = () => {
    document.removeEventListener('keydown', onKey)
    row.replaceChildren(...kids)
  }
  const onKey = (e) => { if (e.key === 'Escape') cancel() }
  document.addEventListener('keydown', onKey)

  no.addEventListener('click', (e) => { e.stopPropagation(); cancel() })
  yes.addEventListener('click', (e) => {
    e.stopPropagation()
    document.removeEventListener('keydown', onKey)
    deleteFolder(name)
  })
  // The row itself selects the folder on click; a click anywhere in the prompt
  // must not also navigate.
  wrap.addEventListener('click', (e) => e.stopPropagation())
}

async function deleteFolder(name) {
  try {
    await invoke('delete_folder', { name })
    if (currentFolder === name) currentFolder = null
    setFolderNote('')
    await loadFolders()
  } catch (e) {
    setFolderNote(String(e))
  }
}

function selectFolder(name) {
  currentFolder = name
  renderFolders()
  renderSidebar()
  renderNotesList()
}

meetingsLabel.addEventListener('click', () => {
  currentFolder = null
  renderFolders()
  renderSidebar()
  renderNotesList()
})

// The select lives inside #meetingsLabel, whose click handler clears the
// folder — without this, changing the sort would also kick you out of the
// folder you were sorting.
sortModeEl.addEventListener('click', (e) => e.stopPropagation())
sortModeEl.addEventListener('change', () => {
  sortMode = sortModeEl.value
  localStorage.setItem(SORT_KEY, sortMode)
  renderSidebar()
  renderNotesList()
})

meetingsLabel.addEventListener('dragover', (e) => { e.preventDefault(); meetingsLabel.classList.add('drag-over') })
meetingsLabel.addEventListener('dragleave', () => meetingsLabel.classList.remove('drag-over'))
meetingsLabel.addEventListener('drop', (e) => {
  e.preventDefault()
  meetingsLabel.classList.remove('drag-over')
  dropMeetingOn(null, e.dataTransfer.getData('text/plain'))
})

searchEl.addEventListener('input', () => {
  filter = searchEl.value.trim()
  searchResults = null
  searchHits = null
  renderSidebar()
  renderNotesList()
  clearTimeout(searchTimer)
  if (filter) searchTimer = setTimeout(runSearch, 150)
})
navHome.addEventListener('click', showHome)
navSettings.addEventListener('click', showSettings)
newNoteBtn.addEventListener('click', showDraft)

newFolderBtn.addEventListener('click', () => {
  const row = el('div', 'folder-row')
  const input = document.createElement('input')
  input.className = 'name'
  input.placeholder = 'Folder name'
  row.appendChild(input)
  folderList.prepend(row)
  input.focus()

  const finish = async () => {
    input.removeEventListener('blur', finish)
    input.removeEventListener('keydown', onKey)
    const name = input.value.trim()
    row.remove()
    if (!name) return
    try {
      await invoke('create_folder', { name })
      setFolderNote('')
      await loadFolders()
    } catch (e) {
      setFolderNote(String(e))
    }
  }
  const onKey = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); input.blur() }
    if (e.key === 'Escape') { input.value = ''; input.blur() }
  }
  input.addEventListener('blur', finish)
  input.addEventListener('keydown', onKey)
})

// ── views ────────────────────────────────────────────────────────────────────

/// Five surfaces share the content pane: the Coming-up dashboard, the blank
/// note you type into, a recorded meeting, Settings, and Homework. One function
/// owns which is lit so the nav highlight can never drift from the view.
function showView(view) {
  viewDash.classList.toggle('on', view === 'dash')
  viewHome.classList.toggle('on', view === 'draft')
  viewNote.classList.toggle('on', view === 'note')
  viewSettings.classList.toggle('on', view === 'settings')
  viewHomework.classList.toggle('on', view === 'homework')
  navHome.classList.toggle('on', view === 'dash')
  navSettings.classList.toggle('on', view === 'settings')
  navHomework.classList.toggle('on', view === 'homework')
  for (const [el, active] of [[navHome, view === 'dash'], [navSettings, view === 'settings'], [navHomework, view === 'homework']]) {
    active ? el.setAttribute('aria-current', 'page') : el.removeAttribute('aria-current')
  }
}

/// Home: what's coming up, and everything already recorded.
function showHome() {
  openId = null
  showView('dash')
  renderAgenda()
  renderNotesList()
  renderSidebar()
}

/// The blank note — "+ New note", or the sidebar's own entry into a draft.
function showDraft() {
  openId = null
  showView('draft')
  renderSidebar()
  if (!recording) titleEl.focus()
}

function showSettings() {
  openId = null
  showView('settings')
  loadSettings()
  renderSidebar()
}

function showHomework() {
  openId = null
  showView('homework')
  renderSidebar()
  loadHomework()
}

navHomework.addEventListener('click', showHomework)

function currentMeeting() {
  return meetings.find((m) => m.id === openId)
}

async function openNote(id) {
  openId = id
  noteTab = 'notes'
  showView('note')
  answersEl.innerHTML = ''
  resetVideoPanel()
  renderSidebar()
  renderSuggestions()

  const m = currentMeeting()
  if (!m) return
  noteTitle.value = m.title
  chipWhen.textContent = [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)].filter(Boolean).join(' · ')
  renderTemplateChips()
  renderNoteActions()
  setTab('notes')
}

/// The two chips whose labels depend on what is happening right now: whether
/// this meeting has audio nobody has transcribed, and whether it is the one
/// being recorded into.
function renderNoteActions() {
  const m = currentMeeting()
  if (!m) return
  const pending = (m.pending_segments || []).length > 0
  const live = continuingId === m.id

  // Never while anything is recording: those lane WAVs are still being written,
  // and after a relaunch mid-meeting the UI cannot tell which meeting that is.
  // (`finish_meeting` refuses it in Rust too — this only keeps it off screen.)
  chipFinish.hidden = !pending || recording
  // While another meeting is recording there is no second session to offer.
  chipContinue.hidden = recording && !live
  chipContinue.lastChild.textContent = live ? ' Stop recording' : ' Continue recording'
  chipContinue.classList.toggle('danger', live)
}

chipContinue.addEventListener('click', () => {
  const m = currentMeeting()
  if (!m || busy) return
  continuingId === m.id ? stopRecording() : continueRecording(m.id)
})

chipFinish.addEventListener('click', () => {
  const m = currentMeeting()
  if (m) finishMeeting(m.id)
})

/// Which shape of notes to write, remembered per meeting in `meta.json` once
/// generated. Picking a different template on a meeting that already has notes
/// regenerates them; picking one before the first generation just selects it.
function renderTemplateChips() {
  const m = currentMeeting()
  tmplChips.innerHTML = ''
  if (!m || !m.transcribed) return
  const current = m.template || 'general'
  for (const t of TEMPLATES) {
    const b = document.createElement('button')
    b.className = 'chip act' + (t.id === current ? ' sel' : '')
    b.textContent = t.label
    b.addEventListener('click', () => selectTemplate(m, t.id))
    tmplChips.appendChild(b)
  }
}

async function selectTemplate(m, id) {
  if (id === (m.template || 'general')) return
  m.template = id
  renderTemplateChips()
  if (m.has_notes) await renderNotes(true, true)
}

function setTab(tab) {
  noteTab = tab
  tabNotes.classList.toggle('on', tab === 'notes')
  tabRaw.classList.toggle('on', tab === 'raw')
  tab === 'notes' ? renderNotes() : renderTranscript()
}

tabNotes.addEventListener('click', () => setTab('notes'))
tabRaw.addEventListener('click', () => setTab('raw'))

/// Show what the user typed during the meeting under the model's write-up. The
/// two used to share one file, which is why typing during a meeting appeared in
/// the Enhanced tab; they are separate now, so the view fetches both.
async function appendTypedNotes(id) {
  let typed = ''
  try {
    typed = await invoke('meeting_typed_notes', { id })
  } catch { /* an unreadable notes.md shouldn't blank the summary */ }
  if (!typed.trim() || openId !== id || noteTab !== 'notes') return

  const heading = document.createElement('h2')
  heading.textContent = 'Your notes'
  noteBody.appendChild(heading)
  renderMarkdown(typed, noteBody.appendChild(document.createElement('div')))
}

async function renderTranscript() {
  const m = currentMeeting()
  noteBody.innerHTML = '<p class="placeholder">Loading transcript…</p>'
  if (!m || !m.transcribed) {
    noteBody.innerHTML = '<p class="placeholder">This recording has no transcript — it was stopped before Whisper ran, or no speech was detected.</p>'
    return
  }
  const asked = m.id
  try {
    const segs = await invoke('meeting_segments', { id: m.id })
    // Same race as the notes path: a big transcript can arrive after the user has
    // moved on to another meeting or switched tabs.
    if (openId !== asked || noteTab !== 'raw') return
    noteBody.innerHTML = ''
    if (!segs.length) {
      noteBody.innerHTML = '<p class="placeholder">(no speech detected)</p>'
      return
    }
    for (const turn of groupTurns(segs)) {
      const block = el('div', 'seg-turn')
      const head = el('div', 'who')
      head.appendChild(el('time', '', turn.lines[0].at))
      const p = el('p', '', turn.lines.map((l) => l.text).join(' '))
      block.append(head, p)
      noteBody.appendChild(block)
    }
  } catch (e) {
    noteBody.innerHTML = ''
    const p = document.createElement('p')
    p.className = 'placeholder'
    p.textContent = String(e)
    noteBody.appendChild(p)
  }
}

/// Consecutive lines read as one paragraph, with one timestamp on the front.
///
/// Whisper cuts a line every time someone pauses, which is why a transcript
/// arrives as a list of fragments. A block ends once it has run longer than the
/// chunk length — a long stretch still has to break somewhere a reader can
/// follow.
function groupTurns(lines) {
  const limit = chunkSeconds()
  const turns = []
  for (const line of lines) {
    const last = turns[turns.length - 1]
    if (last && atSecs(line.at) - atSecs(last.lines[0].at) < limit) last.lines.push(line)
    else turns.push({ lines: [line] })
  }
  return turns
}

/// `M:SS` (or `H:MM:SS`) as seconds.
function atSecs(at) {
  return String(at)
    .split(':')
    .map(Number)
    .reduce((total, part) => total * 60 + (part || 0), 0)
}

async function renderNotes(force = false, regenerate = false) {
  const m = currentMeeting()
  if (!m) return

  if (!m.transcribed) {
    noteBody.innerHTML = '<p class="placeholder">Nothing to summarize — this recording has no transcript.</p>'
    return
  }

  // Whether the model actually ran decides whether the "these notes are out of
  // date" flag has been cleared on disk: a cached write-up comes back untouched.
  const wasMissing = !m.has_notes

  if (!m.has_notes && !force) {
    // Don't silently kick off a multi-gigabyte download or a long generation:
    // make it a deliberate press.
    noteBody.innerHTML = ''
    const wrap = document.createElement('div')
    wrap.className = 'note-empty'
    const p = document.createElement('p')
    p.textContent = modelReady
      ? 'Oatmeal can read this transcript and write it up — summary, key points, decisions and action items.'
      : `Writing notes needs the local language model (~${modelMb} MB, downloaded once). Everything still runs on this machine.`
    const go = document.createElement('button')
    go.className = 'gen'
    go.textContent = modelReady ? 'Write the notes' : 'Download model and write notes'
    go.addEventListener('click', () => renderNotes(true))
    wrap.append(p, go)
    noteBody.appendChild(wrap)
    // No summary yet doesn't mean nothing was written: show what was typed.
    await appendTypedNotes(m.id)
    return
  }

  noteBody.innerHTML = '<p class="placeholder">Reading the transcript and writing it up… this takes a minute on first run.</p>'
  setModelChip('busy', 'Writing notes…')
  const asked = m.id
  try {
    const md = await invoke('write_notes', { id: m.id, template: m.template || 'general', force: regenerate })
    m.has_notes = true
    if (regenerate || wasMissing) m.notes_stale = false
    renderSidebar()
    renderNotesList()
    // The user may have opened a different meeting while this was generating.
    if (openId !== asked || noteTab !== 'notes') return
    renderMarkdown(md, noteBody)
    if (m.notes_stale) noteBody.prepend(staleBanner())
    await appendTypedNotes(asked)
  } catch (e) {
    noteBody.innerHTML = ''
    const p = document.createElement('p')
    p.className = 'placeholder'
    p.textContent = String(e)
    noteBody.appendChild(p)
  } finally {
    refreshModelChip()
  }
}

/// A source was added to this meeting after the model wrote it up — more audio
/// recorded, or a video attached — so the notes on screen describe only part of
/// it. Rewriting them is a full model run, so this says so and offers the button
/// rather than spending the time unasked.
function staleBanner() {
  const wrap = el('div', 'stale')
  wrap.append(el('span', '', 'This meeting has more in it than these notes were written from.'))
  const go = el('button', '', 'Regenerate')
  go.addEventListener('click', () => renderNotes(true, true))
  wrap.append(go)
  return wrap
}

// Rename from the note view's title field.
noteTitle.addEventListener('blur', commitTitle)
noteTitle.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') { e.preventDefault(); noteTitle.blur() }
  if (e.key === 'Escape') { const m = currentMeeting(); if (m) noteTitle.value = m.title; noteTitle.blur() }
})

async function commitTitle() {
  const m = currentMeeting()
  if (!m) return
  const title = noteTitle.value.trim()
  if (!title || title === m.title) { noteTitle.value = m.title; return }
  try {
    await invoke('rename_meeting', { id: m.id, title })
    m.title = title
    renderSidebar()
  } catch (e) {
    noteTitle.value = m.title
    setStatus(String(e), true)
  }
}

chipExport.addEventListener('click', async () => {
  const m = currentMeeting()
  if (!m) return
  try {
    const dest = await invoke('export_meeting', { id: m.id })
    setStatus(`Exported to ${dest}`)
  } catch (e) {
    setStatus(String(e), true)
  }
})

chipDelete.addEventListener('click', async () => {
  const m = currentMeeting()
  if (!m) return
  if (chipDelete.dataset.armed !== '1') {
    chipDelete.dataset.armed = '1'
    chipDelete.lastChild.textContent = ' Really delete?'
    setTimeout(() => {
      chipDelete.dataset.armed = ''
      chipDelete.lastChild.textContent = ' Delete'
    }, 4000)
    return
  }
  try {
    await invoke('delete_meeting', { id: m.id })
    await loadMeetings()
    showHome()
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    chipDelete.dataset.armed = ''
    chipDelete.lastChild.textContent = ' Delete'
  }
})

// ── slept mid-recording ──────────────────────────────────────────────────────
//
// macOS captures nothing while the machine is asleep, so the stretch the lid was
// shut for is simply missing. Rather than leave one take with a hole in it, the
// take is stopped here and the meeting is offered back for a fresh one — which
// is what `continueRecording` already does for a meeting picked up later.

const sleepStrip = $('sleepStrip')
const sleepStripText = $('sleepStripText')
let sleptMeetingId = null

function fmtGap(ms) {
  const mins = Math.round(ms / 60000)
  if (mins < 1) return 'a moment'
  if (mins < 60) return `${mins} minute${mins === 1 ? '' : 's'}`
  const hrs = Math.round(mins / 60)
  return `${hrs} hour${hrs === 1 ? '' : 's'}`
}

listen(EVENTS.slept, async (e) => {
  if (!recording) return
  const gap = fmtGap(Number(e.payload?.asleep_ms) || 0)
  sleptMeetingId = await stopRecording()
  sleepStripText.innerHTML =
    `<strong>Your Mac slept for ${gap} while recording.</strong> ` +
    'Nothing was captured during that time, so the recording was stopped and saved.'
  $('sleepResume').style.display = sleptMeetingId ? '' : 'none'
  sleepStrip.classList.add('show')
})

$('sleepResume').addEventListener('click', () => {
  sleepStrip.classList.remove('show')
  if (sleptMeetingId) continueRecording(sleptMeetingId)
})

$('sleepDismiss').addEventListener('click', () => {
  sleepStrip.classList.remove('show')
  sleptMeetingId = null
})

// ── ask ──────────────────────────────────────────────────────────────────────

function renderSuggestions() {
  suggestEl.innerHTML = ''
  for (const s of SUGGESTIONS) {
    const b = document.createElement('button')
    b.textContent = s
    b.addEventListener('click', () => { askInput.value = s; ask() })
    suggestEl.appendChild(b)
  }
}

askSend.addEventListener('click', () => ask())
askInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') ask() })

// Tokens arrive from Rust as they're generated. Appending them beats waiting for
// the whole paragraph, which reads as a hang on a laptop-sized model.
listen(EVENTS.chatToken, ({ payload }) => {
  if (!streamingAnswer || !payload || !payload.text) return
  // First token replaces the placeholder, the rest append.
  if (payload.seq === 1) {
    streamingAnswer.classList.remove('thinking', 'err')
    streamingAnswer.textContent = ''
  }
  streamingAnswer.textContent += payload.text
})

async function ask() {
  const m = currentMeeting()
  const question = askInput.value.trim()
  if (!m || !question) return
  askInput.value = ''
  await runChat(question, 'Thinking…', () => invoke('ask_meeting', { id: m.id, question }))
}

/// Draft a follow-up message from this meeting's notes, into the same answers
/// list an ask writes to. Sending it is the user's job — Oatmeal never sends
/// anything itself — so a finished draft gets a copy button.
async function draftFollowup() {
  const m = currentMeeting()
  if (!m) return
  const a = await runChat('Follow-up draft', 'Drafting…', () => invoke('draft_followup', { id: m.id }))
  if (!a) return
  const copy = document.createElement('button')
  copy.className = 'copy'
  copy.textContent = 'Copy'
  copy.addEventListener('click', () => {
    navigator.clipboard.writeText(a.textContent)
    copy.textContent = 'Copied'
    setTimeout(() => { copy.textContent = 'Copy' }, 1500)
  })
  a.after(copy)
}

/// Add a prompt and its pending answer to a list, run `generate` against the
/// local model with the chat controls disabled, and stream the reply into the
/// answer. Returns the answer element, or null if the model failed.
///
/// `into` and `disable` are what the dashboard's library-wide ask varies: a
/// different answers list and a different send button. Everything else — the
/// pending placeholder, the token stream, the model chip, re-enabling on
/// failure — is identical, and there is deliberately only one copy of it.
async function runChat(prompt, pending, generate, { into = answersEl, disable = [askSend, chipFollowup] } = {}) {
  const qa = document.createElement('div')
  qa.className = 'qa'
  const q = document.createElement('div')
  q.className = 'q'
  q.textContent = prompt
  const a = document.createElement('div')
  a.className = 'a thinking'
  a.textContent = pending
  q.appendChild(dismiss(qa, a))
  qa.append(q, a)
  into.appendChild(qa)
  qa.scrollIntoView({ behavior: 'smooth', block: 'end' })

  for (const el of disable) el.disabled = true
  streamingAnswer = a
  setModelChip('busy', pending)
  try {
    // The returned string is authoritative: it also covers any event that was
    // dropped while the window was busy.
    a.textContent = await generate()
    // Last, so the copy button and citations a caller inserts with `a.after()`
    // land above it rather than below the small print.
    a.after(caveat())
    return a
  } catch (e) {
    a.textContent = String(e)
    return null
  } finally {
    a.classList.remove('thinking')
    streamingAnswer = null
    for (const el of disable) el.disabled = false
    refreshModelChip()
  }
}

const TRASH = '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7h16" /><path d="M10 4h4M6 7l1 13h10l1-13" /><path d="M10 11v6M14 11v6" /></svg>'

/// Throw one question and its answer away. The dashboard's list is never
/// cleared for you — a note view wipes its answers when you open another note,
/// but the library-wide ask has no such moment — so without this, every
/// question asked in a session stays stacked above the agenda forever.
///
/// Removing the wrapper is enough: the caveat, the copy button and the citation
/// chips are all inserted with `a.after()`, which puts them inside it. Dropping
/// the streaming reference matters though — the token handler would otherwise
/// keep writing into a node that is no longer in the document.
function dismiss(qa, a) {
  const b = document.createElement('button')
  b.className = 'del'
  b.title = 'Delete this question'
  b.innerHTML = TRASH
  b.addEventListener('click', () => {
    if (streamingAnswer === a) streamingAnswer = null
    qa.remove()
  })
  return b
}

/// The small print under a generated answer.
///
/// Only successful answers get one: an error message is not a claim about the
/// meeting, and hedging it would read as though the app were unsure whether it
/// had failed. Rust refuses outright when the recording can't support an answer
/// (`chat::NOT_DISCUSSED`), so this covers the remaining case — an answer that
/// is grounded but may still have read the transcript wrong.
function caveat() {
  const el = document.createElement('div')
  el.className = 'caveat'
  el.textContent =
    'AI-generated from your transcript, on your machine. It can be incomplete or wrong — check the recording before relying on it.'
  return el
}

// ── ask across the whole library ─────────────────────────────────────────────
//
// The question people actually have is "what did we decide about pricing?" —
// they don't know which meeting holds the answer, which is the one thing the
// per-meeting ask above cannot help with. Rust picks the meetings and streams
// the reply through the same event, so this only has to render the citations.

recallSend.addEventListener('click', () => askLibrary())
recallInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') askLibrary() })

async function askLibrary() {
  const question = recallInput.value.trim()
  if (!question) return
  recallInput.value = ''

  let result = null
  let failure = null
  const a = await runChat(
    question,
    'Searching your meetings…',
    async () => {
      try {
        result = await invoke('ask_library', { question })
        return result.answer
      } catch (e) {
        // runChat renders this into the answer, but the reason is worth a
        // toast too — the dashboard scrolls, and the answer may be off-screen.
        failure = e
        throw e
      }
    },
    { into: recallAnswers, disable: [recallSend] }
  )
  if (failure) {
    setStatus(String(failure), true)
    return
  }
  if (a && result.sources.length) renderCitations(a, result.sources)
}

/// Chips naming the meetings behind an answer. Titles are user- and
/// transcript-derived text, so they go in through textContent, never markup.
function renderCitations(answerEl, sources) {
  const cites = document.createElement('div')
  cites.className = 'cites'
  sources.forEach((s, i) => {
    const b = document.createElement('button')
    const n = document.createElement('b')
    n.textContent = `[${i + 1}]`
    const label = document.createElement('span')
    label.textContent = s.title
    b.append(n, label)
    b.title = `Open “${s.title}”`
    b.addEventListener('click', () => openNote(s.id))
    cites.appendChild(b)
  })
  answerEl.after(cites)
}

chipFollowup.addEventListener('click', draftFollowup)

/// The video whose title and length the panel is currently showing, or null.
/// The import button stays disabled until a probe has succeeded: a range can't
/// be checked against a video nobody has looked up yet, and transcribing the
/// wrong ten minutes is the failure this feature has to avoid.
let probedVideo = null

/// Hides the panel and blanks the url/range inputs, the probed-title line, and
/// the probe result. Switching notes must call this: `probedVideo` and the
/// input values otherwise survive the switch, so the panel would keep showing
/// one note's video — with Transcribe still enabled — while `openId` points at
/// a different note, and pressing it would attach that video to the wrong one.
function resetVideoPanel() {
  videoPanel.hidden = true
  videoUrl.value = ''
  videoStart.value = ''
  videoEnd.value = ''
  videoMeta.textContent = ''
  probedVideo = null
  videoImport.disabled = true
}

chipVideo.addEventListener('click', () => {
  if (videoPanel.hidden) {
    videoPanel.hidden = false
    videoUrl.focus()
  } else {
    resetVideoPanel()
  }
})

videoUrl.addEventListener('change', async () => {
  const url = videoUrl.value.trim()
  probedVideo = null
  videoImport.disabled = true
  if (!url) {
    videoMeta.textContent = ''
    return
  }
  videoMeta.textContent = 'Looking it up…'
  try {
    probedVideo = await invoke('video_probe', { url })
    videoMeta.textContent = `${probedVideo.title} — ${clock(probedVideo.duration_secs)}`
    videoImport.disabled = false
  } catch (e) {
    videoMeta.textContent = ''
    setStatus(String(e), true)
  }
})

videoImport.addEventListener('click', async () => {
  if (!probedVideo || !openId) return
  videoImport.disabled = true
  setStatus('Transcribing the video — this takes a few minutes.')
  try {
    await invoke('video_import', {
      meetingId: openId,
      url: videoUrl.value.trim(),
      start: videoStart.value,
      end: videoEnd.value,
    })
    resetVideoPanel()
    setStatus('Video added. Regenerate the notes to fold it in.')
    // The import set `notes_stale` on disk; the cached record still says false,
    // and `renderNotes` reads the cache — so without this reload the banner
    // offering to regenerate never appears and the note looks unchanged.
    await loadMeetings()
    await openNote(openId)
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    videoImport.disabled = !probedVideo
  }
})

/// Seconds to `12:30` / `1:05:20`, for the panel's duration line.
function clock(secs) {
  const t = Math.max(0, Math.floor(secs))
  const h = Math.floor(t / 3600)
  const m = Math.floor((t % 3600) / 60)
  const s = t % 60
  const pad = (n) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

// ── local model status ───────────────────────────────────────────────────────

let modelReady = false
let modelMb = 0

function setModelChip(state, text) {
  modelChip.classList.remove('ready', 'busy')
  if (state) modelChip.classList.add(state)
  modelTxt.textContent = text
}

async function refreshModelChip() {
  try {
    const s = await invoke('chat_model_status')
    modelReady = s.present
    modelMb = s.approx_mb
    setModelChip(
      s.present ? 'ready' : '',
      s.present ? (s.loaded ? 'Local model resident' : 'Local model ready') : `Local model not downloaded (~${s.approx_mb} MB)`,
    )
  } catch {
    setModelChip('', 'Local model unavailable')
  }
}

modelChip.addEventListener('click', async () => {
  if (modelReady) {
    // Second click frees the memory it's holding.
    await invoke('unload_chat_model')
    refreshModelChip()
    return
  }
  setModelChip('busy', 'Downloading local model…')
  try {
    await invoke('ensure_chat_model')
  } catch (e) {
    setStatus(String(e), true)
  }
  refreshModelChip()
})

// ── theme ────────────────────────────────────────────────────────────────────

const THEMES = ['system', 'light', 'dark']
const SUN = '<circle cx="12" cy="12" r="4.2" /><path d="M12 2.6v2.2M12 19.2v2.2M4.2 12H2M22 12h-2.2M6.3 6.3 4.8 4.8M19.2 19.2l-1.5-1.5M17.7 6.3l1.5-1.5M4.8 19.2l1.5-1.5" />'
const MOON = '<path d="M20 14.2A8.2 8.2 0 0 1 9.8 4a8.4 8.4 0 1 0 10.2 10.2Z" />'
const AUTO = '<circle cx="12" cy="12" r="8.4" /><path d="M12 3.6v16.8" /><path d="M12 3.6a8.4 8.4 0 0 1 0 16.8" fill="currentColor" stroke="none" />'

let theme = localStorage.getItem('oatmeal.theme') || 'system'

function applyTheme() {
  if (theme === 'system') document.documentElement.removeAttribute('data-theme')
  else document.documentElement.setAttribute('data-theme', theme)
  themeIcon.innerHTML = theme === 'light' ? SUN : theme === 'dark' ? MOON : AUTO
  themeBtn.title = theme === 'system' ? 'Theme: follows macOS' : `Theme: ${theme}`
}

themeBtn.addEventListener('click', () => {
  theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]
  localStorage.setItem('oatmeal.theme', theme)
  applyTheme()
})

// ── hide-from-capture ────────────────────────────────────────────────────────

async function refreshHide() {
  try {
    const hidden = await invoke('is_hidden_from_capture')
    hideEl.classList.toggle('on', hidden)
    hideLabel.textContent = hidden ? 'hidden from shares' : 'visible to shares'
  } catch { /* non-macOS / not ready */ }
}

hideEl.addEventListener('click', async () => {
  try {
    const hidden = await invoke('is_hidden_from_capture')
    await invoke('set_hidden_from_capture', { hidden: !hidden })
    refreshHide()
  } catch (e) {
    setStatus(String(e), true)
  }
})

// ── coming up ────────────────────────────────────────────────────────────────
//
// The agenda comes from Apple Calendar via EventKit (`list_events`) — whatever
// calendars this Mac already has. Without permission it says so and offers the one
// action that fixes it, rather than faking a day.

const AGENDA_DAYS = 60
const AGENDA_REFRESH_MS = 5 * 60 * 1000

let agenda = { authorized: false, denied: false, events: [] }
let agendaLoading = true
let displayName = ''

/// The name from Settings, used where the app addresses you. With no name set the
/// greeting stays empty rather than guessing at "there".
function renderGreeting() {
  const hour = new Date().getHours()
  const part = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'
  greetingEl.textContent = displayName ? `${part}, ${displayName}` : ''
  chipWhoEl.textContent = displayName || 'Me'
}

function el(tag, cls, text) {
  const node = document.createElement(tag)
  if (cls) node.className = cls
  if (text != null) node.textContent = text
  return node
}

function startOfDay(d) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate())
}

/// Timed events carry an offset; all-day events are bare dates, which need the
/// explicit midnight or Safari reads them as UTC.
function eventStart(ev) {
  return new Date(ev.all_day ? `${ev.start}T00:00:00` : ev.start)
}

function fmtClock(date) {
  return date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }).toLowerCase()
}

/// How many days one page of the agenda covers.
const AGENDA_PAGE_DAYS = 7

/// Which page is showing: 0 is the week starting today, 1 the next, and so on.
let agendaPage = 0

function dayKey(date) {
  return startOfDay(date).getTime()
}

function fmtDayRange(from, to) {
  const opts = { month: 'short', day: 'numeric' }
  return `${from.toLocaleDateString(undefined, opts)} – ${to.toLocaleDateString(undefined, opts)}`
}

/// `9:00 – 10:00 AM` for a timed event, `All day` otherwise. The meridiem is only
/// spelled out once when both ends share it, which is how a calendar reads.
function fmtSpan(ev) {
  if (ev.all_day) return 'All day'
  const start = ev.at
  const end = ev.end ? new Date(ev.end) : null
  const time = (d) => d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
  if (!end || Number.isNaN(end.getTime())) return time(start).toUpperCase()
  const [a, b] = [time(start), time(end)]
  const meridiem = (t) => t.slice(-2).toUpperCase()
  return meridiem(a) === meridiem(b)
    ? `${a.replace(/\s?[AP]M$/i, '')} – ${b.replace(/\s?[AP]M$/i, '')} ${meridiem(b)}`
    : `${a.toUpperCase()} – ${b.toUpperCase()}`
}

/// One row per day across the page, today first, whether or not it has events —
/// an empty day is information too.
function renderAgenda() {
  agendaEl.innerHTML = ''
  if (agendaLoading) {
    const row = el('div', 'ag-row')
    row.appendChild(el('div', 'ag-what', 'Checking your calendar…'))
    agendaEl.appendChild(row)
    return
  }

  const today = startOfDay(new Date())
  const from = new Date(today)
  from.setDate(from.getDate() + agendaPage * AGENDA_PAGE_DAYS)
  const to = new Date(from)
  to.setDate(to.getDate() + AGENDA_PAGE_DAYS - 1)

  rangeEl.textContent = agendaPage === 0 ? '' : fmtDayRange(from, to)
  agPrev.disabled = agendaPage === 0

  // Group this page's events by local day.
  const byDay = new Map()
  for (const raw of agenda.events) {
    const at = eventStart(raw)
    if (Number.isNaN(at.getTime())) continue
    const key = dayKey(at)
    if (key < dayKey(from) || key > dayKey(to)) continue
    if (!byDay.has(key)) byDay.set(key, [])
    byDay.get(key).push({ ...raw, at })
  }

  if (!agenda.authorized) {
    agendaEl.appendChild(permissionRow(today))
    return
  }

  for (let i = 0; i < AGENDA_PAGE_DAYS; i++) {
    const day = new Date(from)
    day.setDate(day.getDate() + i)
    const events = (byDay.get(dayKey(day)) || []).sort((a, b) => a.at - b.at)
    // Days beyond today with nothing on them would pad the card with blanks.
    if (!events.length && dayKey(day) !== dayKey(today)) continue
    agendaEl.appendChild(dayRow(day, events, dayKey(day) === dayKey(today)))
  }

  if (!agendaEl.children.length) {
    const row = el('div', 'ag-row')
    row.appendChild(el('div', 'ag-what', el('div', 'quiet', 'Nothing scheduled this week.')))
    agendaEl.appendChild(row)
  }
}

function dayRow(day, events, isToday) {
  const row = el('div', 'ag-row' + (isToday ? ' today' : ''))

  const when = el('div', 'ag-when')
  when.appendChild(el('span', 'd', String(day.getDate())))
  const stack = el('span', 'm', day.toLocaleDateString(undefined, { month: 'long' }))
  stack.appendChild(el('span', 'wd', day.toLocaleDateString(undefined, { weekday: 'short' })))
  when.appendChild(stack)
  if (isToday) when.appendChild(el('span', 'dot'))
  row.appendChild(when)

  const what = el('div', 'ag-what')
  if (!events.length) {
    what.appendChild(el('div', 'quiet', "No events today — start a note whenever you're ready."))
  } else {
    for (const ev of events) {
      const item = el('button', 'ag-event')
      item.append(el('span', 't', ev.summary), el('span', 'at', fmtSpan(ev)))
      if (ev.calendar) item.title = ev.calendar
      // Clicking an event titles a draft; recording stays a deliberate act.
      item.addEventListener('click', () => {
        showDraft()
        titleEl.textContent = ev.summary
        queueSave()
      })
      what.appendChild(item)
    }
  }
  row.appendChild(what)
  return row
}

/// The row that stands in for the whole card when macOS hasn't granted access.
function permissionRow(today) {
  const row = el('div', 'ag-row today')

  const when = el('div', 'ag-when')
  when.appendChild(el('span', 'd', String(today.getDate())))
  const stack = el('span', 'm', today.toLocaleDateString(undefined, { month: 'long' }))
  stack.appendChild(el('span', 'wd', today.toLocaleDateString(undefined, { weekday: 'short' })))
  when.append(stack, el('span', 'dot'))

  const what = el('div', 'ag-what')
  what.appendChild(
    el('div', 'quiet', agenda.denied
      ? "Calendar access is off, so Oatmeal can't see your day."
      : "Let Oatmeal read this Mac's calendar to see your day.")
  )
  const action = el('button', 'ag-setup', agenda.denied ? 'Open System Settings' : 'Allow calendar access')
  action.addEventListener('click', async () => {
    if (agenda.denied) {
      invoke('open_calendar_settings').catch(() => {})
      return
    }
    try {
      await invoke('calendar_request_access')
      await loadAgenda()
      renderAgenda()
    } catch { /* the Settings tab reports the detail */ }
  })
  what.appendChild(action)

  row.append(when, what)
  return row
}

/// Pull the window EventKit can give us. Permission state rides along, so the
/// card can offer the prompt itself.
async function loadAgenda() {
  try {
    agenda = await invoke('list_events', { days: AGENDA_DAYS })
  } catch (e) {
    agenda = { authorized: false, denied: false, events: [] }
  }
  agendaLoading = false
  if (viewDash.classList.contains('on')) renderAgenda()
}

agPrev.addEventListener('click', () => {
  if (agendaPage === 0) return
  agendaPage -= 1
  renderAgenda()
})

agNext.addEventListener('click', () => {
  agendaPage += 1
  renderAgenda()
})

const PAGE_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M6 3h7l5 5v13H6z" /><path d="M13 3v5h5" /></svg>'

const SNIPPET_LABELS = { title: 'Title', notes: 'Notes', transcript: 'Transcript' }

/// A snippet as text nodes, with each occurrence of `query` wrapped in `<mark>`.
/// Built node by node deliberately: a snippet is a slice of somebody's
/// transcript, and `innerHTML` would run whatever markup happened to be in it.
/// `indexOf`, not a regex, so a query full of `.*` matches literally.
function highlighted(text, query) {
  const frag = document.createDocumentFragment()
  const hay = text.toLowerCase()
  const needle = query.toLowerCase()
  // A lowercase form of a different length (exotic scripts) would slide every
  // index; plain text beats a misplaced highlight.
  if (!needle || hay.length !== text.length || needle.length !== query.length) {
    frag.appendChild(document.createTextNode(text))
    return frag
  }
  let from = 0
  for (let at = hay.indexOf(needle); at !== -1; at = hay.indexOf(needle, from)) {
    if (at > from) frag.appendChild(document.createTextNode(text.slice(from, at)))
    const mark = document.createElement('mark')
    mark.textContent = text.slice(at, at + needle.length)
    frag.appendChild(mark)
    from = at + needle.length
  }
  frag.appendChild(document.createTextNode(text.slice(from)))
  return frag
}

/// The matching excerpts under a search result, each labelled with where it
/// came from.
function snippetLines(snippets, query) {
  const wrap = el('div', 'snips')
  for (const s of snippets) {
    const line = el('div', 'snip')
    line.appendChild(el('span', 'src', SNIPPET_LABELS[s.source] || s.source))
    const body = el('span', 'x')
    body.appendChild(highlighted(s.text, query))
    line.append(body)
    wrap.appendChild(line)
  }
  return wrap
}

function renderNotesList() {
  const list = visibleMeetings()
  notesListEl.innerHTML = ''
  if (!list.length) {
    notesListEl.appendChild(
      el('div', 'notes-empty', filter ? 'No notes match that search.' : 'Your meeting notes will appear here')
    )
    return
  }
  for (const m of list) {
    const row = el('button', 'note-row' + (m.has_notes ? ' done' : ''))
    const ic = el('span', 'ic')
    ic.innerHTML = PAGE_ICON
    const txt = el('span')
    txt.append(
      el('div', 't', m.title),
      el('div', 's', [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)].filter(Boolean).join(' · '))
    )
    // While a search is on, the list becomes results: same rows, plus the text
    // that matched. No new view and no router change — this is still the list.
    const snippets = filter && searchHits ? searchHits.get(m.id) : null
    if (snippets && snippets.length) {
      row.classList.add('result')
      txt.appendChild(snippetLines(snippets, filter))
    }
    row.append(ic, txt)
    // A recording Oatmeal died in the middle of is stranded until somebody asks
    // for it to be transcribed — so ask here, where the meeting is listed.
    if ((m.pending_segments || []).length && !recording) {
      const fin = el('button', 'row-act', 'Finish transcribing')
      fin.title = 'Transcribe the audio this recording never got written up'
      fin.addEventListener('click', (e) => { e.stopPropagation(); finishMeeting(m.id) })
      row.append(fin)
    }
    row.addEventListener('click', () => openNote(m.id))
    notesListEl.appendChild(row)
  }
}

// ── sidebar collapse ─────────────────────────────────────────────────────────

const RAIL_KEY = 'oatmeal.railCollapsed'

function setRail(collapsed) {
  document.body.classList.toggle('rail-collapsed', collapsed)
  localStorage.setItem(RAIL_KEY, collapsed ? '1' : '0')
  railToggle.title = collapsed ? 'Show sidebar (⌘\\)' : 'Hide sidebar (⌘\\)'
}

railToggle.addEventListener('click', () => setRail(!document.body.classList.contains('rail-collapsed')))

document.addEventListener('keydown', (e) => {
  if (!(e.metaKey || e.ctrlKey)) return
  if (e.key === '\\') {
    e.preventDefault()
    setRail(!document.body.classList.contains('rail-collapsed'))
  } else if (e.key === 'k') {
    e.preventDefault()
    setRail(false)
    searchEl.focus()
    searchEl.select()
  }
})

// ── settings ─────────────────────────────────────────────────────────────────

function note(el, msg, tone = '') {
  el.textContent = msg
  el.classList.remove('err', 'ok')
  if (tone) el.classList.add(tone)
}

async function loadSettings() {
  try {
    const s = await invoke('get_settings')
    displayNameEl.value = s.displayName || ''
    displayName = s.displayName || ''
    renderGreeting()
    // The recorder reads the language from localStorage (both windows do); the
    // copy in the config is what survives a reinstall.
    languageEl.value = getLang() || s.language || ''
    followupStyleEl.value = s.followupStyle || 'brief'
    followupCustomEl.value = s.followupCustom || ''
    if (s.chunkSeconds) localStorage.setItem(CHUNK_KEY, String(s.chunkSeconds))
    chunkSecondsEl.value = String(chunkSeconds())
    renderFollowupStyle()
  } catch (e) {
    note(settingsNote, String(e), 'err')
  }
  try {
    modelPathEl.textContent = await invoke('default_model_path')
  } catch { /* the path is informational */ }
  refreshCalendar()
}

async function loadHomework() {
  try {
    homework = await invoke('list_homework')
  } catch {
    homework = []
  }
  renderHomework()
}

function renderHomework() {
  const open = homework.filter((item) => !item.done).length
  hwCountEl.textContent = String(open)
  hwCountEl.hidden = open === 0

  hwListEl.innerHTML = ''
  if (!homework.length) {
    hwListEl.appendChild(el('div', 'notes-empty', 'No homework yet — add something above.'))
    return
  }
  for (const item of homework) {
    const row = el('div', 'hw-row' + (item.done ? ' done' : ''))
    const check = document.createElement('input')
    check.type = 'checkbox'
    check.checked = item.done
    check.addEventListener('change', () => toggleHomework(item.id, check.checked))

    const txt = el('span', 'txt')
    txt.append(
      el('div', 't', item.title),
      el('div', 's', [fmtDueDate(item.due_date), item.note].filter(Boolean).join(' · '))
    )

    const del = el('button', 'del', '×')
    del.title = 'Delete'
    del.addEventListener('click', () => deleteHomeworkItem(item.id))

    row.append(check, txt, del)
    hwListEl.appendChild(row)
  }
}

async function toggleHomework(id, done) {
  try {
    await invoke('set_homework_done', { id, done })
    await loadHomework()
  } catch (e) {
    note(hwStatusEl, String(e), 'err')
  }
}

async function deleteHomeworkItem(id) {
  try {
    await invoke('delete_homework', { id })
    await loadHomework()
  } catch (e) {
    note(hwStatusEl, String(e), 'err')
  }
}

hwAddBtn.addEventListener('click', async () => {
  const title = hwTitleEl.value.trim()
  const dueDate = hwDatePicker.getValue()
  if (!title) {
    note(hwStatusEl, 'Give it a title.', 'err')
    return
  }
  if (!dueDate) {
    note(hwStatusEl, 'Pick a due date.', 'err')
    return
  }
  try {
    await invoke('add_homework', { title, note: hwNoteInputEl.value.trim(), dueDate })
    hwTitleEl.value = ''
    hwNoteInputEl.value = ''
    hwDatePicker.setValue(null)
    note(hwStatusEl, 'Added.', 'ok')
    await loadHomework()
  } catch (e) {
    note(hwStatusEl, String(e), 'err')
  }
})

/// The custom instruction box is only meaningful for the custom style. An
/// unknown value (a config from a newer build) leaves the select showing
/// nothing, so fall back rather than present an empty control.
function renderFollowupStyle() {
  if (!followupStyleEl.value) followupStyleEl.value = 'brief'
  followupCustomField.hidden = followupStyleEl.value !== 'custom'
}

followupStyleEl.addEventListener('change', renderFollowupStyle)

$('saveSettings').addEventListener('click', async () => {
  note(settingsNote, 'Saving…')
  try {
    const saved = await invoke('save_settings', {
      displayName: displayNameEl.value.trim(),
      language: languageEl.value.trim(),
      followupStyle: followupStyleEl.value,
      followupCustom: followupCustomEl.value.trim(),
      chunkSeconds: Number(chunkSecondsEl.value) || DEFAULT_CHUNK_SECS,
    })
    localStorage.setItem(CHUNK_KEY, String(saved.chunkSeconds || DEFAULT_CHUNK_SECS))
    setLang(languageEl.value.trim())
    displayName = saved.displayName || ''
    renderGreeting()
    note(settingsNote, 'Saved.', 'ok')
  } catch (e) {
    note(settingsNote, String(e), 'err')
  }
})

/// Calendar access is a macOS permission, not an account. Three states worth
/// showing: granted, not asked yet, and refused — the last one has to send people
/// to System Settings, because macOS will not prompt a second time.
function renderCalendar(state) {
  const { authorized, denied } = state
  acctEl.classList.toggle('linked', authorized)
  acctLabel.textContent = authorized
    ? 'Reading this Mac’s calendars'
    : denied
      ? 'Calendar access is turned off for Oatmeal'
      : 'Not allowed yet'
  $('calAllow').hidden = authorized
  $('calAllow').textContent = denied ? 'Try again' : 'Allow calendar access'
  $('calSettings').hidden = authorized
  $('calRecheck').hidden = !authorized
  if (authorized) note(calNote, '')
  else if (denied) note(calNote, 'Turn Oatmeal on under Privacy & Security → Calendars, then come back.')
}

async function refreshCalendar() {
  try {
    renderCalendar({ authorized: await invoke('calendar_authorized'), denied: agenda.denied })
  } catch (e) {
    note(calNote, String(e), 'err')
  }
}

$('calAllow').addEventListener('click', async () => {
  const button = $('calAllow')
  button.disabled = true
  note(calNote, 'Waiting for the macOS prompt…')
  try {
    const granted = await invoke('calendar_request_access')
    await loadAgenda()
    renderCalendar({ authorized: granted, denied: !granted })
    if (granted) note(calNote, 'Done — your schedule is on the Home tab.', 'ok')
  } catch (e) {
    note(calNote, String(e), 'err')
  } finally {
    button.disabled = false
  }
})

$('calSettings').addEventListener('click', () => {
  invoke('open_calendar_settings').catch((e) => note(calNote, String(e), 'err'))
})

/// Coming back from System Settings, the app has to look again — macOS doesn't
/// tell it the switch moved.
$('calRecheck').addEventListener('click', async () => {
  note(calNote, 'Checking…')
  await loadAgenda()
  renderCalendar({ authorized: agenda.authorized, denied: agenda.denied })
  if (agenda.authorized) note(calNote, 'Reading your calendars now.', 'ok')
})

// ── boot ─────────────────────────────────────────────────────────────────────

async function boot() {
  applyTheme()
  try {
    const saved = await invoke('get_settings')
    displayName = saved.displayName || ''
  } catch { /* first run, or no config yet */ }
  renderGreeting()
  setRail(localStorage.getItem(RAIL_KEY) === '1')
  sortModeEl.value = sortMode
  renderSuggestions()
  refreshHide()
  refreshModelChip()
  await loadMeetings()
  await loadFolders()
  await loadHomework()
  showHome()
  loadAgenda()
  setInterval(loadAgenda, AGENDA_REFRESH_MS)
  invoke('app_version')
    .then((v) => { brandVersionEl.textContent = `v${v}` })
    .catch(() => {})
  // Not awaited: a slow network must not hold up the window, and the gate can
  // arrive a moment after the UI does.
  refreshUpdate()
  setInterval(refreshUpdate, UPDATE_RECHECK_MS)

  setIntroVisible(!localStorage.getItem(INTRO_KEY))
  try {
    const saved = JSON.parse(localStorage.getItem(DRAFT_KEY) || '{}')
    if (saved.title) titleEl.textContent = saved.title
    if (saved.body) notesEl.textContent = saved.body
  } catch { /* corrupt draft — start clean */ }

  try {
    if (await invoke('is_session_active')) {
      recording = true
      startTimer(Number(await invoke('session_elapsed_ms').catch(() => 0)) || 0)
      toStopButton()
      setStatus('Recording in progress…')
      liveLines = await invoke('live_lines')
      emit(EVENTS.session, { active: true })
      showTranscriptWindow(true)
    }
  } catch { /* ignore */ }

  if (recording) showDraft()
}

boot()

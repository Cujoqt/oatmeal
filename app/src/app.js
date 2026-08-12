// Oatmeal — native recorder UI.
//
// Talks to the Rust commands: session start/stop (mic + system audio lanes,
// on-device Whisper), live transcription events, the meeting library, the local
// language model behind notes and recaps, and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core
const { listen, emit } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)

import { EVENTS, getLang, setLang } from '/shared.js'
import { createDatePicker } from '/datepicker.js'

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
const navHome = $('navHome')
const navSettings = $('navSettings')
const navHomework = $('navHomework')
const viewHomework = $('viewHomework')
const hwTitleEl = $('hwTitle')
const hwNoteInputEl = $('hwNoteInput')
const hwAddBtn = $('hwAdd')
const hwStatusEl = $('hwStatus')
const hwListEl = $('hwList')
const railToggle = $('railToggle')
const viewDash = $('viewDash')
const viewSettings = $('viewSettings')
const agendaEl = $('agenda')
const rangeEl = $('agRange')
const agPrev = $('agPrev')
const agNext = $('agNext')
const notesListEl = $('notesList')
const newNoteBtn = $('newNote')
const displayNameEl = $('displayName')
const greetingEl = $('greeting')
const chipWhoEl = $('chipWho')
const calNote = $('calNote')
const languageEl = $('language')
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
const chipExport = $('chipExport')
const chipFollowup = $('chipFollowup')
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
const versionLabel = $('versionLabel')
const updateCheckBtn = $('updateCheck')
const updateOpenBtn = $('updateOpen')

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
let searchTimer = null
let searchSeq = 0
let folders = []
let currentFolder = null
let openId = null
let noteTab = 'notes'
let liveLines = []
let saveTimer = null
let hintTimer = null
let sessionDir = ''
/// Element a streamed chat answer is being written into, or null between asks.
let streamingAnswer = null
let homework = []

const hwDatePicker = createDatePicker($('hwDatePicker'))

function setStatus(msg, isErr = false) {
  statusEl.textContent = msg
  statusEl.classList.toggle('err', isErr)
}

// ── tiny markdown renderer ───────────────────────────────────────────────────
//
// The model writes Markdown; only headings, bullets, bold and paragraphs are
// worth supporting. Everything goes through textContent, so model output can
// never inject markup.

function renderMarkdown(md, target) {
  target.innerHTML = ''
  let list = null

  const inline = (el, text) => {
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

  for (const raw of md.split('\n')) {
    const line = raw.trim()
    if (!line) { list = null; continue }

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
  const s = Math.floor(ms / 1000)
  const m = Math.floor(s / 60)
  return `${String(m).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`
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

async function stopRecording() {
  busy = true
  btn.disabled = true
  stopTimer()
  document.body.classList.remove('recording')
  setStatus('Transcribing on-device — this can take a moment…')

  emit(EVENTS.state, { state: 'finishing', message: 'Writing the final transcript…' })

  let landedId = null
  try {
    await invoke('save_notes', draft()).catch(() => {})
    const res = await invoke('stop_session', { modelPath: '', language: getLang() })
    landedId = (res.dir || '').split('/').pop()
    setStatus('Done. Notes saved locally.')
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    recording = false
    liveLines = []
    titleEl.textContent = ''
    notesEl.textContent = ''
    localStorage.removeItem(DRAFT_KEY)
    emit(EVENTS.session, { active: false })
    toRecordButton()
    busy = false
    await loadMeetings()
    // Land in the note that was just recorded — that's the thing worth reading.
    if (landedId && meetings.some((m) => m.id === landedId)) openNote(landedId)
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
    setStatus(await invoke('ask_meeting', { id: '', question }))
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

async function refreshUpdate({ manual = false } = {}) {
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
    showGate(
      'Update Oatmeal to edit these notes',
      `<p>The notes in this folder were written by a newer version of Oatmeal
       (format ${data.storedVersion}, this build reads ${data.dataVersion}). Nothing
       has been changed — editing is turned off so an older build can't rewrite
       them into a shape it understands.</p>
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

async function openDownload() {
  const url = downloadLink()
  if (!url) return
  try {
    await invoke('open_update_download', { url })
  } catch (e) {
    note(versionLabel, String(e))
  }
}

updateGetBtn.addEventListener('click', openDownload)
gateGetBtn.addEventListener('click', openDownload)
updateOpenBtn.addEventListener('click', openDownload)
updateCheckBtn.addEventListener('click', () => refreshUpdate({ manual: true }))
updateLaterBtn.addEventListener('click', () => {
  if (update?.latest) localStorage.setItem(UPDATE_SNOOZE_KEY, update.latest)
  updateStrip.classList.remove('show')
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
  return currentFolder ? list.filter((m) => m.folder === currentFolder) : list
}

/// Runs `filter` against transcript and notes content too, not just titles.
/// Debounced so a full scan doesn't happen on every keystroke.
async function runSearch() {
  const q = filter
  const seq = ++searchSeq
  let results
  try {
    results = await invoke('search_meetings', { query: q })
  } catch {
    return
  }
  if (seq !== searchSeq || q !== filter) return
  searchResults = results
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

    item.append(dot, txt)
    item.addEventListener('click', () => openNote(m.id))
    item.draggable = true
    item.addEventListener('dragstart', (e) => {
      e.dataTransfer.effectAllowed = 'move'
      e.dataTransfer.setData('text/plain', m.id)
    })
    sideList.appendChild(item)
  }
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
    const del = el('button', 'del', '×')
    del.title = `Delete "${f.name}"`
    del.addEventListener('click', (e) => { e.stopPropagation(); deleteFolder(f.name) })
    row.append(name, count, del)

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
  renderSidebar()
  renderSuggestions()

  const m = currentMeeting()
  if (!m) return
  noteTitle.value = m.title
  chipWhen.textContent = [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)].filter(Boolean).join(' · ')
  renderTemplateChips()
  setTab('notes')
}

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
    for (const s of segs) {
      const row = document.createElement('div')
      row.className = 'seg-line'
      const b = document.createElement('b')
      b.textContent = s.at
      const t = document.createElement('span')
      t.textContent = s.text
      row.append(b, t)
      noteBody.appendChild(row)
    }
  } catch (e) {
    noteBody.innerHTML = ''
    const p = document.createElement('p')
    p.className = 'placeholder'
    p.textContent = String(e)
    noteBody.appendChild(p)
  }
}

async function renderNotes(force = false, regenerate = false) {
  const m = currentMeeting()
  if (!m) return

  if (!m.transcribed) {
    noteBody.innerHTML = '<p class="placeholder">Nothing to summarize — this recording has no transcript.</p>'
    return
  }

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
    renderSidebar()
    renderNotesList()
    // The user may have opened a different meeting while this was generating.
    if (openId !== asked || noteTab !== 'notes') return
    renderMarkdown(md, noteBody)
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

/// Add a prompt and its pending answer to the list, run `generate` against the
/// local model with the chat controls disabled, and stream the reply into the
/// answer. Returns the answer element, or null if the model failed.
async function runChat(prompt, pending, generate) {
  const qa = document.createElement('div')
  qa.className = 'qa'
  const q = document.createElement('div')
  q.className = 'q'
  q.textContent = prompt
  const a = document.createElement('div')
  a.className = 'a thinking'
  a.textContent = pending
  qa.append(q, a)
  answersEl.appendChild(qa)
  qa.scrollIntoView({ behavior: 'smooth', block: 'end' })

  askSend.disabled = true
  chipFollowup.disabled = true
  streamingAnswer = a
  setModelChip('busy', pending)
  try {
    // The returned string is authoritative: it also covers any event that was
    // dropped while the window was busy.
    a.textContent = await generate()
    return a
  } catch (e) {
    a.textContent = String(e)
    return null
  } finally {
    a.classList.remove('thinking')
    streamingAnswer = null
    askSend.disabled = false
    chipFollowup.disabled = false
    refreshModelChip()
  }
}

chipFollowup.addEventListener('click', draftFollowup)

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
    row.append(ic, txt)
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

$('saveSettings').addEventListener('click', async () => {
  note(settingsNote, 'Saving…')
  try {
    const saved = await invoke('save_settings', {
      displayName: displayNameEl.value.trim(),
      language: languageEl.value.trim(),
    })
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
  renderSuggestions()
  refreshHide()
  refreshModelChip()
  await loadMeetings()
  await loadFolders()
  showHome()
  loadAgenda()
  setInterval(loadAgenda, AGENDA_REFRESH_MS)
  // Not awaited: a slow network must not hold up the window, and the gate can
  // arrive a moment after the UI does.
  refreshUpdate()

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

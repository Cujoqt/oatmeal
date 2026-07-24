// Oatmeal — native recorder UI.
//
// Talks to the Rust commands: session start/stop (mic + system audio lanes,
// on-device Whisper), live transcription events, the meeting library, the local
// language model behind notes and recaps, and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core
const { listen, emit } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)

import { EVENTS, getLang } from '/shared.js'

const btn = $('btn')
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
const navHome = $('navHome')
const modelChip = $('modelChip')
const modelTxt = $('modelTxt')
const viewHome = $('viewHome')
const viewNote = $('viewNote')
const noteTitle = $('noteTitle')
const noteBody = $('noteBody')
const chipWhen = $('chipWhen')
const chipDelete = $('chipDelete')
const tabNotes = $('tabNotes')
const tabRaw = $('tabRaw')
const answersEl = $('answers')
const suggestEl = $('suggest')
const askInput = $('askInput')
const askSend = $('askSend')

const DRAFT_KEY = 'oatmeal.draft'
const INTRO_KEY = 'oatmeal.introSeen'
const SUGGESTIONS = ['What did I miss?', 'What are the action items?', 'Summarize the decisions', 'What should I follow up on?']

let recording = false
let busy = false
let tick = null
let startedAt = 0
let meetings = []
let filter = ''
let openId = null
let noteTab = 'notes'
let liveLines = []
let saveTimer = null
let hintTimer = null
let sessionDir = ''

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

function startTimer() {
  startedAt = Date.now()
  timerEl.textContent = '00:00'
  tick = setInterval(() => { timerEl.textContent = fmtElapsed(Date.now() - startedAt) }, 500)
}

function stopTimer() { clearInterval(tick); tick = null }

function toRecordButton() {
  document.body.classList.remove('recording')
  btn.title = 'Start recording'
  btn.disabled = false
  titleEl.setAttribute('data-placeholder', 'New note')
}

function toStopButton() {
  document.body.classList.add('recording')
  btn.title = 'Stop recording'
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
  showHome()
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

btn.addEventListener('click', () => {
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
  try {
    // An empty id means "the meeting happening right now".
    setStatus(await invoke('ask_meeting', { id: '', question }))
  } catch (e) {
    setStatus(String(e), true)
  }
}

// ── odds and ends ────────────────────────────────────────────────────────────

revealBtn.addEventListener('click', () => {
  setStatus(sessionDir ? `Saved in ${sessionDir}` : 'The folder is created when you start recording.')
})

okayBtn.addEventListener('click', () => {
  introEl.hidden = true
  localStorage.setItem(INTRO_KEY, '1')
  notesEl.focus()
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

function visibleMeetings() {
  if (!filter) return meetings
  const q = filter.toLowerCase()
  return meetings.filter((m) => m.title.toLowerCase().includes(q))
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
}

searchEl.addEventListener('input', () => { filter = searchEl.value.trim(); renderSidebar() })
navHome.addEventListener('click', showHome)

// ── views ────────────────────────────────────────────────────────────────────

function showHome() {
  openId = null
  viewNote.classList.remove('on')
  viewHome.classList.add('on')
  navHome.classList.add('on')
  renderSidebar()
}

function currentMeeting() {
  return meetings.find((m) => m.id === openId)
}

async function openNote(id) {
  openId = id
  noteTab = 'notes'
  viewHome.classList.remove('on')
  viewNote.classList.add('on')
  navHome.classList.remove('on')
  answersEl.innerHTML = ''
  renderSidebar()
  renderSuggestions()

  const m = currentMeeting()
  if (!m) return
  noteTitle.value = m.title
  chipWhen.textContent = [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)].filter(Boolean).join(' · ')
  setTab('notes')
}

function setTab(tab) {
  noteTab = tab
  tabNotes.classList.toggle('on', tab === 'notes')
  tabRaw.classList.toggle('on', tab === 'raw')
  tab === 'notes' ? renderNotes() : renderTranscript()
}

tabNotes.addEventListener('click', () => setTab('notes'))
tabRaw.addEventListener('click', () => setTab('raw'))

async function renderTranscript() {
  const m = currentMeeting()
  noteBody.innerHTML = '<p class="placeholder">Loading transcript…</p>'
  if (!m || !m.transcribed) {
    noteBody.innerHTML = '<p class="placeholder">This recording has no transcript — it was stopped before Whisper ran, or no speech was detected.</p>'
    return
  }
  try {
    const segs = await invoke('meeting_segments', { id: m.id })
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

async function renderNotes(force = false) {
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
    return
  }

  noteBody.innerHTML = '<p class="placeholder">Reading the transcript and writing it up… this takes a minute on first run.</p>'
  setModelChip('busy', 'Writing notes…')
  try {
    const md = await invoke('write_notes', { id: m.id, force: false })
    renderMarkdown(md, noteBody)
    m.has_notes = true
    renderSidebar()
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

async function ask() {
  const m = currentMeeting()
  const question = askInput.value.trim()
  if (!m || !question) return
  askInput.value = ''
  askSend.disabled = true

  const qa = document.createElement('div')
  qa.className = 'qa'
  const q = document.createElement('div')
  q.className = 'q'
  q.textContent = question
  const a = document.createElement('div')
  a.className = 'a thinking'
  a.textContent = 'Thinking…'
  qa.append(q, a)
  answersEl.appendChild(qa)
  qa.scrollIntoView({ behavior: 'smooth', block: 'end' })

  setModelChip('busy', 'Thinking…')
  try {
    const answer = await invoke('ask_meeting', { id: m.id, question })
    a.classList.remove('thinking')
    a.textContent = answer
  } catch (e) {
    a.classList.remove('thinking')
    a.textContent = String(e)
  } finally {
    askSend.disabled = false
    refreshModelChip()
  }
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

// ── boot ─────────────────────────────────────────────────────────────────────

async function boot() {
  applyTheme()
  renderSuggestions()
  refreshHide()
  refreshModelChip()
  await loadMeetings()

  introEl.hidden = Boolean(localStorage.getItem(INTRO_KEY))
  try {
    const saved = JSON.parse(localStorage.getItem(DRAFT_KEY) || '{}')
    if (saved.title) titleEl.textContent = saved.title
    if (saved.body) notesEl.textContent = saved.body
  } catch { /* corrupt draft — start clean */ }

  try {
    if (await invoke('is_session_active')) {
      recording = true
      startTimer()
      toStopButton()
      setStatus('Recording in progress…')
      liveLines = await invoke('live_lines')
      emit(EVENTS.session, { active: true })
      showTranscriptWindow(true)
    }
  } catch { /* ignore */ }

  if (introEl.hidden && !recording) titleEl.focus()
}

boot()

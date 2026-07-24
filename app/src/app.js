// Oatmeal — native recorder UI.
//
// Talks to the Rust commands: session start/stop (mic + system audio lanes,
// on-device Whisper), live transcription events, the meeting library, the local
// language model behind notes and recaps, and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)

const btn = $('btn')
const btnIcon = $('btnIcon')
const titleEl = $('title')
const timerEl = $('timer')
const statusEl = $('status')
const headlineEl = $('headline')
const eyebrowEl = $('eyebrow')
const themeBtn = $('theme')
const themeIcon = $('themeIcon')
const hideEl = $('hide')
const hideLabel = $('hideLabel')
const statCountEl = $('statCount')
const statHoursEl = $('statHours')
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
const livedock = $('livedock')
const liveBody = $('liveBody')
const liveAnswer = $('liveAnswer')
const liveAsk = $('liveAsk')
const liveSend = $('liveSend')
const dockMin = $('dockMin')

const IDLE_HEADLINE = 'What are we talking about today?'
const MIC_ICON =
  '<rect x="9" y="3" width="6" height="11" rx="3" /><path d="M6 11a6 6 0 0 0 12 0M12 17v3" />'
const STOP_ICON = '<rect x="7" y="7" width="10" height="10" rx="2.5" />'
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
  btnIcon.innerHTML = MIC_ICON
  btn.title = 'Record'
  btn.disabled = false
}

function toStopButton() {
  document.body.classList.add('recording')
  btnIcon.innerHTML = STOP_ICON
  btn.title = 'Stop'
  btn.disabled = false
}

function renderGreeting() {
  const now = new Date()
  const day = now.toLocaleDateString(undefined, { weekday: 'long' })
  const h = now.getHours()
  eyebrowEl.textContent = `${day} ${h < 12 ? 'morning' : h < 17 ? 'afternoon' : 'evening'}`
}

async function startRecording() {
  busy = true
  btn.disabled = true
  showHome()
  liveLines = []
  liveBody.innerHTML = '<div class="waiting">Listening… text appears a few seconds behind the room.</div>'
  liveAnswer.textContent = ''

  try {
    setStatus('Preparing the transcription model (first run downloads it once)…')
    await invoke('ensure_model')

    setStatus('Starting capture…')
    await invoke('start_session', { title: titleEl.value.trim() })

    recording = true
    startTimer()
    toStopButton()
    titleEl.disabled = true
    headlineEl.textContent = titleEl.value.trim() || 'Listening…'
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

  let landedId = null
  try {
    const res = await invoke('stop_session', { modelPath: '', language: 'en' })
    landedId = (res.dir || '').split('/').pop()
    setStatus('Done. Notes saved locally.')
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    recording = false
    titleEl.disabled = false
    titleEl.value = ''
    headlineEl.textContent = IDLE_HEADLINE
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

listen('oatmeal://live-line', (event) => {
  const line = event.payload
  if (!line || !line.text) return
  liveLines.push(line)

  const waiting = liveBody.querySelector('.waiting')
  if (waiting) waiting.remove()

  const div = document.createElement('div')
  div.className = 'l'
  div.textContent = line.text
  liveBody.appendChild(div)
  liveBody.scrollTop = liveBody.scrollHeight
})

dockMin.addEventListener('click', () => livedock.classList.toggle('min'))

liveSend.addEventListener('click', () => askLive())
liveAsk.addEventListener('keydown', (e) => { if (e.key === 'Enter') askLive() })

async function askLive() {
  const question = liveAsk.value.trim()
  if (!question) return
  if (!liveLines.length) {
    liveAnswer.textContent = 'Nothing has been transcribed yet.'
    return
  }
  liveAsk.value = ''
  liveSend.disabled = true
  liveAnswer.textContent = 'Thinking…'
  try {
    // An empty id means "the meeting happening right now".
    liveAnswer.textContent = await invoke('ask_meeting', { id: '', question })
  } catch (e) {
    liveAnswer.textContent = String(e)
  } finally {
    liveSend.disabled = false
  }
}

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

function renderStats() {
  const weekAgo = Date.now() - 7 * 86400000
  statCountEl.textContent = String(meetings.filter((m) => new Date(m.started_at).getTime() >= weekAgo).length)
  const secs = meetings.filter((m) => m.transcribed).reduce((sum, m) => sum + m.duration_secs, 0)
  statHoursEl.textContent = secs >= 3600 ? `${(secs / 3600).toFixed(1)}h` : secs > 0 ? `${Math.round(secs / 60)}m` : '0m'
}

async function loadMeetings() {
  try {
    meetings = await invoke('list_meetings')
  } catch {
    meetings = []
  }
  renderSidebar()
  renderStats()
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
  renderGreeting()
  renderSuggestions()
  refreshHide()
  refreshModelChip()
  await loadMeetings()

  try {
    if (await invoke('is_session_active')) {
      recording = true
      startTimer()
      toStopButton()
      titleEl.disabled = true
      headlineEl.textContent = 'Listening…'
      setStatus('Recording in progress…')
      liveLines = await invoke('live_lines')
      if (liveLines.length) {
        liveBody.innerHTML = ''
        for (const l of liveLines) {
          const div = document.createElement('div')
          div.className = 'l'
          div.textContent = l.text
          liveBody.appendChild(div)
        }
      }
    }
  } catch { /* ignore */ }
}

boot()

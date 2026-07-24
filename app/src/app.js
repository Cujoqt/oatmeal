// Oatmeal — native recorder UI.
//
// Talks to the Rust commands: session start/stop (which drive the mic + system
// audio lanes and on-device Whisper), the meeting library, and the screen-share
// hide toggle.

const { invoke } = window.__TAURI__.core

const btn = document.getElementById('btn')
const btnIcon = document.getElementById('btnIcon')
const titleEl = document.getElementById('title')
const timerEl = document.getElementById('timer')
const statusEl = document.getElementById('status')
const headlineEl = document.getElementById('headline')
const eyebrowEl = document.getElementById('eyebrow')
const resultEl = document.getElementById('result')
const savedEl = document.getElementById('saved')
const transcriptEl = document.getElementById('transcript')
const themeBtn = document.getElementById('theme')
const themeIcon = document.getElementById('themeIcon')
const hideEl = document.getElementById('hide')
const hideLabel = document.getElementById('hideLabel')
const statCountEl = document.getElementById('statCount')
const statHoursEl = document.getElementById('statHours')
const recentBody = document.getElementById('recentBody')
const viewAllBtn = document.getElementById('viewAll')

const IDLE_HEADLINE = 'What are we talking about today?'
const RECENT_PREVIEW = 2

const MIC_ICON =
  '<rect x="9" y="3" width="6" height="11" rx="3" /><path d="M6 11a6 6 0 0 0 12 0M12 17v3" />'
const STOP_ICON = '<rect x="7" y="7" width="10" height="10" rx="2.5" />'

let recording = false
let busy = false
let tick = null
let startedAt = 0
let meetings = []
let showingAll = false

function setStatus(msg, isErr = false) {
  statusEl.textContent = msg
  statusEl.classList.toggle('err', isErr)
}

function fmtElapsed(ms) {
  const s = Math.floor(ms / 1000)
  const m = Math.floor(s / 60)
  return `${String(m).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`
}

function startTimer() {
  startedAt = Date.now()
  timerEl.textContent = '00:00'
  tick = setInterval(() => {
    timerEl.textContent = fmtElapsed(Date.now() - startedAt)
  }, 500)
}

function stopTimer() {
  clearInterval(tick)
  tick = null
}

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

// ── greeting ─────────────────────────────────────────────────────────────────

function renderGreeting() {
  const now = new Date()
  const day = now.toLocaleDateString(undefined, { weekday: 'long' })
  const h = now.getHours()
  const partOfDay = h < 12 ? 'morning' : h < 17 ? 'afternoon' : 'evening'
  eyebrowEl.textContent = `${day} ${partOfDay}`
}

// ── recording flow ───────────────────────────────────────────────────────────

async function startRecording() {
  busy = true
  btn.disabled = true
  document.body.classList.remove('showing-result')
  transcriptEl.innerHTML = ''
  savedEl.textContent = ''

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

  try {
    const res = await invoke('stop_session', { modelPath: '', language: 'en' })
    renderResult(res)
    setStatus('Done. Notes saved locally.')
  } catch (e) {
    setStatus(String(e), true)
  } finally {
    recording = false
    titleEl.disabled = false
    headlineEl.textContent = IDLE_HEADLINE
    toRecordButton()
    busy = false
    loadMeetings()
  }
}

function renderResult(res) {
  savedEl.textContent = `saved → ${res.transcript_path}`
  transcriptEl.innerHTML = ''
  const segs = (res.segments || []).filter((s) => s.text.trim())
  if (!segs.length) {
    transcriptEl.innerHTML = '<div class="seg">(no speech detected)</div>'
  } else {
    for (const s of segs) {
      const div = document.createElement('div')
      div.className = 'seg'
      const stamp = document.createElement('b')
      stamp.textContent = `[${fmtCs(s.start_cs)}]`
      div.append(stamp, document.createTextNode(s.text.trim()))
      transcriptEl.appendChild(div)
    }
  }
  document.body.classList.add('showing-result')
}

function fmtCs(cs) {
  const total = Math.floor(cs / 100)
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`
}

btn.addEventListener('click', () => {
  if (busy) return
  if (recording) stopRecording()
  else startRecording()
})

// Space starts/stops — unless the user is typing the meeting title.
document.addEventListener('keydown', (e) => {
  if (e.code !== 'Space' || e.repeat) return
  if (e.target === titleEl || e.target.tagName === 'INPUT') return
  e.preventDefault()
  if (busy) return
  if (recording) stopRecording()
  else startRecording()
})

// ── meeting library ──────────────────────────────────────────────────────────

function fmtDuration(secs) {
  if (!secs) return null
  if (secs < 60) return `${secs} sec`
  return `${Math.round(secs / 60)} min`
}

function fmtWhen(date) {
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate())
  const days = Math.round((startOfDay(new Date()) - startOfDay(date)) / 86400000)
  if (days === 0) return 'Today'
  if (days === 1) return 'Yesterday'
  if (days < 7) return date.toLocaleDateString(undefined, { weekday: 'long' })
  return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

const PENCIL_ICON =
  '<path d="M4 20h4L20 8a2.4 2.4 0 0 0-3.4-3.4L4.6 16.6 4 20Z" />'
const TRASH_ICON =
  '<path d="M4 7h16M9.5 7V5.2A1.2 1.2 0 0 1 10.7 4h2.6a1.2 1.2 0 0 1 1.2 1.2V7M6.5 7l.8 12a1.6 1.6 0 0 0 1.6 1.5h6.2a1.6 1.6 0 0 0 1.6-1.5l.8-12" />'

function iconButton(path, label, className = '') {
  const b = document.createElement('button')
  b.className = className
  b.title = label
  b.setAttribute('aria-label', label)
  b.innerHTML =
    '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">' +
    path +
    '</svg>'
  return b
}

function meetingRow(m) {
  const row = document.createElement('div')
  row.className = 'mrow'

  const ico = document.createElement('div')
  ico.className = 'mico'
  ico.innerHTML =
    '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round">' +
    MIC_ICON +
    '</svg>'

  const meta = document.createElement('div')
  meta.className = 'meta'
  const name = document.createElement('div')
  name.className = 'name'
  name.textContent = m.title
  const sub = document.createElement('div')
  sub.className = 'sub'
  sub.textContent = [fmtWhen(new Date(m.started_at)), fmtDuration(m.duration_secs)]
    .filter(Boolean)
    .join(' · ')
  meta.append(name, sub)

  const tag = document.createElement('span')
  tag.className = m.transcribed ? 'tag done' : 'tag audio'
  tag.textContent = m.transcribed ? 'Summarized' : 'Audio only'

  const acts = document.createElement('div')
  acts.className = 'acts'
  const renameBtn = iconButton(PENCIL_ICON, 'Rename')
  const deleteBtn = iconButton(TRASH_ICON, 'Move to Trash', 'danger')
  acts.append(renameBtn, deleteBtn)

  renameBtn.addEventListener('click', () => beginRename(row, m, name))
  deleteBtn.addEventListener('click', () => confirmDelete(row, m, tag, acts))

  row.append(ico, meta, tag, acts)
  row.title = m.dir
  return row
}

// ── rename ───────────────────────────────────────────────────────────────────

function beginRename(row, m, nameEl) {
  const input = document.createElement('input')
  input.className = 'rename'
  input.value = m.title
  nameEl.replaceWith(input)
  input.focus()
  input.select()

  let settled = false
  const revert = () => {
    if (settled) return
    settled = true
    input.replaceWith(nameEl)
  }

  const commit = async () => {
    if (settled) return
    const title = input.value.trim()
    if (!title || title === m.title) return revert()
    settled = true
    input.disabled = true
    try {
      await invoke('rename_meeting', { id: m.id, title })
      m.title = title
      nameEl.textContent = title
      input.replaceWith(nameEl)
    } catch (e) {
      settled = false
      input.disabled = false
      setStatus(String(e), true)
    }
  }

  input.addEventListener('keydown', (e) => {
    e.stopPropagation() // don't let Space reach the record shortcut
    if (e.key === 'Enter') commit()
    else if (e.key === 'Escape') revert()
  })
  input.addEventListener('blur', commit)
}

// ── delete ───────────────────────────────────────────────────────────────────
//
// Two steps, in the row itself: the tag and actions are swapped for an explicit
// "Move to Trash?" prompt. The recording goes to the Trash rather than being
// unlinked, so a wrong click is recoverable from Finder.

function confirmDelete(row, m, tag, acts) {
  const prompt = document.createElement('div')
  prompt.className = 'confirm'

  const q = document.createElement('span')
  q.className = 'q'
  q.textContent = 'Move to Trash?'

  const cancel = document.createElement('button')
  cancel.textContent = 'Cancel'
  const yes = document.createElement('button')
  yes.className = 'yes'
  yes.textContent = 'Move'

  prompt.append(q, cancel, yes)
  tag.style.display = 'none'
  acts.replaceWith(prompt)
  yes.focus()

  const dismiss = () => {
    tag.style.display = ''
    prompt.replaceWith(acts)
  }
  cancel.addEventListener('click', dismiss)

  yes.addEventListener('click', async () => {
    yes.disabled = true
    cancel.disabled = true
    try {
      await invoke('delete_meeting', { id: m.id })
      setStatus(`Moved “${m.title}” to the Trash.`)
      loadMeetings()
    } catch (e) {
      setStatus(String(e), true)
      dismiss()
    }
  })
}

function renderMeetings() {
  recentBody.innerHTML = ''
  if (!meetings.length) {
    recentBody.innerHTML = '<div class="empty">No meetings yet — your first recording lands here.</div>'
    viewAllBtn.style.visibility = 'hidden'
    return
  }
  viewAllBtn.style.visibility = meetings.length > RECENT_PREVIEW ? 'visible' : 'hidden'
  viewAllBtn.textContent = showingAll ? 'Show less' : 'View all'
  const shown = showingAll ? meetings : meetings.slice(0, RECENT_PREVIEW)
  for (const m of shown) recentBody.appendChild(meetingRow(m))
}

function renderStats() {
  const weekAgo = Date.now() - 7 * 86400000
  const thisWeek = meetings.filter((m) => new Date(m.started_at).getTime() >= weekAgo)
  statCountEl.textContent = String(thisWeek.length)

  // "Notes written for you" — time Oatmeal actually turned into a transcript.
  const secs = meetings
    .filter((m) => m.transcribed)
    .reduce((sum, m) => sum + m.duration_secs, 0)
  statHoursEl.textContent =
    secs >= 3600 ? `${(secs / 3600).toFixed(1)}h` : secs > 0 ? `${Math.round(secs / 60)}m` : '0m'
}

async function loadMeetings() {
  try {
    meetings = await invoke('list_meetings')
  } catch (e) {
    meetings = []
  }
  renderMeetings()
  renderStats()
}

viewAllBtn.addEventListener('click', () => {
  showingAll = !showingAll
  renderMeetings()
})

// ── theme ────────────────────────────────────────────────────────────────────
//
// Three states, cycled by the title-bar button: system → light → dark. "System"
// leaves `data-theme` off the root so the CSS media query decides; the other two
// pin it. The choice survives restarts.

const THEMES = ['system', 'light', 'dark']
const SUN =
  '<circle cx="12" cy="12" r="4.2" /><path d="M12 2.6v2.2M12 19.2v2.2M4.2 12H2M22 12h-2.2M6.3 6.3 4.8 4.8M19.2 19.2l-1.5-1.5M17.7 6.3l1.5-1.5M4.8 19.2l1.5-1.5" />'
const MOON = '<path d="M20 14.2A8.2 8.2 0 0 1 9.8 4a8.4 8.4 0 1 0 10.2 10.2Z" />'
const AUTO = '<circle cx="12" cy="12" r="8.4" /><path d="M12 3.6v16.8" /><path d="M12 3.6a8.4 8.4 0 0 1 0 16.8" fill="currentColor" stroke="none" />'

let theme = localStorage.getItem('oatmeal.theme') || 'system'

function applyTheme() {
  if (theme === 'system') document.documentElement.removeAttribute('data-theme')
  else document.documentElement.setAttribute('data-theme', theme)

  themeIcon.innerHTML = theme === 'light' ? SUN : theme === 'dark' ? MOON : AUTO
  themeBtn.title =
    theme === 'system' ? 'Theme: follows macOS' : `Theme: ${theme}`
}

themeBtn.addEventListener('click', () => {
  theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]
  localStorage.setItem('oatmeal.theme', theme)
  applyTheme()
})

// ── hide-from-capture toggle ─────────────────────────────────────────────────

async function refreshHide() {
  try {
    const hidden = await invoke('is_hidden_from_capture')
    hideEl.classList.toggle('on', hidden)
    hideLabel.textContent = hidden ? 'hidden from shares' : 'visible to shares'
  } catch (e) {
    /* non-macOS / not ready */
  }
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

// ── boot: sync with any in-progress session ──────────────────────────────────

async function boot() {
  applyTheme()
  renderGreeting()
  refreshHide()
  loadMeetings()
  try {
    if (await invoke('is_session_active')) {
      recording = true
      startTimer()
      toStopButton()
      titleEl.disabled = true
      headlineEl.textContent = 'Listening…'
      setStatus('Recording in progress…')
    }
  } catch (e) {
    /* ignore */
  }
}

boot()

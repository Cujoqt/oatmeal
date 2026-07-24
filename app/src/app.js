// Oatmeal — native recorder UI (M5).
//
// Talks to the Rust commands: session start/stop (which drive the mic + system
// audio lanes and on-device Whisper), and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core

const btn = document.getElementById('btn')
const heroEl = document.getElementById('hero')
const stageEl = document.getElementById('stage')
const eyebrowEl = document.getElementById('eyebrow')
const titleEl = document.getElementById('title')
const timerEl = document.getElementById('timer')
const statusEl = document.getElementById('status')
const resultEl = document.getElementById('result')
const savedEl = document.getElementById('saved')
const transcriptEl = document.getElementById('transcript')
const hideEl = document.getElementById('hide')
const hideLabel = document.getElementById('hideLabel')

let recording = false
let busy = false
let tick = null
let startedAt = 0

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
  timerEl.classList.add('on')
  timerEl.textContent = '00:00'
  tick = setInterval(() => {
    timerEl.textContent = fmtElapsed(Date.now() - startedAt)
  }, 500)
}

function stopTimer() {
  clearInterval(tick)
  tick = null
  timerEl.classList.remove('on')
}

const MIC_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.8" stroke-linecap="round"><rect x="9" y="3" width="6" height="11" rx="3"/><path d="M6 11a6 6 0 0 0 12 0M12 17v3"/></svg>'
const STOP_ICON = '<svg viewBox="0 0 24 24"><rect x="7" y="7" width="10" height="10" rx="2.5" fill="#fff"/></svg>'

function toRecordButton() {
  stageEl.classList.remove('live')
  stageEl.classList.add('idle')
  heroEl.classList.remove('live')
  btn.innerHTML = MIC_ICON
  btn.setAttribute('aria-label', 'Record')
  btn.disabled = false
}

function toStopButton() {
  stageEl.classList.remove('idle')
  stageEl.classList.add('live')
  heroEl.classList.add('live')
  btn.innerHTML = STOP_ICON
  btn.setAttribute('aria-label', 'Stop recording')
  btn.disabled = false
}

// ── recording flow ───────────────────────────────────────────────────────────

async function startRecording() {
  busy = true
  btn.disabled = true
  resultEl.classList.remove('open')
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
    setStatus('Recording — your mic and the other side of the call, locally.')
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
    toRecordButton()
    busy = false
  }
}

function renderResult(res) {
  savedEl.innerHTML = `saved → <b>${escapeHtml(res.transcript_path)}</b>`
  transcriptEl.innerHTML = ''
  const segs = (res.segments || []).filter((s) => s.text.trim())
  if (!segs.length) {
    transcriptEl.innerHTML = '<div class="seg">(no speech detected)</div>'
  } else {
    for (const s of segs) {
      const div = document.createElement('div')
      div.className = 'seg'
      div.innerHTML = `<b>[${fmtCs(s.start_cs)}]</b> ${escapeHtml(s.text.trim())}`
      transcriptEl.appendChild(div)
    }
  }
  resultEl.classList.add('open')
}

function fmtCs(cs) {
  const total = Math.floor(cs / 100)
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]))
}

function toggleRecording() {
  if (busy) return
  if (recording) stopRecording()
  else startRecording()
}

btn.addEventListener('click', toggleRecording)

// Space toggles record — unless typing in the meeting title.
document.addEventListener('keydown', (e) => {
  if (e.code !== 'Space' && e.key !== ' ') return
  if (e.target === titleEl || e.target.tagName === 'INPUT' || e.target.isContentEditable) return
  e.preventDefault()
  toggleRecording()
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

function setGreeting() {
  const h = new Date().getHours()
  const part = h < 12 ? 'Good morning' : h < 18 ? 'Good afternoon' : 'Good evening'
  const day = new Date().toLocaleDateString(undefined, { weekday: 'long' })
  eyebrowEl.textContent = `${day} · ${part}`
}

async function boot() {
  setGreeting()
  refreshHide()
  try {
    if (await invoke('is_session_active')) {
      recording = true
      startTimer()
      toStopButton()
      titleEl.disabled = true
      setStatus('Recording in progress…')
    }
  } catch (e) {
    /* ignore */
  }
}

boot()

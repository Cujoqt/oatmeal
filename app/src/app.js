// Oatmeal — native recorder UI (M5).
//
// Talks to the Rust commands: session start/stop (which drive the mic + system
// audio lanes and on-device Whisper), and the screen-share hide toggle.

const { invoke } = window.__TAURI__.core

const btn = document.getElementById('btn')
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

function toRecordButton() {
  btn.classList.remove('stop')
  btn.innerHTML = '<span class="dot"></span>Record'
  btn.disabled = false
}

function toStopButton() {
  btn.classList.add('stop')
  btn.innerHTML = '<span class="dot"></span>Stop'
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

btn.addEventListener('click', () => {
  if (busy) return
  if (recording) stopRecording()
  else startRecording()
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

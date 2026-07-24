// Oatmeal — the note window.
//
// This is the surface you look at during a meeting: a title, your own notes, and
// the recording dock. The live transcript lives in its own floating window
// (transcript.html) so it can sit over a call without covering your notes.
//
// Rust owns everything real — capture, streaming transcription, files. This file
// only drives those commands and keeps the two windows in sync via Tauri events.

import { EVENTS, LANG_KEY, getLang } from '/shared.js'

const { invoke } = window.__TAURI__.core
const { listen, emit } = window.__TAURI__.event

const el = (id) => document.getElementById(id)
const titleEl = el('title')
const notesEl = el('notes')
const saveHintEl = el('saveHint')
const statusEl = el('status')
const clusterEl = el('cluster')
const recBtn = el('rec')
const expandBtn = el('expand')
const revealBtn = el('reveal')
const introEl = el('intro')
const okayBtn = el('okay')
const askEl = el('ask')
const hideEl = el('hide')
const hideLabel = el('hideLabel')

const DRAFT_KEY = 'oatmeal.draft'
const INTRO_KEY = 'oatmeal.introSeen'

let recording = false
let busy = false
let sessionDir = ''
let saveTimer = null

// ── status + chrome ──────────────────────────────────────────────────────────

function setStatus(msg, isErr = false) {
  statusEl.textContent = msg
  statusEl.classList.toggle('err', isErr)
}

function setRecordingUI(on) {
  recording = on
  clusterEl.classList.toggle('live', on)
  recBtn.title = on ? 'Stop recording' : 'Start recording'
  titleEl.setAttribute('data-placeholder', on ? 'Untitled meeting' : 'New note')
}

// ── notes: debounced autosave to notes.md ────────────────────────────────────

function draft() {
  return { title: titleEl.textContent.trim(), body: notesEl.textContent }
}

function queueSave() {
  const d = draft()
  // Keep a local copy immediately so a crash or restart never loses typing.
  localStorage.setItem(DRAFT_KEY, JSON.stringify(d))

  clearTimeout(saveTimer)
  saveTimer = setTimeout(async () => {
    try {
      const path = await invoke('save_notes', d)
      // Before a meeting exists the backend holds the notes in memory and
      // returns null; there is nothing on disk to report yet.
      if (path) flashHint('Saved to notes.md')
    } catch (e) {
      flashHint(String(e))
    }
  }, 800)
}

let hintTimer = null
function flashHint(msg) {
  saveHintEl.textContent = msg
  saveHintEl.classList.add('show')
  clearTimeout(hintTimer)
  hintTimer = setTimeout(() => saveHintEl.classList.remove('show'), 1800)
}

titleEl.addEventListener('input', queueSave)
notesEl.addEventListener('input', queueSave)

// ── recording ────────────────────────────────────────────────────────────────

async function startRecording() {
  busy = true
  try {
    setStatus('Preparing the transcription model (first run downloads it once)…')
    await invoke('ensure_model')

    setStatus('Starting capture…')
    const paths = await invoke('start_session', {
      title: draft().title,
      language: getLang(),
    })
    sessionDir = paths.dir

    setRecordingUI(true)
    // Flush whatever was typed before the folder existed.
    await invoke('save_notes', draft()).catch(() => {})
    await showTranscript(true)
    emit(EVENTS.session, { active: true, title: draft().title })
    setStatus('Recording — mic and the other side of the call, transcribing live.')
  } catch (e) {
    setStatus(String(e), true)
    setRecordingUI(false)
  } finally {
    busy = false
  }
}

async function stopRecording() {
  busy = true
  setStatus('Finishing the transcript on-device — this can take a moment…')
  emit(EVENTS.state, { state: 'finishing', message: 'Writing the final transcript…' })

  try {
    // Save one last time before the folder stops being the active session.
    await invoke('save_notes', draft()).catch(() => {})
    const res = await invoke('stop_session', { modelPath: '', language: getLang() })
    emit(EVENTS.final, res)
    setStatus(`Saved to ${res.dir}`)
  } catch (e) {
    setStatus(String(e), true)
    emit(EVENTS.state, { state: 'error', message: String(e) })
  } finally {
    setRecordingUI(false)
    emit(EVENTS.session, { active: false, title: draft().title })
    busy = false
  }
}

function toggleRecording() {
  if (busy) return
  if (recording) stopRecording()
  else startRecording()
}

recBtn.addEventListener('click', toggleRecording)
// The transcript window has the same stop button; it asks us to run the flow so
// there is only ever one owner of the session state.
listen(EVENTS.toggleRecord, toggleRecording)

// ── transcript window ────────────────────────────────────────────────────────

async function showTranscript(visible) {
  try {
    await invoke('set_transcript_window_visible', { visible })
  } catch (e) {
    setStatus(String(e), true)
  }
}

expandBtn.addEventListener('click', async () => {
  const visible = await invoke('is_transcript_window_visible').catch(() => false)
  showTranscript(!visible)
})
listen(EVENTS.hideTranscript, () => showTranscript(false))

// ── odds and ends ────────────────────────────────────────────────────────────

revealBtn.addEventListener('click', () => {
  setStatus(sessionDir ? `Recording folder: ${sessionDir}` : 'The folder is created when you start recording.')
})

okayBtn.addEventListener('click', () => {
  introEl.hidden = true
  localStorage.setItem(INTRO_KEY, '1')
  notesEl.focus()
})

askEl.addEventListener('keydown', (e) => {
  if (e.key !== 'Enter') return
  e.preventDefault()
  // Not wired to a model yet — say so rather than silently swallowing input.
  setStatus('Ask is not connected yet — your notes and transcript are saved locally.')
})

hideEl.addEventListener('click', async () => {
  try {
    const hidden = await invoke('is_hidden_from_capture')
    await invoke('set_hidden_from_capture', { hidden: !hidden })
    refreshHide()
  } catch (e) {
    setStatus(String(e), true)
  }
})

async function refreshHide() {
  try {
    const hidden = await invoke('is_hidden_from_capture')
    hideEl.classList.toggle('on', hidden)
    hideLabel.textContent = hidden ? 'hidden from shares' : 'visible to shares'
  } catch (e) {
    /* non-macOS / not ready */
  }
}

// ── boot ─────────────────────────────────────────────────────────────────────

function today() {
  return new Date().toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' })
}

async function boot() {
  el('day').textContent = 'Today'
  el('day').title = today()
  if (!localStorage.getItem(LANG_KEY)) localStorage.setItem(LANG_KEY, 'en')

  try {
    const saved = JSON.parse(localStorage.getItem(DRAFT_KEY) || '{}')
    if (saved.title) titleEl.textContent = saved.title
    if (saved.body) notesEl.textContent = saved.body
  } catch (e) {
    /* corrupt draft — start clean */
  }

  introEl.hidden = !!localStorage.getItem(INTRO_KEY)
  refreshHide()

  try {
    if (await invoke('is_session_active')) {
      setRecordingUI(true)
      setStatus('Recording in progress…')
      showTranscript(true)
    }
  } catch (e) {
    /* backend not ready */
  }

  if (introEl.hidden) titleEl.focus()
}

boot()

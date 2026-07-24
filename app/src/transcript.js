// Oatmeal — the floating transcript window.
//
// Listens to the streaming events from `live.rs` and appends a line per chunk.
// Chunks are final when they arrive (the worker never rewrites one), so this is
// pure append — no flicker, no reflow of what you already read.
//
// When the meeting stops, the batch pass produces a better transcript of the
// whole mix; that arrives as one `ui-final` event and replaces the live lines.

import { EVENTS, LANG_KEY, getLang, setLang, fmtCs, escapeHtml } from '/shared.js'

const { invoke } = window.__TAURI__.core
const { listen, emit } = window.__TAURI__.event

const el = (id) => document.getElementById(id)
const scrollEl = el('scroll')
const linesEl = el('lines')
const emptyEl = el('empty')
const kickerEl = el('kicker')
const promptEl = el('prompt')
const noteEl = el('note')
const clusterEl = el('cluster')
const recBtn = el('rec')
const searchBtn = el('searchBtn')
const searchEl = el('search')
const langEl = el('lang')

let recording = false
/// Every line currently shown, so search can re-render without re-fetching.
let lines = []

// ── rendering ────────────────────────────────────────────────────────────────

function setNote(msg, isErr = false) {
  noteEl.textContent = msg || ''
  noteEl.classList.toggle('err', isErr)
}

function setRecordingUI(on) {
  recording = on
  clusterEl.classList.toggle('live', on)
  recBtn.title = on ? 'Stop recording' : 'Start recording'
  kickerEl.textContent = on ? 'Transcript on…' : 'Transcript off…'
  promptEl.textContent = on ? 'Try saying “Hello Oatmeal”' : 'Press the button to start recording'
}

function refresh() {
  const has = lines.length > 0
  emptyEl.classList.toggle('hidden', has)
  linesEl.classList.toggle('hidden', !has)
}

/// True when the user is already parked at the bottom — only then do we follow
/// new lines, so scrolling back to re-read something isn't yanked away.
function atBottom() {
  return scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight < 60
}

function addLine(seg) {
  const text = (seg.text || '').trim()
  if (!text) return
  const stick = atBottom()

  lines.push({ start_cs: seg.start_cs, text })
  const div = document.createElement('div')
  div.className = 'line'
  div.innerHTML = `<time>${fmtCs(seg.start_cs)}</time><span>${escapeHtml(text)}</span>`
  linesEl.appendChild(div)
  refresh()
  applyFilter()

  if (stick) scrollEl.scrollTop = scrollEl.scrollHeight
}

function replaceAll(segments) {
  lines = (segments || [])
    .map((s) => ({ start_cs: s.start_cs, text: (s.text || '').trim() }))
    .filter((s) => s.text)
  linesEl.innerHTML = ''
  for (const s of lines) {
    const div = document.createElement('div')
    div.className = 'line'
    div.innerHTML = `<time>${fmtCs(s.start_cs)}</time><span>${escapeHtml(s.text)}</span>`
    linesEl.appendChild(div)
  }
  refresh()
  applyFilter()
  scrollEl.scrollTop = scrollEl.scrollHeight
}

// ── backend events ───────────────────────────────────────────────────────────

listen(EVENTS.segment, (e) => addLine(e.payload))

listen(EVENTS.state, (e) => {
  const { state, message } = e.payload || {}
  if (state === 'listening') setNote('')
  else setNote(message || '', state === 'error')
})

listen(EVENTS.session, (e) => {
  const { active } = e.payload || {}
  if (active && !recording) {
    // A fresh meeting starts with a clean sheet.
    lines = []
    linesEl.innerHTML = ''
    refresh()
    setNote('')
  }
  setRecordingUI(!!active)
})

listen(EVENTS.final, (e) => {
  replaceAll(e.payload?.segments)
  setNote('Final transcript saved.')
})

// ── controls ─────────────────────────────────────────────────────────────────

// The note window owns the session; ask it to flip.
recBtn.addEventListener('click', () => emit(EVENTS.toggleRecord, {}))

for (const id of ['minimize', 'collapse']) {
  el(id).addEventListener('click', () => emit(EVENTS.hideTranscript, {}))
}

el('copy').addEventListener('click', async () => {
  const text = lines.map((l) => `[${fmtCs(l.start_cs)}] ${l.text}`).join('\n')
  if (!text) return setNote('Nothing to copy yet.')
  try {
    await navigator.clipboard.writeText(text)
    setNote('Transcript copied.')
  } catch (err) {
    setNote(String(err), true)
  }
})

el('settings').addEventListener('click', () => {
  setNote(`Model: on-device Whisper · language: ${getLang() || 'auto-detect'}`)
})

el('learn').addEventListener('click', (e) => {
  e.preventDefault()
  setNote('Recording others may require their consent — check the rules where you are.')
})

// ── search ───────────────────────────────────────────────────────────────────

searchBtn.addEventListener('click', () => {
  searchEl.classList.toggle('open')
  if (searchEl.classList.contains('open')) searchEl.focus()
  else {
    searchEl.value = ''
    applyFilter()
  }
})

searchEl.addEventListener('input', applyFilter)
searchEl.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    searchEl.value = ''
    searchEl.classList.remove('open')
    applyFilter()
  }
})

/// Hide non-matching lines and highlight the hits in the rest.
function applyFilter() {
  const q = searchEl.value.trim().toLowerCase()
  const nodes = linesEl.children
  let hits = 0
  for (let i = 0; i < nodes.length; i++) {
    const span = nodes[i].lastElementChild
    const text = lines[i]?.text ?? span.textContent
    if (!q) {
      nodes[i].classList.remove('hide')
      span.textContent = text
      continue
    }
    const match = text.toLowerCase().includes(q)
    nodes[i].classList.toggle('hide', !match)
    if (match) {
      hits++
      span.innerHTML = highlight(text, q)
    }
  }
  if (q) setNote(hits ? `${hits} matching line${hits === 1 ? '' : 's'}` : 'No matches')
  else if (noteEl.textContent.endsWith('matches') || noteEl.textContent.includes('matching line')) setNote('')
}

function highlight(text, q) {
  const lower = text.toLowerCase()
  let out = ''
  let i = 0
  while (i < text.length) {
    const at = lower.indexOf(q, i)
    if (at === -1) {
      out += escapeHtml(text.slice(i))
      break
    }
    out += escapeHtml(text.slice(i, at))
    out += `<mark>${escapeHtml(text.slice(at, at + q.length))}</mark>`
    i = at + q.length
  }
  return out
}

// ── boot ─────────────────────────────────────────────────────────────────────

async function boot() {
  if (!localStorage.getItem(LANG_KEY)) setLang('en')
  langEl.value = getLang()
  langEl.addEventListener('change', () => {
    setLang(langEl.value)
    setNote(
      recording
        ? 'Language applies to the next recording.'
        : `Transcribing in ${langEl.options[langEl.selectedIndex].text}.`,
    )
  })

  setRecordingUI(false)
  try {
    setRecordingUI(await invoke('is_session_active'))
  } catch (e) {
    /* backend not ready */
  }
  refresh()
}

boot()

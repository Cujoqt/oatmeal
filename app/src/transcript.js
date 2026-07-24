// Oatmeal — the floating transcript window.
//
// A transparent, always-on-top panel you park over a call. It renders the same
// `oatmeal://live-line` events the backend emits while recording, one line per
// decoded window. Lines are final when they arrive — the worker never rewrites
// one — so this is pure append: no flicker, no reflow of what you already read.
//
// The note window owns the session. This window only renders and asks.

import { EVENTS, LANG_KEY, getLang, setLang, fmtMs, escapeHtml } from '/shared.js'

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

function rowFor(line) {
  const div = document.createElement('div')
  div.className = 'line'
  div.innerHTML = `<time>${fmtMs(line.at_ms)}</time><span>${escapeHtml(line.text)}</span>`
  return div
}

function addLine(line) {
  const text = (line.text || '').trim()
  if (!text) return
  const stick = atBottom()

  const row = { at_ms: line.at_ms || 0, text }
  lines.push(row)
  linesEl.appendChild(rowFor(row))
  refresh()
  applyFilter()

  if (stick) scrollEl.scrollTop = scrollEl.scrollHeight
}

function replaceAll(incoming) {
  lines = (incoming || [])
    .map((l) => ({ at_ms: l.at_ms || 0, text: (l.text || '').trim() }))
    .filter((l) => l.text)
  linesEl.innerHTML = ''
  for (const l of lines) linesEl.appendChild(rowFor(l))
  refresh()
  applyFilter()
  scrollEl.scrollTop = scrollEl.scrollHeight
}

// ── events ───────────────────────────────────────────────────────────────────

listen(EVENTS.line, (e) => addLine(e.payload || {}))

listen(EVENTS.state, (e) => {
  const { state, message } = e.payload || {}
  setNote(message || '', state === 'error')
})

listen(EVENTS.session, async (e) => {
  const { active } = e.payload || {}
  if (active && !recording) {
    // A fresh meeting starts with a clean sheet.
    replaceAll([])
    setNote('')
  }
  setRecordingUI(Boolean(active))
})

// ── controls ─────────────────────────────────────────────────────────────────

// The note window owns the session; ask it to flip.
recBtn.addEventListener('click', () => emit(EVENTS.toggleRecord, {}))

for (const id of ['minimize', 'collapse']) {
  el(id).addEventListener('click', () => emit(EVENTS.hideTranscript, {}))
}

el('copy').addEventListener('click', async () => {
  const text = lines.map((l) => `[${fmtMs(l.at_ms)}] ${l.text}`).join('\n')
  if (!text) return setNote('Nothing to copy yet.')
  try {
    await navigator.clipboard.writeText(text)
    setNote('Transcript copied.')
  } catch (err) {
    setNote(String(err), true)
  }
})

el('settings').addEventListener('click', () => {
  setNote(`On-device Whisper · language: ${getLang() || 'auto-detect'}`)
})

el('learn').addEventListener('click', (e) => {
  e.preventDefault()
  setNote('Recording others may require their consent — check the rules where you are.')
})

// ── search ───────────────────────────────────────────────────────────────────

searchBtn.addEventListener('click', () => {
  searchEl.classList.toggle('open')
  if (searchEl.classList.contains('open')) {
    searchEl.focus()
  } else {
    searchEl.value = ''
    applyFilter()
  }
})

searchEl.addEventListener('input', applyFilter)
searchEl.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return
  searchEl.value = ''
  searchEl.classList.remove('open')
  applyFilter()
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
  else if (/matching line|No matches/.test(noteEl.textContent)) setNote('')
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
    // Opened mid-meeting: catch up on everything decoded so far.
    replaceAll(await invoke('live_lines'))
  } catch {
    /* backend not ready */
  }
  refresh()
}

boot()

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
const autoBtn = el('autoBtn')
const pinBtn = el('pin')
const answerEl = el('answer')
const ansQEl = el('ansQ')
const ansBodyEl = el('ansBody')

let recording = false
/// Every line currently shown, so search can re-render without re-fetching.
let lines = []

/// Rows kept in the DOM. A long meeting produced thousands, and every one of them
/// was re-examined on each new line — the panel got slower the longer you talked.
/// The authoritative transcript is the file written at stop, so trimming the top
/// of the panel costs nothing.
const MAX_ROWS = 1200

/// Whether a search is narrowing the list right now. Without this, every incoming
/// line paid for a full pass over the DOM even with an empty search box.
let filtering = false

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

/// Whether new lines keep the view on the newest one.
///
/// It is a remembered decision rather than a measurement taken as each line
/// lands: a panel that is on another Space, behind a full-screen call or hidden
/// has no usable scroll metrics, so measuring at that moment reads as "scrolled
/// up" and the panel comes back parked in the middle of the meeting. Only a real
/// scroll by the user changes it.
let follow = true

function toBottom() {
  scrollEl.scrollTop = scrollEl.scrollHeight
}

scrollEl.addEventListener('scroll', () => {
  // A hidden panel reports zeros; that is not the user scrolling away.
  if (scrollEl.clientHeight) follow = atBottom()
})

// Back from another Space, or from behind whatever was in front: catch up on
// everything that arrived while the panel was away.
window.addEventListener('focus', () => { if (follow) toBottom() })
document.addEventListener('visibilitychange', () => {
  if (follow && !document.hidden) toBottom()
})

function rowFor(line) {
  const div = document.createElement('div')
  div.className = 'line'
  div.innerHTML = `<time>${fmtMs(line.at_ms)}</time><span>${escapeHtml(line.text)}</span>`
  return div
}

function addLine(line) {
  const text = (line.text || '').trim()
  if (!text) return
  const row = { at_ms: line.at_ms || 0, text }
  lines.push(row)
  linesEl.appendChild(rowFor(row))

  // Drop the oldest rows past the ceiling, keeping `lines` and the DOM in step —
  // applyFilter() pairs them up by index.
  while (lines.length > MAX_ROWS) {
    lines.shift()
    if (linesEl.firstElementChild) linesEl.removeChild(linesEl.firstElementChild)
  }

  refresh()
  // Only the new row needs a decision, and only when a search is actually on.
  if (filtering) applyFilter()

  if (follow) toBottom()

  maybeAutoAnswer(text)
}

// ── auto-answer ────────────────────────────────────────────────────────────────
//
// When it is switched on, a line that looks like a question is handed to the
// local model, which answers it in a card below the transcript from what it
// already knows — no network, nothing written into the meeting. Detection is a
// cheap heuristic here so the model is never woken just to decide whether a line
// was even a question; the real limits (one answer at a time, a minimum gap
// between them, a length cap) live in Rust, where the recording is protected.

const AUTO_KEY = 'oatmeal.autoAnswer'

/// Mirrors `autoanswer::MIN_INTERVAL` as a client-side pre-filter, so a burst of
/// questions doesn't flash a "Thinking…" card the backend will only reject. The
/// backend gate is still the authority.
const AUTO_MIN_GAP_MS = 6000

/// First words that make a line a question even without a "?" — speech
/// recognition rarely punctuates one.
const QUESTION_STARTS = new Set([
  'what', 'why', 'how', 'when', 'where', 'who', 'whom', 'whose', 'which',
  'can', 'could', 'should', 'would', 'will', 'is', 'are', 'am', 'was', 'were',
  'do', 'does', 'did', 'has', 'have', 'had', 'may', 'might', 'shall',
])

let autoOn = false
/// True while an answer is being generated, so we don't stack a second request
/// on top of one the backend would refuse anyway.
let answering = false
/// When the last answer started, for the client-side gap check.
let lastAskAt = 0

function looksLikeQuestion(text) {
  const t = text.trim()
  if (t.length < 6) return false
  if (t.endsWith('?')) return true
  const first = t.toLowerCase().match(/^[a-z]+/)
  return first ? QUESTION_STARTS.has(first[0]) : false
}

function maybeAutoAnswer(text) {
  if (!autoOn || !recording || answering) return
  if (Date.now() - lastAskAt < AUTO_MIN_GAP_MS) return
  if (!looksLikeQuestion(text)) return
  askAuto(text)
}

async function askAuto(question) {
  answering = true
  lastAskAt = Date.now()
  ansQEl.textContent = question
  ansBodyEl.textContent = 'Thinking…'
  answerEl.classList.remove('hidden')
  if (follow) toBottom()
  try {
    // Tokens stream into the card via the liveAnswer listener; the returned
    // string is the finished answer, which also covers the case where nothing
    // streamed (e.g. an immediate refusal).
    const full = await invoke('answer_live_question', { question })
    if (full) ansBodyEl.textContent = full
  } catch (err) {
    // A rate-limit/busy refusal isn't worth showing — just retire the card if it
    // never filled. Any other error is surfaced.
    const msg = String(err)
    if (/rate limited|busy|too long/.test(msg)) {
      if (ansBodyEl.textContent === 'Thinking…') answerEl.classList.add('hidden')
    } else {
      ansBodyEl.textContent = msg
    }
  } finally {
    answering = false
  }
}

// The first token of a new answer clears the "Thinking…" placeholder; the rest
// append. textContent throughout, so model output can never inject markup.
listen(EVENTS.liveAnswer, (e) => {
  const { seq, text } = e.payload || {}
  if (!text) return
  if (seq === 1) ansBodyEl.textContent = ''
  ansBodyEl.textContent += text
  // The card grows in flow, taking height off the transcript above it — without
  // this the newest lines slide out of sight as the answer streams in.
  if (follow) toBottom()
})

el('ansClose').addEventListener('click', () => answerEl.classList.add('hidden'))

// ── pin ──────────────────────────────────────────────────────────────────────
//
// Always-on-top only holds within one Space, so swiping over to the call left
// the panel behind. Pinned, it joins every Space and may sit over a full-screen
// app; the flag is remembered so it comes back pinned next time.

const PIN_KEY = 'oatmeal.pin'
let pinned = false

function setPinUI(on) {
  pinned = on
  pinBtn.classList.toggle('on', on)
  pinBtn.setAttribute('aria-pressed', String(on))
  pinBtn.title = on ? 'Pinned over every Space (on)' : 'Pin over every Space (off)'
}

async function applyPin(on) {
  setPinUI(on)
  localStorage.setItem(PIN_KEY, on ? '1' : '0')
  try {
    await invoke('set_transcript_pinned', { pinned: on })
  } catch (err) {
    setNote(String(err), true)
  }
}

pinBtn.addEventListener('click', () => {
  applyPin(!pinned)
  setNote(pinned ? 'Pinned — the panel follows you across Spaces.' : 'Unpinned.')
})

function setAutoUI(on) {
  autoOn = on
  autoBtn.classList.toggle('on', on)
  autoBtn.setAttribute('aria-pressed', String(on))
  autoBtn.title = on ? 'Auto-answer questions (on)' : 'Auto-answer questions (off)'
}

autoBtn.addEventListener('click', () => {
  const on = !autoOn
  setAutoUI(on)
  localStorage.setItem(AUTO_KEY, on ? '1' : '0')
  if (on) {
    // Load the model now so the first real answer isn't paying the load cost.
    invoke('warm_chat_model').catch(() => {})
    setNote('Auto-answer on — spoken questions get a quick, unverified answer.')
  } else {
    answerEl.classList.add('hidden')
    setNote('Auto-answer off.')
  }
})

function replaceAll(incoming) {
  lines = (incoming || [])
    .map((l) => ({ at_ms: l.at_ms || 0, text: (l.text || '').trim() }))
    .filter((l) => l.text)
    .slice(-MAX_ROWS)
  linesEl.innerHTML = ''
  for (const l of lines) linesEl.appendChild(rowFor(l))
  refresh()
  applyFilter()
  toBottom()
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
    answerEl.classList.add('hidden')
    // Warm the model up front so the first spoken question answers fast.
    if (autoOn) invoke('warm_chat_model').catch(() => {})
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
  filtering = Boolean(q)

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

  setAutoUI(localStorage.getItem(AUTO_KEY) === '1')
  applyPin(localStorage.getItem(PIN_KEY) === '1')

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

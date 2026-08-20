// Oatmeal — the floating transcript window.
//
// A transparent, always-on-top panel you park over a call. It renders the
// `oatmeal://live-line` events the backend emits while recording. Whisper cuts a
// line every time somebody pauses, so lines are merged into paragraphs as they
// arrive — one timestamp per paragraph, a new one once the block has run past
// the chunk length. Lines are final when they arrive — the worker never rewrites
// one — so this is pure append: the newest paragraph grows, nothing above it
// moves.
//
// The note window owns the session. This window only renders and asks.

import { EVENTS, LANG_KEY, chunkSeconds, getLang, setLang, fmtMs, escapeHtml } from '/shared.js'
import { speechToLatex } from '/math.js'
import { toMathML } from '/mathml.js'

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
const mathBtn = el('mathBtn')
const pinBtn = el('pin')
const answerEl = el('answer')
const ansQEl = el('ansQ')
const ansBodyEl = el('ansBody')

let recording = false
/// Every paragraph currently shown, newest last, one per `#lines` child and in
/// the same order — `applyFilter()` pairs them up by index. Each is
/// `{ at_ms, texts: [] }`: the time it started and the lines merged into it, kept
/// so search can re-render without re-fetching.
let blocks = []

/// Paragraphs kept in the DOM. A long meeting produced thousands of rows, and
/// every one of them was re-examined on each new line — the panel got slower the
/// longer you talked. The authoritative transcript is the file written at stop,
/// so trimming the top of the panel costs nothing.
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
  const has = blocks.length > 0
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

function blockText(block) {
  return block.texts.join(' ')
}

function rowFor(block) {
  const div = document.createElement('div')
  div.className = 'line'
  div.innerHTML = `<time>${fmtMs(block.at_ms)}</time><span>${escapeHtml(blockText(block))}</span>`
  return div
}

/// Where a line goes: the open paragraph, unless that paragraph started more
/// than the chunk length ago.
function addLine(line) {
  const text = (line.text || '').trim()
  if (!text) return
  const at_ms = line.at_ms || 0
  const open = blocks[blocks.length - 1]
  let block, div

  if (open && at_ms - open.at_ms < chunkSeconds() * 1000) {
    open.texts.push(text)
    block = open
    div = linesEl.lastElementChild
    // Re-rendered as plain text; a live search paints its marks back on
    // below, and maybeTypeset() below re-derives math from the grown block —
    // any typesetting from before this line merged in must not survive
    // unchanged, or a stale equation could outlive the words it came from.
    delete div.dataset.latex
    div.lastElementChild.textContent = blockText(open)
  } else {
    block = { at_ms, texts: [text] }
    blocks.push(block)
    div = rowFor(block)
    linesEl.appendChild(div)
  }

  // Drop the oldest paragraphs past the ceiling, keeping `blocks` and the DOM in
  // step — applyFilter() pairs them up by index.
  while (blocks.length > MAX_ROWS) {
    blocks.shift()
    if (linesEl.firstElementChild) linesEl.removeChild(linesEl.firstElementChild)
  }

  refresh()
  maybeTypeset(block, div)
  // Only the newest paragraph changed, but a search still has to re-decide it.
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
  const limit = chunkSeconds() * 1000
  blocks = []
  for (const l of incoming || []) {
    const text = (l.text || '').trim()
    if (!text) continue
    const at_ms = l.at_ms || 0
    const open = blocks[blocks.length - 1]
    if (open && at_ms - open.at_ms < limit) open.texts.push(text)
    else blocks.push({ at_ms, texts: [text] })
  }
  blocks = blocks.slice(-MAX_ROWS)
  linesEl.innerHTML = ''
  for (const b of blocks) {
    const div = rowFor(b)
    linesEl.appendChild(div)
    maybeTypeset(b, div)
  }
  refresh()
  applyFilter()
  toBottom()
}

// ── live math typesetting ───────────────────────────────────────────────────
//
// When the toggle is on, a paragraph that reads as spoken mathematics is
// typeset in place. `speechToLatex` (math.js) is the free path — a pure
// phrase table, tried first, no model involved. Only when it returns null
// *and* the text contains vocabulary spoken maths actually uses does the
// line go to the gated model call `latex_from_speech`, which shares its
// single-flight, six-second-minimum rate limit with auto-answer because both
// compete with Whisper for the GPU.
//
// `looksMathy`/`mathiness` (also in math.js) are dead code and deliberately
// unused here — Dylan dropped auto-detection after evidence it fires on
// ordinary meetings and misses most branches of math. What gates the model
// call instead is much narrower: not "is this a lecture", just "does this
// line contain the kind of words spoken math uses". Its worst case is one
// wasted, rate-limited model call — not a mis-rendered transcript line.

const MATH_KEY = 'oatmeal.mathMode'
let mathOn = false

/// Mirrors AUTO_MIN_GAP_MS: a client-side pre-filter so a burst of math-y
/// lines doesn't fire a request per line the backend gate would only refuse.
/// The backend gate — shared with auto-answer — is still the authority.
const MATH_MIN_GAP_MS = 6000
/// True while a conversion is in flight, so a second line never stacks a
/// request on top of one the shared gate would reject anyway.
let mathConverting = false
let lastMathAt = 0

/// The block/div/source-text a model conversion is currently running for, so
/// the streamed tokens below land on the right row and a result that arrives
/// after the paragraph grew past what was asked about gets discarded instead
/// of stamping stale math over new words.
let mathStreamBlock = null
let mathStreamDiv = null
let mathStreamText = ''
let mathStreamBuf = ''

/// Vocabulary and symbols `speechToLatex`'s own grammar recognises — plus,
/// minus, times, equals, powers, roots, pi, theta, the integral opener, and
/// the bare operator characters. Not a lecture classifier: just whether this
/// particular line is worth the model's time after the free path passed on it.
const MATH_CUES = /\b(plus|minus|times|equals|squared|cubed|divided by|square root|to the power|raised to the power|the integral|pi|theta)\b/i

function looksLikeMathCue(text) {
  // The bare-operator branch fires on any hyphen, slash or equals — "follow-
  // up", a date — which is broader than the named-word branch above. Kept
  // anyway: ASR output carries almost no punctuation (see math.js's module
  // comment), so this rarely matches prose in practice, and a false positive
  // here only costs one wasted, rate-limited model call.
  return MATH_CUES.test(text) || /[+\-*/=^]/.test(text)
}

function setMathUI(on) {
  mathOn = on
  mathBtn.classList.toggle('on', on)
  mathBtn.setAttribute('aria-pressed', String(on))
  mathBtn.title = on ? 'Typeset spoken math (on)' : 'Typeset spoken math (off)'
}

mathBtn.addEventListener('click', () => {
  const on = !mathOn
  setMathUI(on)
  localStorage.setItem(MATH_KEY, on ? '1' : '0')
  setNote(on ? 'Math typesetting on — spoken equations render as they’re heard.' : 'Math typesetting off.')
})

/// Typeset `latex` into a block's span and record it on the block element as
/// the source of truth. applyFilter() rewrites every span's markup wholesale
/// on each search keystroke, which would otherwise destroy the MathML nodes —
/// `data-latex` is what lets it rebuild them afterward instead of losing them.
function setBlockLatex(div, latex) {
  div.dataset.latex = latex
  const span = div.lastElementChild
  span.textContent = ''
  span.appendChild(toMathML(latex))
}

/// Try to typeset a block's current text. Called whenever a block's content
/// changes — a new paragraph, another line merged into an open one, or the
/// initial catch-up render — so a paragraph that turns out to be pure spoken
/// math gets typeset regardless of when it finished. A block that doesn't
/// convert is left exactly as it already renders: plain text.
function maybeTypeset(block, div) {
  if (!mathOn) return
  const text = blockText(block)
  try {
    const latex = speechToLatex(text)
    if (latex !== null) {
      setBlockLatex(div, latex)
      return
    }
    if (looksLikeMathCue(text)) requestLatex(block, div, text)
  } catch {
    // speechToLatex and toMathML both document a never-throw contract, but
    // this runs inside addLine()'s per-line hot path — a throw here would
    // skip applyFilter(), toBottom() and maybeAutoAnswer() for the line, not
    // just lose the typesetting. Defence in depth: leave the line as plain
    // text rather than let a throw here degrade the live transcript.
    delete div.dataset.latex
    div.lastElementChild.textContent = text
  }
}

async function requestLatex(block, div, text) {
  if (mathConverting) return
  if (Date.now() - lastMathAt < MATH_MIN_GAP_MS) return
  mathConverting = true
  lastMathAt = Date.now()
  mathStreamBlock = block
  mathStreamDiv = div
  mathStreamText = text
  mathStreamBuf = ''
  try {
    // The streamed tokens paint the block live via the liveMath listener
    // below; the returned string is the finished conversion, which also
    // covers the case where nothing streamed.
    const full = await invoke('latex_from_speech', { speech: text })
    // Only commit if the block still reads exactly as it did when asked —
    // more speech may have merged into the paragraph while the model ran,
    // and stamping old math over new words would be worse than plain text —
    // and only if the toggle is still on, so a request started before the
    // user switched it off doesn't paint math into a panel told to stop.
    if (full && mathOn && div.isConnected && blockText(block) === text) setBlockLatex(div, full)
  } catch {
    // Fail soft: a gate refusal, a timeout, a model error. Nothing streamed
    // survives either — put the block back to its own current plain text
    // rather than leave a partial equation on screen.
    if (div.isConnected && blockText(block) === text) {
      delete div.dataset.latex
      div.lastElementChild.textContent = text
    }
  } finally {
    mathConverting = false
    mathStreamBlock = null
    mathStreamDiv = null
  }
}

// textContent throughout toMathML, so model output can never inject markup —
// same discipline as the liveAnswer listener above.
listen(EVENTS.liveMath, (e) => {
  const { seq, text: piece } = e.payload || {}
  if (!piece || !mathOn || !mathStreamDiv || !mathStreamDiv.isConnected) return
  if (blockText(mathStreamBlock) !== mathStreamText) return // superseded
  if (seq === 1) mathStreamBuf = ''
  mathStreamBuf += piece
  setBlockLatex(mathStreamDiv, mathStreamBuf)
})

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
  const text = blocks.map((b) => `[${fmtMs(b.at_ms)}] ${blockText(b)}`).join('\n')
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

/// Hide paragraphs that do not contain the query and highlight the hits in the
/// rest. A paragraph is the unit: the whole block is the smallest thing on
/// screen that still carries its own timestamp.
function applyFilter() {
  const q = searchEl.value.trim().toLowerCase()
  const nodes = linesEl.children
  let hits = 0
  filtering = Boolean(q)

  for (let i = 0; i < nodes.length; i++) {
    const div = nodes[i]
    const span = div.lastElementChild
    const text = blocks[i] ? blockText(blocks[i]) : span.textContent
    if (!q) {
      div.classList.remove('hide')
      span.textContent = text
    } else {
      const match = text.toLowerCase().includes(q)
      div.classList.toggle('hide', !match)
      if (match) {
        hits++
        span.innerHTML = highlight(text, q)
      }
    }
    // The rewrites above just replaced this span's markup wholesale, which
    // would silently destroy any typeset MathML node. Rebuild it from the
    // block's own recorded LaTeX — the source of truth data-latex exists for.
    if (div.dataset.latex) {
      span.textContent = ''
      span.appendChild(toMathML(div.dataset.latex))
    }
  }

  if (q) setNote(hits ? `${hits} matching paragraph${hits === 1 ? '' : 's'}` : 'No matches')
  else if (/matching paragraph|No matches/.test(noteEl.textContent)) setNote('')
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
  setMathUI(localStorage.getItem(MATH_KEY) === '1')
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

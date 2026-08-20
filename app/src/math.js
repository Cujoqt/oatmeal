// Detecting that a transcript is about mathematics.
//
// Two signals are required together, and that is the whole point. Vocabulary
// alone fires on "let's factor that into the board deck"; density alone fires
// on any meeting full of budget numbers. A lecture has both, and nothing else
// reliably does.
//
// The vocabulary signal is phrases, not single words. "Derivative", "limit",
// "solve" and "fraction" are all ordinary business English on their own — a
// derivatives desk, a credit limit, a problem to solve, a fraction of last
// year's margin. Three rounds of tuning a single-word list against reweighted
// thresholds could not separate a math lecture from a trading-desk update,
// because the ambiguity is in the words themselves, not their frequency. A
// phrase carries the syntax that disambiguates: nobody says "the derivative
// of" or "by the chain rule" about a trading book.
//
// Phrases are not automatically safe just for being multiple words, though —
// "with respect to" and "solve for" were tried here first and both turned out
// to be ordinary formal business English ("reviewed spend with respect to the
// approved budget", "asked us to solve for the gap"), and "limit as" matched
// across an unrelated boundary ("breaching the credit limit as we approach
// the end of the quarter"). Every phrase below has been checked for two
// distinct failure modes: being a genuine business idiom on its own, and
// being formable by mashing the tail of one clause into the head of an
// unrelated next one. Where a phrase only fails the second way, requiring the
// leading article ("the limit as", not "limit as") fixes it, because a
// lecturer says "the limit as" but "credit limit" is never followed by "the".
//
// A handful of single words survive contact with business English anyway
// (hypotenuse, polynomial, asymptote, ...) and still count as evidence on
// their own. "Epsilon" and "cauchy" deliberately are not on this list even
// though neither is ordinary speech either — Epsilon is a real marketing-data
// vendor name, so a meeting could plausibly mention it once. They still count
// as evidence, just only inside the specific phrases below ("for every
// epsilon", "cauchy sequence"), which nobody says about a vendor.
//
// This runs per live block and once over a finished transcript, so it is a
// string check and nothing more — the model is never woken merely to classify,
// the same reason `looksLikeQuestion` in transcript.js is a heuristic.

// Spoken-math phrases. ASR output has almost no punctuation, so these are
// matched against normalized (lowercased, punctuation-stripped) text as
// contiguous word sequences — a phrase is open-ended on the right, so
// "the derivative of" matches "the derivative of x squared" without needing
// to know what follows. Each phrase counts once per transcript, not once per
// occurrence, so repeating "square root of" ten times cannot substitute for
// actually covering more mathematical ground.
const PHRASES = [
  'the derivative of',
  'the derivatives of',
  'partial derivative',
  'second derivative',
  'third derivative',
  'take the derivative',
  'the integral of',
  'definite integral',
  'indefinite integral',
  // Requires the leading article: "credit limit as we approach" must not
  // match, and never says "the" right before "limit" in that construction.
  'the limit as',
  'approaches infinity',
  'square root of',
  'cube root of',
  // "to the power" alone was dropped — "deferred to the power players in the
  // room" is a real sentence. "to the power of" and "raised to the power" are
  // both specific enough that they don't form by coincidence.
  'to the power of',
  'raised to the power',
  'd by d',
  'plus a constant',
  'plus c',
  'chain rule',
  'product rule',
  'quotient rule',
  'power rule',
  'fundamental theorem of calculus',
  'mean value theorem',
  'taylor series',
  // Real-analysis vocabulary. "for every epsilon" and "epsilon neighborhood"
  // need the literal word "epsilon" or "cauchy" as part of a specific
  // construction — see the note on SURVIVOR_WORDS below for why those two
  // words don't get to count on their own.
  'for every epsilon',
  'epsilon neighborhood',
  'cauchy sequence',
  'continuous function',
]

/// Single words that are mathematical in a lecture and not ordinary business
/// speech — unlike "derivative" or "limit", nobody says "hypotenuse" or
/// "asymptote" about a trading book or a status meeting. These count as
/// evidence on their own, without needing a surrounding phrase.
///
/// "Epsilon" and "cauchy" are deliberately absent: both are unambiguous math
/// vocabulary, but Epsilon is also a real marketing-data vendor, so a single
/// mention in an ordinary meeting ("we're working with Epsilon on the loyalty
/// campaign") is plausible. They still count as evidence, just only inside
/// the PHRASES above ("for every epsilon", "cauchy sequence") — nobody says
/// those about a vendor.
const SURVIVOR_WORDS = new Set([
  'hypotenuse', 'hypotenuses',
  'polynomial', 'polynomials',
  'asymptote', 'asymptotes',
  'logarithm', 'logarithms', 'logarithmic',
  'coefficient', 'coefficients',
  'factorial', 'factorials',
  'sine', 'cosine',
  'theorem', 'theorems',
])

const OPERATOR = /[+\-*/=^<>]/

/// Fraction of tokens that read as mathematical notation rather than prose.
function density(text) {
  const tokens = text.split(/\s+/).filter(Boolean)
  if (!tokens.length) return 0
  const mathy = tokens.filter((t) => {
    if (/\d/.test(t) || OPERATOR.test(t)) return true
    // Single letters like x, y, f are mathematical; articles (a) and pronouns
    // (I) are not. Exclude them to avoid inflating density on ordinary prose.
    if (/^[a-z]$/i.test(t)) {
      const lower = t.toLowerCase()
      return lower !== 'a' && lower !== 'i'
    }
    return false
  }).length
  return mathy / tokens.length
}

/// Normalized text for phrase matching: lowercased, punctuation stripped to
/// spaces, collapsed, and padded so every phrase can be found as whole words
/// with a plain substring search. The padding is what stops "chain rules"
/// (plural) from matching "chain rule" — the character run is there, but not
/// followed by a word boundary.
function normalizeForPhrases(text) {
  const collapsed = text.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim()
  return ` ${collapsed} `
}

function countPhraseHits(text) {
  const normalized = normalizeForPhrases(text)
  let hits = 0
  for (const phrase of PHRASES) {
    if (normalized.includes(` ${phrase} `)) hits++
  }
  return hits
}

/// 0-1. Higher means more likely to be mathematics.
export function mathiness(text) {
  const words = text.toLowerCase().match(/[a-z]+/g) || []
  if (!words.length) return 0
  const phrases = countPhraseHits(text)
  const survivors = words.filter((w) => SURVIVOR_WORDS.has(w)).length
  // A phrase is worth more than a survivor word because it is syntactically
  // unambiguous, not just a word that happens to be rare in business speech.
  const vocabHits = phrases * 3 + survivors
  // Vocabulary is required. Without a math phrase or a survivor word, no
  // amount of density (a meeting full of budget figures) is enough — this is
  // the guard that stops "4.2 million" and "11 percent" from reading as math.
  if (vocabHits === 0) return 0
  const vocab = vocabHits / words.length
  return Math.min(1, vocab * 8 + density(text) * 0.8)
}

/// Detection threshold. Tuned so both lecture fixtures in math.test.mjs clear
/// it by a real margin and every business/finance fixture — including the
/// adversarial passages that defeated earlier vocabulary choices — scores 0,
/// because phrase matching gives them no vocabulary signal at all. If a future
/// fixture forces a change, move this — never weaken a false-positive test to
/// make a lecture test pass, because a false positive silently reshapes
/// someone's meeting notes.
const THRESHOLD = 0.35

/// Too few words to judge. A single line like "the integral of x" is exactly
/// the case where a wrong guess is most likely and least recoverable.
const MIN_WORDS = 25

export function looksMathy(text) {
  const words = text.toLowerCase().match(/[a-z]+/g) || []
  if (words.length < MIN_WORDS) return false
  return mathiness(text) >= THRESHOLD
}

// speechToLatex: a phrase-table converter from spoken mathematics to LaTeX,
// the free fast path in front of a model call (Task 9's gated
// `latex_from_speech`). Every symbol and command produced here must exist in
// SYMBOLS/FUNCTIONS/BIGOPS in mathml.js — that table is the only thing
// standing between this output and a rendered equation in the live panel, so
// emitting anything outside it prints literal backslash text instead.
//
// Scoped to exactly what the task brief names: powers (squared, cubed, "to
// the power of"), square roots, fractions ("a over b"), the four arithmetic
// operators, equals, pi and theta, the definite integral, plain numbers and
// single-letter variables. It deliberately does not reach for other Greek
// letters (alpha/beta/sigma are also ordinary English — a beta version, six
// sigma — where pi/theta aren't), function application ("f of x"), limits,
// derivatives, or roots beyond square (mathml.js's \sqrt only ever takes one
// argument — there's no safe way to render \sqrt[n]{}). Each of those either
// has no rendering path or is genuinely ambiguous from words alone, and
// returning null for them is correct, not a shortfall.
//
// Grammar: an Expression is Terms joined by binary operators; a Term is one
// or more Factors written with no operator between them (implicit
// multiplication — "3 x" -> "3x"); a Factor is a number, a single letter, pi
// or theta, or a square root, each optionally squared/cubed/raised to a
// power. Any unrecognised word, anywhere in the sentence, aborts the whole
// conversion rather than stitching a partial result around it — half a
// translation is exactly the confident-nonsense outcome this exists to avoid.

const NUMBER_WORDS = {
  zero: 0, one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7,
  eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12, thirteen: 13,
  fourteen: 14, fifteen: 15, sixteen: 16, seventeen: 17, eighteen: 18,
  nineteen: 19, twenty: 20,
}

// Tens words that combine with a following ones word (1-9) into a compound
// number: "twenty one" -> 21. Kept separate from NUMBER_WORDS because the
// combination is conditional on what follows, not a plain lookup.
const TENS_WORDS = {
  twenty: 20, thirty: 30, forty: 40, fifty: 50, sixty: 60, seventy: 70,
  eighty: 80, ninety: 90,
}

// Only the two symbols the brief names — see the module comment above for
// why the rest of the Greek alphabet is left out.
const SYMBOL_WORDS = { pi: '\\pi', theta: '\\theta' }

/// True when the word at `i` begins a bare number literal — a digit token,
/// a number word, or a tens word. Used to detect two number atoms sitting
/// next to each other with no operator between them ("two three", "twenty
/// one thirteen" past the compound), which implicit multiplication would
/// otherwise silently concatenate into a wrong digit string.
function startsBareNumber(words, i) {
  const w = words[i]
  if (w === undefined) return false
  if (/^\d+$/.test(w)) return true
  if (Object.prototype.hasOwnProperty.call(NUMBER_WORDS, w)) return true
  if (Object.prototype.hasOwnProperty.call(TENS_WORDS, w)) return true
  return false
}

/// A decimal point between digits ("3.5") would otherwise be silently
/// mangled: normalization below treats punctuation as a word separator, so
/// "3.5" tokenizes as "3" then "5", and implicit multiplication concatenates
/// them into "35" — changing the value without any signal that it happened.
/// Bailing out here is cheaper than teaching the grammar decimals.
const HAS_DECIMAL = /\d+\.\d+/

function isOperatorStart(words, i) {
  const w = words[i]
  if (w === 'plus' || w === 'minus' || w === 'times' || w === 'equals' || w === 'over') return true
  if (w === 'divided' && words[i + 1] === 'by') return true
  return false
}

/// A small recursive-descent parser over one word array, built as a factory
/// (rather than free functions closing over module state) so the integral
/// pattern below can run two independent parses — one for the bounds, one
/// for the body — over slices of the same sentence without sharing position.
function makeParser(words) {
  let i = 0

  // A `+`/`-` right after a square root's argument is genuinely ambiguous
  // scope — "the square root of x plus one" could mean sqrt(x) + 1 or
  // sqrt(x + 1), and nothing here can tell which the speaker meant. Refusing
  // it matches the same call already made for "x plus 1 over 2".
  function sqrtScopeIsAmbiguous() {
    return words[i] === 'plus' || words[i] === 'minus'
  }

  function parseFactor() {
    if (i >= words.length) return null
    if (words[i] === 'the' && words[i + 1] === 'square' && words[i + 2] === 'root' && words[i + 3] === 'of') {
      i += 4
      const inner = parseFactor()
      if (inner === null || sqrtScopeIsAmbiguous()) return null
      return `\\sqrt{${inner}}`
    }
    if (words[i] === 'square' && words[i + 1] === 'root' && words[i + 2] === 'of') {
      i += 3
      const inner = parseFactor()
      if (inner === null || sqrtScopeIsAmbiguous()) return null
      return `\\sqrt{${inner}}`
    }
    // Tens word, optionally combined with a following ones word (1-9) into
    // a compound: "twenty" -> 20, "twenty one" -> 21. Checked before the
    // plain NUMBER_WORDS lookup below so the compound wins over treating
    // "twenty" and "one" as two separate factors.
    if (Object.prototype.hasOwnProperty.call(TENS_WORDS, words[i])) {
      const tens = TENS_WORDS[words[i]]
      const ones = NUMBER_WORDS[words[i + 1]]
      if (ones !== undefined && ones >= 1 && ones <= 9) { i += 2; return String(tens + ones) }
      i += 1
      return String(tens)
    }
    const w = words[i]
    if (/^\d+$/.test(w)) { i += 1; return w }
    if (Object.prototype.hasOwnProperty.call(NUMBER_WORDS, w)) { i += 1; return String(NUMBER_WORDS[w]) }
    if (Object.prototype.hasOwnProperty.call(SYMBOL_WORDS, w)) { i += 1; return SYMBOL_WORDS[w] }
    if (/^[a-z]$/.test(w)) { i += 1; return w }
    return null
  }

  // Powers bind to the single factor just parsed, not the whole term: "3 x
  // squared" is spoken as 3 times (x squared), not (3x) squared, so the
  // suffix is checked per-factor rather than per-term.
  function parseFactorWithSuffix() {
    const base = parseFactor()
    if (base === null) return null
    if (words[i] === 'squared') { i += 1; return `${base}^2` }
    if (words[i] === 'cubed') { i += 1; return `${base}^3` }
    if (words[i] === 'to' && words[i + 1] === 'the' && words[i + 2] === 'power' && words[i + 3] === 'of') {
      i += 4
      const exp = parseFactor()
      return exp === null ? null : `${base}^${exp}`
    }
    return base
  }

  // Every factor produced above is exactly one mathml.js token (a number, a
  // single letter, a \pi/\theta command, or one \sqrt{...} unit), so a `^`
  // placed right after it binds to the whole thing with no braces needed —
  // mathml.js's own parser attaches `^` to whatever single node preceded it.
  function parseTerm() {
    const parts = []
    let prevWasNumber = false
    while (i < words.length && !isOperatorStart(words, i)) {
      const numberHere = startsBareNumber(words, i)
      // Two bare numbers back to back with no operator between them is
      // genuinely ambiguous ("two three" — one number misheard as two
      // words, or two separate numbers?) and implicit multiplication would
      // otherwise concatenate them into a wrong digit string, the same
      // mechanism the decimal guard above exists to stop. Leave the second
      // number unconsumed; the leftover-word check in speechToLatex turns
      // that into null for the whole expression rather than a guess.
      if (prevWasNumber && numberHere) break
      const start = i
      const f = parseFactorWithSuffix()
      if (f === null) { i = start; break }
      parts.push(f)
      prevWasNumber = numberHere
    }
    if (!parts.length) return null
    // Concatenate bare numbers/letters directly ("3 x" -> "3x"), but keep a
    // space around any \command factor — mathml.js's tokenizer reads a
    // backslash command as greedily as it can, so "\pi" next to "x" with no
    // space would read back as the single command "\pix".
    let out = ''
    for (let k = 0; k < parts.length; k += 1) {
      if (k > 0 && (parts[k - 1].startsWith('\\') || parts[k].startsWith('\\'))) out += ' '
      out += parts[k]
    }
    return out
  }

  function parseExpression() {
    const first = parseTerm()
    if (first === null) return null
    let result = first
    while (i < words.length) {
      if (words[i] === 'plus') { i += 1; const t = parseTerm(); if (t === null) return null; result += ` + ${t}`; continue }
      if (words[i] === 'minus') { i += 1; const t = parseTerm(); if (t === null) return null; result += ` - ${t}`; continue }
      if (words[i] === 'times') { i += 1; const t = parseTerm(); if (t === null) return null; result += ` \\times ${t}`; continue }
      if (words[i] === 'divided' && words[i + 1] === 'by') { i += 2; const t = parseTerm(); if (t === null) return null; result += ` \\div ${t}`; continue }
      if (words[i] === 'equals') { i += 1; const t = parseTerm(); if (t === null) return null; result += ` = ${t}`; continue }
      if (words[i] === 'over') {
        // "a over b" is a fraction of the two adjacent terms, not a
        // general-precedence operator — "x plus 1 over 2" is genuinely
        // ambiguous (over the whole sum, or just the 1?), so a fraction is
        // only produced when "over" is the sentence's only operator.
        // Anything more falls through to the model.
        if (result !== first) return null
        i += 1
        const denom = parseTerm()
        if (denom === null || i < words.length) return null
        return `\\frac{${first}}{${denom}}`
      }
      return null
    }
    return result
  }

  return {
    parseFactor,
    parseExpression,
    pos: () => i,
    setPos: (n) => { i = n },
  }
}

/// "the integral from A to B of EXPR d X" — the one multi-clause shape the
/// brief names explicitly. Handled as its own pattern rather than folded
/// into the Expression grammar above because its pieces (bounds, body, the
/// trailing differential) don't compose the way arithmetic terms do.
function tryIntegral(words) {
  let i = 0
  if (words[i] === 'the') i += 1
  if (words[i] !== 'integral' || words[i + 1] !== 'from') return null
  i += 2

  const bounds = makeParser(words)
  bounds.setPos(i)
  const lower = bounds.parseFactor()
  if (lower === null || words[bounds.pos()] !== 'to') return null
  bounds.setPos(bounds.pos() + 1)
  const upper = bounds.parseFactor()
  if (upper === null || words[bounds.pos()] !== 'of') return null
  i = bounds.pos() + 1

  const last = words.length - 1
  if (last - i < 1 || words[last - 1] !== 'd' || !/^[a-z]$/.test(words[last])) return null
  const exprWords = words.slice(i, last - 1)
  if (!exprWords.length) return null

  const body = makeParser(exprWords)
  const expr = body.parseExpression()
  if (expr === null || body.pos() < exprWords.length) return null

  return `\\int_{${lower}}^{${upper}} ${expr} \\, d${words[last]}`
}

/// Converts spoken mathematics to LaTeX, or returns null when the phrase
/// table doesn't cleanly cover the line. Null is the correct answer for
/// anything outside the scope documented in the module comment above — it
/// hands the line to `latex_from_speech` instead of guessing.
export function speechToLatex(text) {
  if (typeof text !== 'string' || !text.trim()) return null
  if (HAS_DECIMAL.test(text)) return null

  const words = text.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim().split(/\s+/).filter(Boolean)
  if (!words.length) return null

  const integral = tryIntegral(words)
  if (integral !== null) return integral

  const parser = makeParser(words)
  const result = parser.parseExpression()
  if (result === null || parser.pos() < words.length) return null
  return result
}

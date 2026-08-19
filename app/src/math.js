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
// of" or "with respect to" or "by the chain rule" about a trading book. A
// handful of single words survive contact with business English anyway
// (hypotenuse, polynomial, asymptote, ...) and still count as evidence.
//
// This runs per live block and once over a finished transcript, so it is a
// string check and nothing more — the model is never woken merely to classify,
// the same reason `looksLikeQuestion` in transcript.js is a heuristic.

// Spoken-math phrases. ASR output has almost no punctuation, so these are
// matched against normalized (lowercased, punctuation-stripped) text as
// contiguous word sequences — a phrase is open-ended on the right, so
// "derivative of" matches "derivative of x squared" without needing to know
// what follows. Each phrase counts once per transcript, not once per
// occurrence, so repeating "solve for x" ten times cannot substitute for
// actually covering more mathematical ground.
const PHRASES = [
  'derivative of',
  'derivatives of',
  'partial derivative',
  'second derivative',
  'third derivative',
  'take the derivative',
  'differentiate with respect to',
  'integral of',
  'integral from',
  'integrate from',
  'definite integral',
  'indefinite integral',
  'limit as',
  'approaches infinity',
  'with respect to',
  'solve for',
  'square root of',
  'cube root of',
  'to the power of',
  'to the power',
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
]

/// Single words that are mathematical in a lecture and not ordinary business
/// speech — unlike "derivative" or "limit", nobody says "hypotenuse" or
/// "asymptote" about a trading book or a status meeting. These count as
/// evidence on their own, without needing a surrounding phrase.
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
/// with a plain substring search.
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
/// it by a real margin and every business/finance fixture — including the two
/// adversarial passages that defeated the single-word vocabulary — scores 0,
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

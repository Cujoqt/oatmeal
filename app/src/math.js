// Detecting that a transcript is about mathematics.
//
// Two signals are required together, and that is the whole point. Vocabulary
// alone fires on "let's factor that into the board deck"; density alone fires
// on any meeting full of budget numbers. A lecture has both, and nothing else
// reliably does.
//
// This runs per live block and once over a finished transcript, so it is a
// string check and nothing more — the model is never woken merely to classify,
// the same reason `looksLikeQuestion` in transcript.js is a heuristic.

const VOCAB = new Set([
  'derivative', 'derivatives', 'integral', 'integrate', 'integrals', 'limit',
  'theorem', 'equation', 'equations', 'matrix', 'vector', 'sine', 'cosine',
  'tangent', 'logarithm', 'log', 'polynomial', 'coefficient', 'hypotenuse',
  'proof', 'squared', 'cubed', 'exponent', 'denominator', 'numerator',
  'fraction', 'slope', 'asymptote', 'factorial', 'summation', 'sigma',
  'differentiate', 'substitute', 'solve', 'simplify', 'graph',
])

// Words that are mathematical in a lecture and ordinary everywhere else.
// They only count when a word from VOCAB is already present, so "times",
// "over" and "factor" cannot carry a detection by themselves.
const WEAK_VOCAB = new Set([
  'times', 'plus', 'minus', 'over', 'equals', 'divided', 'factor', 'variable',
])

const OPERATOR = /[+\-*/=^<>]/

// Fraction of tokens that read as mathematical notation rather than prose.
function density(text) {
  const tokens = text.split(/\s+/).filter(Boolean)
  if (!tokens.length) return 0
  const mathy = tokens.filter(
    (t) => /\d/.test(t) || OPERATOR.test(t) || /^[a-z]$/i.test(t),
  ).length
  return mathy / tokens.length
}

// 0-1. Higher means more likely to be mathematics.
export function mathiness(text) {
  const words = text.toLowerCase().match(/[a-z]+/g) || []
  if (!words.length) return 0
  const strong = words.filter((w) => VOCAB.has(w)).length
  const weak = words.filter((w) => WEAK_VOCAB.has(w)).length
  // Weak words are worth a fraction of a strong one and only when a strong one
  // is present at all, so a finance meeting saying "times" and "over" scores
  // nothing from them.
  const vocab = strong === 0 ? 0 : (strong + weak * 0.25) / words.length
  return Math.min(1, vocab * 8 + density(text) * 0.8)
}

// Detection threshold. Tuned so the lecture fixture in math.test.mjs passes
// and the finance fixture does not. If a future fixture forces a change, move
// this — never weaken the finance test to make a lecture test pass, because a
// false positive silently reshapes someone's meeting notes.
const THRESHOLD = 0.35

// Too few words to judge. A single line like "the integral of x" is exactly
// the case where a wrong guess is most likely and least recoverable.
const MIN_WORDS = 25

export function looksMathy(text) {
  const words = text.toLowerCase().match(/[a-z]+/g) || []
  if (words.length < MIN_WORDS) return false
  if (!words.some((w) => VOCAB.has(w))) return false
  return mathiness(text) >= THRESHOLD
}

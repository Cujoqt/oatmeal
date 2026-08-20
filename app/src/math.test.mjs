import test from 'node:test'
import assert from 'node:assert/strict'
import { looksMathy, mathiness, speechToLatex } from './math.js'

// Real ASR output shape: no punctuation to speak of, spelled-out operators.
const LECTURE = `so the derivative of x squared is 2 x and if we integrate that
  back we get x squared over 2 plus a constant now the limit as h goes to zero
  of f of x plus h minus f of x over h that is the definition we wrote down
  last week so the slope of the tangent line at x equals 3 is 6`

const FINANCE = `so Q3 revenue came in at 4.2 million against a 3.8 million
  forecast that is 11 percent over and headcount went from 42 to 51 which puts
  us at 1.3 million in burn we should factor that into the board deck and the
  runway is 14 months at the current rate`

const CHITCHAT = `yeah I think we should push the launch to next Tuesday
  because the design review is not done and Sarah is out on Monday`

test('a math lecture is detected', () => {
  assert.equal(looksMathy(LECTURE), true)
})

test('a meeting dense in numbers is not detected', () => {
  assert.equal(looksMathy(FINANCE), false, 'digits alone must not be enough')
})

test('ordinary conversation is not detected', () => {
  assert.equal(looksMathy(CHITCHAT), false)
})

test('a lecture scores strictly higher than a finance meeting', () => {
  assert.ok(mathiness(LECTURE) > mathiness(FINANCE))
})

test('a short line is never detected on its own', () => {
  assert.equal(looksMathy('the integral of x'), false, 'too short to be sure')
})

test('single math words used in ordinary business speech are not detected', () => {
  // "matrix", "slope", "vector" and "solve" as loose business metaphor, no
  // spoken-math phrase and no notation. This is exactly the ambiguity that
  // defeated a single-word vocabulary list: these words are ordinary English.
  const businessText = `we need to assess the risk matrix and review the slope of
    this rollout and then discuss the vector of business priorities for the next
    quarter so we can solve these problems before the board meeting`
  assert.equal(looksMathy(businessText), false, 'vocabulary alone must not be enough')
})

test('one incidental digit does not turn business speech into math', () => {
  const businessTextWithDigit = `we need to assess the risk matrix version 2 and review
    the slope of this rollout and then discuss the vector of business priorities
    for the next quarter so we can solve these problems before the board meeting`
  assert.equal(looksMathy(businessTextWithDigit), false, 'vocabulary plus one digit must not be enough')
})

test('a notation-light lecture is still detected', () => {
  // Heavy on spoken-math phrases and survivor words (derivative of, integral
  // of, theorem, polynomial, coefficient) but almost no notation — only one
  // single-letter variable in 50-odd words. Phrases carry this one, not density.
  const lightNotationLecture = `the derivative of equation x is the rate of change and
    when we use the power theorem we can differentiate a polynomial with many
    coefficient terms and the integral of that same equation gives us the area
    under the curve which is fundamental to understanding how these mathematical
    concepts relate`
  assert.equal(looksMathy(lightNotationLecture), true, 'spoken-math phrases with minimal notation must be detected')
})

// Two passages a reviewer constructed to defeat a single-word vocabulary list:
// every math-adjacent word here ("derivatives desk", "credit limit", "proof of
// concept", "common denominator", "a fraction of") is ordinary trading-desk
// English. No spoken-math phrase ("the derivative of", "the limit as", "square
// root of", ...) appears in either one. They must score zero, not just below
// threshold, because there is no vocabulary evidence in them at all.

const ADVERSARIAL_DESK_UPDATE = `our derivatives desk had a strong quarter and the
  team stayed well within the credit limit the whole time proof of concept work
  on the new trading platform wrapped up ahead of schedule and margins are a
  fraction of what they were last year so the common denominator across every
  desk is discipline the numbers came in around version 2 of the forecast`

const ADVERSARIAL_RISK_UPDATE = `the derivatives desk closed 4.2 million in
  notional exposure this week and derivative contracts on the rate swap book
  widened by 11 basis points the risk team flagged that derivative exposure
  against the 3.8 million limit before the board meeting on Tuesday and
  headcount on the desk moved from 12 to 14 this quarter`

test('a desk update dense in math-adjacent business words is not detected', () => {
  assert.equal(looksMathy(ADVERSARIAL_DESK_UPDATE), false)
  assert.equal(mathiness(ADVERSARIAL_DESK_UPDATE), 0)
})

test('a risk update repeating "derivative" in its financial sense is not detected', () => {
  assert.equal(looksMathy(ADVERSARIAL_RISK_UPDATE), false)
  assert.equal(mathiness(ADVERSARIAL_RISK_UPDATE), 0)
})

// Round 4 kept "with respect to" and "solve for" as phrases on the theory
// that a trading desk never says them. It was wrong — both are ordinary
// formal business English. This passage says each once, in exactly that
// ordinary sense, and must score zero now that both phrases are gone.
const FINANCE_REVIEW_WITH_FORMAL_PHRASING = `this quarter we reviewed spend
  with respect to the approved budget across every team and came in under
  plan by six percent leadership asked us to solve for the gap before the
  next board meeting so finance is pulling together options and we should
  have a recommendation ready by Friday for the leadership team to review`

test('formal business phrasing ("with respect to", "solve for") is not detected', () => {
  assert.equal(looksMathy(FINANCE_REVIEW_WITH_FORMAL_PHRASING), false)
  assert.equal(mathiness(FINANCE_REVIEW_WITH_FORMAL_PHRASING), 0)
})

// Round 4's "limit as" matched across an unrelated clause boundary: "credit
// limit" ends one thought and "as we approach the end of the quarter" starts
// the next, and mashing them together reads as the lecture phrase. The fix
// requires the leading article ("the limit as"), which a lecturer actually
// says and "credit limit" never precedes with.
const TREASURY_CREDIT_LIMIT = `during the risk review treasury flagged that we
  are close to breaching the credit limit as we approach the end of the
  quarter and asked finance to confirm headroom before any new draws are
  approved by the committee`

test('"credit limit as" does not read as "the limit as"', () => {
  assert.equal(looksMathy(TREASURY_CREDIT_LIMIT), false)
  assert.equal(mathiness(TREASURY_CREDIT_LIMIT), 0)
})

// A proof-heavy real-analysis segment: no arithmetic notation to speak of,
// and its distinguishing vocabulary (epsilon, delta, continuity, convergence,
// Cauchy) is exactly the kind that is either business-ambiguous ("continuity"
// -> business continuity, "convergence" -> convergence of trends) or, in the
// case of "epsilon", a real vendor name. It is detected through phrases that
// need the literal construction ("for every epsilon", "epsilon neighborhood",
// "cauchy sequence", "continuous function") rather than the bare words.
const REAL_ANALYSIS_LECTURE = `so to prove continuity at this point we need to
  show that for every epsilon greater than zero there exists a delta such
  that whenever the distance between x and the point is less than delta the
  distance between the function values is less than epsilon this is the
  epsilon neighborhood argument and once this holds at every point in the
  domain we call the function a continuous function and by the cauchy
  sequence criterion the series converges`

test('a proof-heavy real-analysis lecture is detected', () => {
  assert.equal(looksMathy(REAL_ANALYSIS_LECTURE), true)
})

// speechToLatex — the brief's four verbatim cases, then coverage beyond them.
// Every emitted command must exist in mathml.js's SYMBOLS/FUNCTIONS/BIGOPS,
// since that table is the only thing standing between this output and a
// rendered equation in the live panel.

test('spoken powers', () => {
  assert.equal(speechToLatex('x squared plus 3 x'), 'x^2 + 3x')
})

test('spoken roots', () => {
  assert.equal(speechToLatex('the square root of 2'), '\\sqrt{2}')
})

test('spoken fractions', () => {
  assert.equal(speechToLatex('a over b'), '\\frac{a}{b}')
})

test('what it cannot handle returns null rather than a wrong answer', () => {
  assert.equal(
    speechToLatex('the limit as h goes to zero of f of x plus h minus f of x over h'),
    null,
    'falling through to the model beats emitting mangled LaTeX',
  )
})

test('cubed', () => {
  assert.equal(speechToLatex('x cubed'), 'x^3')
})

test('to the power of', () => {
  assert.equal(speechToLatex('x to the power of 5'), 'x^5')
})

test('square root without the leading article', () => {
  assert.equal(speechToLatex('square root of 9'), '\\sqrt{9}')
})

test('pi via multiplication', () => {
  assert.equal(speechToLatex('two times pi'), '2 \\times \\pi')
})

test('theta and equals', () => {
  assert.equal(speechToLatex('theta equals pi'), '\\theta = \\pi')
})

test('minus', () => {
  assert.equal(speechToLatex('x minus 1'), 'x - 1')
})

test('divided by, spelled-out numbers', () => {
  assert.equal(speechToLatex('ten divided by two'), '10 \\div 2')
})

test('the definite integral from A to B', () => {
  assert.equal(
    speechToLatex('the integral from zero to one of x squared d x'),
    '\\int_{0}^{1} x^2 \\, dx',
  )
})

test('a decimal number is refused rather than mangled', () => {
  // Punctuation is a word separator during normalization, so without this
  // guard "3.5" would tokenize as "3" then "5" and implicit multiplication
  // would concatenate them into "35" — silently changing the value.
  assert.equal(speechToLatex('x plus 3.5'), null)
})

test('ordinary conversation with a number in it returns null', () => {
  assert.equal(speechToLatex("let's meet at 3 tomorrow"), null)
})

test('a sentence the table only covers halfway returns null', () => {
  // Real integral syntax needs "from A to B" — dropping it is a common way
  // a lecturer's phrasing falls just outside the table's coverage.
  assert.equal(speechToLatex('the integral of x squared'), null)
})

test('"over" only forms a fraction when it is the sentence\'s only operator', () => {
  // "x plus 1 over 2" is genuinely ambiguous — over the whole sum, or just
  // the 1? Guessing either reading risks emitting the wrong one with no way
  // for anything downstream to tell, so this falls through to the model.
  assert.equal(speechToLatex('x plus 1 over 2'), null)
})

// Fix round 1 — F1: compound number words above twenty were concatenating
// into a wrong digit string ("twenty one" -> "201"), the same failure mode
// the decimal guard above exists to stop, just on a different code path.

test('a compound number word converts to its actual value', () => {
  assert.equal(speechToLatex('twenty one'), '21')
})

test('a compound number word does not corrupt the rest of the expression', () => {
  assert.equal(speechToLatex('thirteen plus twenty one'), '13 + 21')
})

test('a compound number word after a power still converts correctly', () => {
  assert.equal(speechToLatex('x squared plus twenty one'), 'x^2 + 21')
})

test('two adjacent bare numbers with no operator between them return null', () => {
  // "two three" is ambiguous — one number misheard as two words, or two
  // separate numbers? Concatenating into "23" would be a silent guess, so
  // this must fall through to the model instead.
  assert.equal(speechToLatex('two three'), null)
})

test('two adjacent numeral tokens are refused the same way as number words', () => {
  assert.equal(speechToLatex('12 21'), null)
})

// Fix round 1 — F2: a square root followed by "plus"/"minus" is the same
// ambiguous-scope class as "x plus 1 over 2", which already returns null —
// this makes the square root case refuse consistently instead of guessing.

test('an ambiguous square-root scope returns null instead of picking a reading', () => {
  // "the square root of x plus one" could mean sqrt(x) + 1 or sqrt(x + 1) —
  // there is no way to tell from the words alone which the speaker meant.
  assert.equal(speechToLatex('the square root of x plus one'), null)
})

import test from 'node:test'
import assert from 'node:assert/strict'
import { looksMathy, mathiness } from './math.js'

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
// English. No spoken-math phrase ("derivative of", "with respect to", "solve
// for", ...) appears in either one. They must score zero, not just below
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

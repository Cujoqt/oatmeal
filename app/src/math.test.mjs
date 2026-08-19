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

test('vocabulary in ordinary business context is not detected', () => {
  // VOCAB words (matrix, slope, vector, solve) used in purely business sense,
  // with no digits or operators: must not trigger detection despite containing
  // multiple dictionary words. This is the false-positive case the heuristic
  // exists to prevent.
  const businessText = `we need to assess the risk matrix and review the slope of
    this rollout and then discuss the vector of business priorities for the next
    quarter so we can solve these problems before the board meeting`
  assert.equal(looksMathy(businessText), false, 'vocabulary alone must not be enough')
})

test('one incidental digit does not trigger detection', () => {
  // Vocabulary in business context plus one stray digit: the edge case that
  // defeated the previous density gate. With a multiplicative formula, both
  // signals must contribute meaningfully; one incidental digit cannot push a
  // marginally mathy text across the threshold.
  const businessTextWithDigit = `we need to assess the risk matrix version 2 and review
    the slope of this rollout and then discuss the vector of business priorities
    for the next quarter so we can solve these problems before the board meeting`
  assert.equal(looksMathy(businessTextWithDigit), false, 'vocabulary plus one digit must not be enough')
})

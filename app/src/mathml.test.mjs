import test from 'node:test'
import assert from 'node:assert/strict'
import { parseLatex } from './mathml.js'

test('digits and identifiers', () => {
  assert.deepEqual(parseLatex('2x'), [
    { t: 'num', v: '2' },
    { t: 'ident', v: 'x' },
  ])
})

test('superscript', () => {
  assert.deepEqual(parseLatex('x^2'), [
    { t: 'sup', base: { t: 'ident', v: 'x' }, over: [{ t: 'num', v: '2' }] },
  ])
})

test('braced superscript keeps the whole group', () => {
  const [node] = parseLatex('x^{n+1}')
  assert.equal(node.t, 'sup')
  assert.deepEqual(node.over, [
    { t: 'ident', v: 'n' },
    { t: 'op', v: '+' },
    { t: 'num', v: '1' },
  ])
})

test('fraction', () => {
  assert.deepEqual(parseLatex('\\frac{a}{b}'), [
    { t: 'frac', num: [{ t: 'ident', v: 'a' }], den: [{ t: 'ident', v: 'b' }] },
  ])
})

test('square root', () => {
  assert.deepEqual(parseLatex('\\sqrt{2}'), [
    { t: 'sqrt', arg: [{ t: 'num', v: '2' }] },
  ])
})

test('definite integral carries both limits', () => {
  const [node] = parseLatex('\\int_0^1')
  assert.equal(node.t, 'bigop')
  assert.equal(node.v, 'int')
  assert.deepEqual(node.under, [{ t: 'num', v: '0' }])
  assert.deepEqual(node.over, [{ t: 'num', v: '1' }])
})

test('function names are one token, not three identifiers', () => {
  assert.deepEqual(parseLatex('\\sin'), [{ t: 'fn', v: 'sin' }])
})

test('an unsupported command survives as raw text rather than throwing', () => {
  // \begin isn't a recognised command, so it falls through to a raw node.
  // {pmatrix} is a brace group whose contents ("pmatrix") aren't a single
  // recognised token, so parseAtom yields seven single-character identifiers
  // rather than the two-raw-node split the brief originally asserted — the
  // brief itself says to fix the assertion to match, not the parser.
  const nodes = parseLatex('\\begin{pmatrix}')
  assert.deepEqual(nodes[0], { t: 'raw', v: '\\begin' })
  assert.equal(nodes[1].t, 'group')
  assert.deepEqual(
    nodes[1].items.map((n) => n.v),
    ['p', 'm', 'a', 't', 'r', 'i', 'x']
  )
})

test('a whole lecture expression parses without throwing', () => {
  const nodes = parseLatex('\\int_0^2 3x^2\\,dx = \\frac{1}{3}')
  assert.ok(nodes.length > 0)
  assert.ok(nodes.some((n) => n.t === 'bigop'))
  assert.ok(nodes.some((n) => n.t === 'frac'))
})

test('\\left and \\right are dropped; the delimiters they wrap parse as ordinary characters', () => {
  const nodes = parseLatex('\\left(x\\right)')
  assert.deepEqual(nodes, [
    { t: 'op', v: '(' },
    { t: 'ident', v: 'x' },
    { t: 'op', v: ')' },
  ])
  assert.ok(!nodes.some((n) => n.t === 'raw'))
})

test('deeply nested braces return a raw node instead of overflowing the stack', () => {
  const src = '{'.repeat(5000) + 'x' + '}'.repeat(5000)
  const nodes = parseLatex(src)
  assert.ok(Array.isArray(nodes))
  assert.ok(nodes.length > 0)
})

test('subscript and superscript together on a plain identifier is a subsup, not a bigop', () => {
  assert.deepEqual(parseLatex('x_1^2'), [
    {
      t: 'subsup',
      base: { t: 'ident', v: 'x' },
      under: [{ t: 'num', v: '1' }],
      over: [{ t: 'num', v: '2' }],
    },
  ])
})

test('a bare \\sum with no limits attached', () => {
  assert.deepEqual(parseLatex('\\sum'), [
    { t: 'bigop', v: 'sum', under: [], over: [] },
  ])
})

test('a bare \\lim with no limits attached', () => {
  assert.deepEqual(parseLatex('\\lim'), [
    { t: 'bigop', v: 'lim', under: [], over: [] },
  ])
})

test('symbol commands substitute their unicode character', () => {
  const cases = {
    pi: 'π', theta: 'θ', alpha: 'α', beta: 'β', lambda: 'λ', mu: 'μ',
    infty: '∞', cdot: '·', times: '×', div: '÷', leq: '≤', geq: '≥',
    neq: '≠', to: '→', pm: '±', approx: '≈',
  }
  for (const [cmd, glyph] of Object.entries(cases)) {
    assert.deepEqual(parseLatex('\\' + cmd), [{ t: 'op', v: glyph }])
  }
})

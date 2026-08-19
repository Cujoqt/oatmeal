// Typesetting a fixed LaTeX subset as MathML.
//
// WKWebView — which is what Tauri renders in on macOS — typesets MathML
// natively, so real fraction bars and integral signs cost a parser we can test
// rather than a vendored library plus base64 font files that the CSP and the
// offline rule would both make awkward.
//
// The parser is pure and has no DOM dependency, so `node --test` can exercise
// every form. Only `toMathML` touches `document`.

const SYMBOLS = {
  pi: 'π', theta: 'θ', alpha: 'α', beta: 'β', lambda: 'λ', mu: 'μ',
  infty: '∞', cdot: '·', times: '×', div: '÷', leq: '≤', geq: '≥',
  neq: '≠', to: '→', pm: '±', approx: '≈',
}
const FUNCTIONS = new Set(['sin', 'cos', 'tan', 'log', 'ln', 'exp'])
const BIGOPS = { int: '∫', sum: '∑', prod: '∏', lim: 'lim' }

/// Split into the smallest units the parser cares about. `\,` and `\;` are
/// LaTeX spacing and carry no meaning here, so they are dropped outright.
function tokenize(src) {
  const out = []
  let i = 0
  while (i < src.length) {
    const c = src[i]
    if (c === '\\') {
      const m = /^\\([a-zA-Z]+|[,;! ])/.exec(src.slice(i))
      if (!m) { out.push({ k: 'raw', v: c }); i += 1; continue }
      i += m[0].length
      if ([',', ';', '!', ' '].includes(m[1])) continue
      out.push({ k: 'cmd', v: m[1] })
      continue
    }
    if (/\s/.test(c)) { i += 1; continue }
    if (/\d/.test(c)) {
      const m = /^\d+(\.\d+)?/.exec(src.slice(i))
      out.push({ k: 'num', v: m[0] }); i += m[0].length; continue
    }
    if (/[a-zA-Z]/.test(c)) { out.push({ k: 'ident', v: c }); i += 1; continue }
    if ('{}^_'.includes(c)) { out.push({ k: c, v: c }); i += 1; continue }
    out.push({ k: 'op', v: c }); i += 1
  }
  return out
}

/// One unit that a `^` or `_` can attach to: a braced group, or a single token.
function parseAtom(ts, pos) {
  const t = ts[pos]
  if (!t) return [null, pos]
  if (t.k === '{') {
    const items = []
    let i = pos + 1
    let depth = 1
    const inner = []
    while (i < ts.length) {
      if (ts[i].k === '{') depth += 1
      if (ts[i].k === '}') { depth -= 1; if (depth === 0) break }
      inner.push(ts[i]); i += 1
    }
    items.push(...parseTokens(inner))
    return [items, i + 1]
  }
  const [node, next] = parseOne(ts, pos)
  return [node ? [node] : null, next]
}

function parseOne(ts, pos) {
  const t = ts[pos]
  if (!t) return [null, pos]
  if (t.k === 'num') return [{ t: 'num', v: t.v }, pos + 1]
  if (t.k === 'ident') return [{ t: 'ident', v: t.v }, pos + 1]
  if (t.k === 'op') return [{ t: 'op', v: t.v }, pos + 1]
  if (t.k === 'cmd') {
    if (t.v === 'frac') {
      const [num, p1] = parseAtom(ts, pos + 1)
      const [den, p2] = parseAtom(ts, p1)
      if (num && den) return [{ t: 'frac', num, den }, p2]
      return [{ t: 'raw', v: '\\frac' }, pos + 1]
    }
    if (t.v === 'sqrt') {
      const [arg, p1] = parseAtom(ts, pos + 1)
      if (arg) return [{ t: 'sqrt', arg }, p1]
      return [{ t: 'raw', v: '\\sqrt' }, pos + 1]
    }
    if (BIGOPS[t.v]) return [{ t: 'bigop', v: t.v, under: [], over: [] }, pos + 1]
    if (FUNCTIONS.has(t.v)) return [{ t: 'fn', v: t.v }, pos + 1]
    if (SYMBOLS[t.v]) return [{ t: 'op', v: SYMBOLS[t.v] }, pos + 1]
    return [{ t: 'raw', v: '\\' + t.v }, pos + 1]
  }
  if (t.k === '{') {
    const [items, p1] = parseAtom(ts, pos)
    // A bare group with no script attached is just its contents, except when
    // it parsed as nothing recognisable — then keep the source text.
    if (items && items.length) return [items.length === 1 ? items[0] : { t: 'group', items }, p1]
    return [{ t: 'raw', v: '{}' }, p1]
  }
  return [{ t: 'raw', v: t.v }, pos + 1]
}

function parseTokens(ts) {
  const out = []
  let i = 0
  while (i < ts.length) {
    let [node, next] = parseOne(ts, i)
    if (!node) { i = next > i ? next : i + 1; continue }
    i = next
    // Scripts bind to whatever came immediately before.
    let under = null
    let over = null
    while (i < ts.length && (ts[i].k === '^' || ts[i].k === '_')) {
      const kind = ts[i].k
      const [arg, p] = parseAtom(ts, i + 1)
      i = p
      if (kind === '^') over = arg
      else under = arg
    }
    if (node.t === 'bigop') {
      if (under) node.under = under
      if (over) node.over = over
    } else if (under && over) {
      node = { t: 'subsup', base: node, under, over }
    } else if (over) {
      node = { t: 'sup', base: node, over }
    } else if (under) {
      node = { t: 'sub', base: node, under }
    }
    out.push(node)
  }
  return out
}

/// Parse a LaTeX fragment into nodes. Never throws — unsupported input becomes
/// `raw` nodes that the emitter renders as plain text.
export function parseLatex(src) {
  return parseTokens(tokenize(String(src)))
}

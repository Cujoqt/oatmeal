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

// parseAtom / parseOne / parseTokens are mutually recursive with no natural
// base case other than running out of tokens, so a brace group nested deeper
// than this blows the call stack before it ever does that — this is LLM-
// generated LaTeX, not hand-written, and the never-throw guarantee has to
// hold against input nobody would write by hand. No real lecture nests 100
// levels deep, so bailing out to a raw node here costs nothing real.
const MAX_DEPTH = 100

/// Split into the smallest units the parser cares about. `\,` and `\;` are
/// LaTeX spacing and carry no meaning here, so they are dropped outright.
/// `\left` and `\right` only control delimiter sizing (MathML sizes
/// delimiters itself), so they are dropped the same way — the delimiter
/// character that follows (e.g. the `(` in `\left(`) is an ordinary
/// character and gets tokenized normally on the next pass.
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
      if (m[1] === 'left' || m[1] === 'right') continue
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
/// `depth` counts brace nesting so far; it is threaded through the three
/// mutually recursive parse functions purely to enforce MAX_DEPTH.
function parseAtom(ts, pos, depth) {
  const t = ts[pos]
  if (!t) return [null, pos]
  if (t.k === '{') {
    if (depth >= MAX_DEPTH) {
      // Past the ceiling: don't recurse into the contents at all, just walk
      // past the matching close brace (a plain loop, not recursion — this is
      // the step that keeps arbitrarily deep leftover nesting from costing
      // any more stack) and hand back the whole group as raw text.
      let i = pos + 1
      let braceDepth = 1
      while (i < ts.length && braceDepth > 0) {
        if (ts[i].k === '{') braceDepth += 1
        else if (ts[i].k === '}') braceDepth -= 1
        i += 1
      }
      return [[{ t: 'raw', v: '{...}' }], i]
    }
    const items = []
    let i = pos + 1
    let braceDepth = 1
    const inner = []
    while (i < ts.length) {
      if (ts[i].k === '{') braceDepth += 1
      if (ts[i].k === '}') { braceDepth -= 1; if (braceDepth === 0) break }
      inner.push(ts[i]); i += 1
    }
    items.push(...parseTokens(inner, depth + 1))
    return [items, i + 1]
  }
  const [node, next] = parseOne(ts, pos, depth)
  return [node ? [node] : null, next]
}

function parseOne(ts, pos, depth) {
  const t = ts[pos]
  if (!t) return [null, pos]
  if (t.k === 'num') return [{ t: 'num', v: t.v }, pos + 1]
  if (t.k === 'ident') return [{ t: 'ident', v: t.v }, pos + 1]
  if (t.k === 'op') return [{ t: 'op', v: t.v }, pos + 1]
  if (t.k === 'cmd') {
    if (t.v === 'frac') {
      const [num, p1] = parseAtom(ts, pos + 1, depth)
      const [den, p2] = parseAtom(ts, p1, depth)
      if (num && den) return [{ t: 'frac', num, den }, p2]
      return [{ t: 'raw', v: '\\frac' }, pos + 1]
    }
    if (t.v === 'sqrt') {
      const [arg, p1] = parseAtom(ts, pos + 1, depth)
      if (arg) return [{ t: 'sqrt', arg }, p1]
      return [{ t: 'raw', v: '\\sqrt' }, pos + 1]
    }
    if (BIGOPS[t.v]) return [{ t: 'bigop', v: t.v, under: [], over: [] }, pos + 1]
    if (FUNCTIONS.has(t.v)) return [{ t: 'fn', v: t.v }, pos + 1]
    if (SYMBOLS[t.v]) return [{ t: 'op', v: SYMBOLS[t.v] }, pos + 1]
    return [{ t: 'raw', v: '\\' + t.v }, pos + 1]
  }
  if (t.k === '{') {
    const [items, p1] = parseAtom(ts, pos, depth)
    // A bare group with no script attached is just its contents, except when
    // it parsed as nothing recognisable — then keep the source text.
    if (items && items.length) return [items.length === 1 ? items[0] : { t: 'group', items }, p1]
    return [{ t: 'raw', v: '{}' }, p1]
  }
  return [{ t: 'raw', v: t.v }, pos + 1]
}

function parseTokens(ts, depth) {
  const out = []
  let i = 0
  while (i < ts.length) {
    let [node, next] = parseOne(ts, i, depth)
    if (!node) { i = next > i ? next : i + 1; continue }
    i = next
    // Scripts bind to whatever came immediately before.
    let under = null
    let over = null
    while (i < ts.length && (ts[i].k === '^' || ts[i].k === '_')) {
      const kind = ts[i].k
      const [arg, p] = parseAtom(ts, i + 1, depth)
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
  return parseTokens(tokenize(String(src)), 0)
}

const MATHML_NS = 'http://www.w3.org/1998/Math/MathML'

function m(tag, text) {
  const el = document.createElementNS(MATHML_NS, tag)
  // textContent, never innerHTML — this renders model output, and the promise
  // that model output can never become markup is the whole reason the rest of
  // the app goes through textContent too.
  if (text !== undefined) el.textContent = text
  return el
}

function row(nodes) {
  const r = m('mrow')
  for (const n of nodes) r.appendChild(emit(n))
  return r
}

function emit(node) {
  switch (node.t) {
    case 'num': return m('mn', node.v)
    case 'ident': return m('mi', node.v)
    case 'op': return m('mo', node.v)
    case 'fn': return m('mi', node.v)
    case 'raw': return m('mtext', node.v)
    case 'group': return row(node.items)
    case 'frac': {
      const f = m('mfrac')
      f.append(row(node.num), row(node.den))
      return f
    }
    case 'sqrt': {
      const s = m('msqrt')
      s.appendChild(row(node.arg))
      return s
    }
    case 'sup': {
      const s = m('msup')
      s.append(emit(node.base), row(node.over))
      return s
    }
    case 'sub': {
      const s = m('msub')
      s.append(emit(node.base), row(node.under))
      return s
    }
    case 'subsup': {
      const s = m('msubsup')
      s.append(emit(node.base), row(node.under), row(node.over))
      return s
    }
    case 'bigop': {
      const glyph = m('mo', BIGOPS[node.v])
      if (!node.under.length && !node.over.length) return glyph
      // munderover stacks the limits above and below, which is what makes a
      // definite integral read as one.
      const u = m('munderover')
      u.append(glyph, row(node.under), row(node.over))
      return u
    }
    default: return m('mtext', String(node.v ?? ''))
  }
}

/// Typeset a LaTeX fragment. Returns a `<math>` element, always — unsupported
/// input degrades to plain text inside it rather than failing.
export function toMathML(latex, display = false) {
  const math = m('math')
  if (display) math.setAttribute('display', 'block')
  math.appendChild(row(parseLatex(latex)))
  return math
}

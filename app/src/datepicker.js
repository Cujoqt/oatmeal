// A small, dependency-free calendar dropdown: a button showing the chosen
// date that opens a month-grid popup on click. The CSP forbids remote
// stylesheets/scripts and the app must work offline, so this is hand-built
// rather than pulled from a library.

const DOW = ['S', 'M', 'T', 'W', 'T', 'F', 'S']

function pad(n) {
  return String(n).padStart(2, '0')
}

function toISO(date) {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}

function fromISO(iso) {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y, m - 1, d)
}

function fmtLabel(date) {
  return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })
}

/// Builds a date-picker button + popup inside `container` (which must be
/// `position: relative` in CSS). `onChange(iso)` fires whenever a day is
/// picked. Returns `{ getValue, setValue }`.
export function createDatePicker(container, onChange = () => {}) {
  let value = null
  let viewMonth = new Date()
  viewMonth.setDate(1)

  const btn = document.createElement('button')
  btn.type = 'button'
  btn.className = 'dp-btn'
  btn.textContent = 'Pick a date'

  const pop = document.createElement('div')
  pop.className = 'dp-pop'
  pop.hidden = true

  container.append(btn, pop)

  function renderPop() {
    pop.innerHTML = ''

    const head = document.createElement('div')
    head.className = 'dp-head'
    const prev = document.createElement('button')
    prev.type = 'button'
    prev.textContent = '‹'
    prev.addEventListener('click', (e) => {
      e.stopPropagation()
      viewMonth.setMonth(viewMonth.getMonth() - 1)
      renderPop()
    })
    const label = document.createElement('span')
    label.textContent = viewMonth.toLocaleDateString(undefined, { month: 'long', year: 'numeric' })
    const next = document.createElement('button')
    next.type = 'button'
    next.textContent = '›'
    next.addEventListener('click', (e) => {
      e.stopPropagation()
      viewMonth.setMonth(viewMonth.getMonth() + 1)
      renderPop()
    })
    head.append(prev, label, next)
    pop.appendChild(head)

    const grid = document.createElement('div')
    grid.className = 'dp-grid'
    for (const d of DOW) {
      const cell = document.createElement('span')
      cell.className = 'dp-dow'
      cell.textContent = d
      grid.appendChild(cell)
    }

    const first = new Date(viewMonth.getFullYear(), viewMonth.getMonth(), 1)
    const startOffset = first.getDay()
    const daysInMonth = new Date(viewMonth.getFullYear(), viewMonth.getMonth() + 1, 0).getDate()
    const todayISO = toISO(new Date())

    for (let i = 0; i < startOffset; i++) grid.appendChild(document.createElement('span'))

    for (let day = 1; day <= daysInMonth; day++) {
      const cellDate = new Date(viewMonth.getFullYear(), viewMonth.getMonth(), day)
      const iso = toISO(cellDate)
      const cell = document.createElement('button')
      cell.type = 'button'
      cell.className = 'dp-day'
      cell.textContent = String(day)
      if (iso === todayISO) cell.classList.add('today')
      if (iso === value) cell.classList.add('sel')
      cell.addEventListener('click', (e) => {
        e.stopPropagation()
        value = iso
        btn.textContent = fmtLabel(cellDate)
        pop.hidden = true
        onChange(value)
      })
      grid.appendChild(cell)
    }
    pop.appendChild(grid)
  }

  btn.addEventListener('click', (e) => {
    e.stopPropagation()
    pop.hidden = !pop.hidden
    if (!pop.hidden) renderPop()
  })
  document.addEventListener('click', (e) => {
    if (!container.contains(e.target)) pop.hidden = true
  })

  return {
    getValue: () => value,
    setValue: (iso) => {
      value = iso
      if (iso) {
        viewMonth = fromISO(iso)
        viewMonth.setDate(1)
        btn.textContent = fmtLabel(fromISO(iso))
      } else {
        btn.textContent = 'Pick a date'
      }
    },
  }
}

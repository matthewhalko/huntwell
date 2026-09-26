import React from 'react'
import { createPortal } from 'react-dom'

export interface PickOption {
  value: string
  label: string
  /// Optional glyph shown before the label, in the button and the list. A
  /// picker where only some options have one looks broken, so give every
  /// option an icon or none of them.
  icon?: React.ReactNode
  /// Secondary text on the right of a row — a price, a timezone offset.
  detail?: string
}

type Pos = { left: number; width: number; top?: number; bottom?: number }

/**
 * A dropdown we draw ourselves.
 *
 * A native `<select>` popup is the operating system's, not the page's: CSS can
 * reach the closed control and nothing else, so in dark mode the open list
 * arrives in whatever palette the platform feels like. Everything here is our
 * own markup, so it takes the theme tokens like the rest of the app — and a
 * long list (every timezone, every plan) gets a filter box, which a native
 * select cannot have at all.
 *
 * The list is portaled to `document.body` so a parent with overflow or a
 * transform (a modal, a rising pane) cannot clip it or trap it in the
 * operating system's combo box.
 */
export function Picker({
  value,
  options,
  onChange,
  placeholder = 'Select…',
  searchFrom = 8,
  className = '',
}: {
  value: string
  options: PickOption[]
  onChange: (v: string) => void
  placeholder?: string
  /// Show the filter box once the list is at least this long.
  searchFrom?: number
  className?: string
}) {
  const [open, setOpen] = React.useState(false)
  const [q, setQ] = React.useState('')
  const [cursor, setCursor] = React.useState(0)
  const [pos, setPos] = React.useState<Pos>({ left: 0, width: 220 })
  const box = React.useRef<HTMLDivElement | null>(null)
  const pop = React.useRef<HTMLDivElement | null>(null)
  const listRef = React.useRef<HTMLDivElement | null>(null)

  const shown = React.useMemo(() => {
    const n = q.trim().toLowerCase()
    return n
      ? options.filter(
          (o) =>
            o.label.toLowerCase().includes(n) ||
            o.value.toLowerCase().includes(n) ||
            (o.detail || '').toLowerCase().includes(n),
        )
      : options
  }, [options, q])

  const current = options.find((o) => o.value === value)

  const place = React.useCallback(() => {
    const el = box.current
    if (!el) return
    const r = el.getBoundingClientRect()
    const room = 280
    const up = window.innerHeight - r.bottom < room && r.top > window.innerHeight - r.bottom
    setPos(
      up
        ? { left: r.left, width: Math.max(r.width, 220), bottom: window.innerHeight - r.top + 4 }
        : { left: r.left, width: Math.max(r.width, 220), top: r.bottom + 4 },
    )
  }, [])

  // Opening starts on the current value, so Enter is a no-op rather than a
  // surprise, and the list is scrolled to it.
  React.useEffect(() => {
    if (!open) return
    setQ('')
    const i = Math.max(0, options.findIndex((o) => o.value === value))
    setCursor(i)
    place()
    const t = setTimeout(() => {
      const el = listRef.current?.children[i] as HTMLElement | undefined
      el?.scrollIntoView({ block: 'center' })
    }, 0)
    return () => clearTimeout(t)
  }, [open, place])

  React.useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => {
      const t = e.target as Node
      if (box.current?.contains(t) || pop.current?.contains(t)) return
      setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.stopPropagation()
      e.preventDefault()
      setOpen(false)
    }
    document.addEventListener('mousedown', away)
    document.addEventListener('keydown', onKey, true)
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true)
    return () => {
      document.removeEventListener('mousedown', away)
      document.removeEventListener('keydown', onKey, true)
      window.removeEventListener('resize', place)
      window.removeEventListener('scroll', place, true)
    }
  }, [open, place])

  const pick = (v: string) => {
    onChange(v)
    setOpen(false)
  }

  const keys = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault()
      const o = shown[cursor]
      if (o) pick(o.value)
      return
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    e.preventDefault()
    const next = Math.min(shown.length - 1, Math.max(0, cursor + (e.key === 'ArrowDown' ? 1 : -1)))
    setCursor(next)
    ;(listRef.current?.children[next] as HTMLElement | undefined)?.scrollIntoView({ block: 'nearest' })
  }

  return (
    <div className={'picker ' + className} ref={box}>
      <button type="button" className={'picker-btn' + (open ? ' open' : '')} onClick={() => setOpen(!open)} aria-haspopup="listbox" aria-expanded={open}>
        {current?.icon && <span className="picker-ico">{current.icon}</span>}
        <span className="picker-val">{current ? current.label : placeholder}</span>
        {current?.detail && <span className="picker-btn-detail">{current.detail}</span>}
        <span className="picker-caret" aria-hidden />
      </button>
      {open &&
        createPortal(
          <div
            ref={pop}
            className="picker-pop fixed"
            role="listbox"
            onKeyDown={keys}
            style={{ left: pos.left, width: pos.width, top: pos.top, bottom: pos.bottom }}
          >
            {options.length >= searchFrom && (
              <input
                className="picker-search"
                autoFocus
                value={q}
                placeholder="Filter…"
                onChange={(e) => {
                  setQ(e.target.value)
                  setCursor(0)
                }}
              />
            )}
            <div className="picker-list" ref={listRef}>
              {shown.map((o, i) => (
                <button
                  type="button"
                  key={o.value}
                  role="option"
                  aria-selected={o.value === value}
                  className={'picker-opt' + (o.value === value ? ' on' : '') + (i === cursor ? ' cursor' : '')}
                  onMouseEnter={() => setCursor(i)}
                  onClick={() => pick(o.value)}
                >
                  <span className="picker-opt-main">
                    {o.icon && <span className="picker-ico">{o.icon}</span>}
                    <span className="picker-opt-label">{o.label}</span>
                  </span>
                  {o.detail && <span className="picker-detail">{o.detail}</span>}
                </button>
              ))}
              {shown.length === 0 && <div className="picker-none">Nothing matches “{q}”.</div>}
            </div>
          </div>,
          document.body,
        )}
    </div>
  )
}

import React, { useMemo, useState } from 'react'
import { Effort, EFFORTS } from '../api'
import { useAuth } from '../auth'
import { Picker } from './Picker'

/// The four shapes an answer comes in, drawn rather than named. Same 24px
/// outlined family as the rail icons, so the picker looks like the app.
function KindIco({ children }: { children: React.ReactNode }) {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {children}
    </svg>
  )
}

const KIND_OPTIONS = [
  {
    value: 'auto',
    label: 'Automatic detection',
    icon: (
      <KindIco>
        <path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1" />
      </KindIco>
    ),
  },
  {
    value: 'artifacts',
    label: 'A table of things',
    icon: (
      <KindIco>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M3 9h18M3 14.5h18M9 9v11" />
      </KindIco>
    ),
  },
  {
    value: 'prospects',
    label: 'People and companies',
    icon: (
      <KindIco>
        <circle cx="9" cy="8" r="3.2" />
        <path d="M2.5 20a6.5 6.5 0 0 1 13 0" />
        <path d="M17 4h4v16h-4" />
      </KindIco>
    ),
  },
  {
    value: 'report',
    label: 'A written report',
    icon: (
      <KindIco>
        <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
        <path d="M14 3v5h5" />
        <path d="M9 13h6M9 17h4" />
      </KindIco>
    ),
  },
  {
    value: 'assets',
    label: 'Files to keep',
    icon: (
      <KindIco>
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      </KindIco>
    ),
  },
]

/** `id` is only for the editor (React keys while reordering); never sent. */
export type PlanAskCol = { name: string; prompt: string; id?: number }

let nextColId = 1
export const newCol = (): PlanAskCol => ({ name: '', prompt: '', id: nextColId++ })

/// Shared create-plan options: name, target, kind, effort, columns, sites.
/// Home and New search plan both render this so the two forms cannot drift.
export function usePlanAsk() {
  const { me } = useAuth()
  const [name, setName] = useState('')
  const [target, setTarget] = useState(10)
  const [effort, setEffort] = useState<Effort>('normal')
  const [kind, setKind] = useState('auto')
  const [cols, setCols] = useState<PlanAskCol[]>([])
  const [sites, setSites] = useState<string[]>([])
  const allowedKinds = me?.kinds
  const kindOptions = useMemo(
    () => (allowedKinds?.length ? KIND_OPTIONS.filter((o) => o.value === 'auto' || allowedKinds.includes(o.value)) : KIND_OPTIONS),
    [allowedKinds?.join(',')],
  )
  const body = (brief: string) => ({
    brief,
    name: name.trim(),
    target,
    effort,
    columns: cols.filter((c) => c.name.trim()).map(({ name, prompt }) => ({ name, prompt })),
    sites: sites.join(', '),
    kind,
  })
  return { name, setName, target, setTarget, effort, setEffort, kind, setKind, cols, setCols, sites, setSites, kindOptions, body }
}

export function PlanAskOptions({ ask }: { ask: ReturnType<typeof usePlanAsk> }) {
  const { name, setName, target, setTarget, effort, setEffort, kind, setKind, cols, setCols, sites, setSites, kindOptions } = ask
  const [siteDraft, setSiteDraft] = useState('')
  const addSite = () => {
    const host = siteDraft
      .trim()
      .toLowerCase()
      .replace(/^https?:\/\//, '')
      .replace(/^www\./, '')
      .split('/')[0]
    if (host && !sites.includes(host)) setSites([...sites, host])
    setSiteDraft('')
  }
  return (
    <div className="new-opts">
      <label>
        <span className="ask-label">Name it</span>
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Optional" />
      </label>
      <label>
        <span className="ask-label">Target results</span>
        <input type="number" min={0} max={500} value={target} onChange={(e) => setTarget(+e.target.value)} />
      </label>
      <div className="kind-field">
        <span className="ask-label">What you want back</span>
        <Picker value={kind} onChange={setKind} options={kindOptions} />
      </div>
      <div>
        <span className="ask-label">How hard to try</span>
        <div className="seg">
          {EFFORTS.map((e) => (
            <button key={e.value} type="button" className={effort === e.value ? 'active' : ''} onClick={() => setEffort(e.value)}>
              {e.label}
            </button>
          ))}
        </div>
      </div>
      {(kind === 'auto' || kind === 'artifacts') && (
        <div className="cols-field">
          <span className="ask-label">Columns</span>
          <ColumnsEditor cols={cols} setCols={setCols} />
        </div>
      )}
      <div className="cols-field">
        <span className="ask-label">Focus on these websites</span>
        {sites.length > 0 && (
          <div className="site-chips">
            {sites.map((h) => (
              <span className="site-chip" key={h}>
                {h}
                <button type="button" aria-label={`Remove ${h}`} title="Remove" onClick={() => setSites(sites.filter((x) => x !== h))}>
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="col-row">
          <input
            value={siteDraft}
            onChange={(e) => setSiteDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ',') {
                e.preventDefault()
                addSite()
              }
            }}
            placeholder="cars.com"
            aria-label="Website to focus on"
          />
          <button type="button" className="btn sm" onClick={addSite} disabled={!siteDraft.trim()}>
            Add site
          </button>
        </div>
        <p className="muted sm cols-hint">Searched first. Huntwell can still look elsewhere if these run dry.</p>
      </div>
    </div>
  )
}

/** A textarea that grows with what is typed (up to a limit, then scrolls),
 *  starting at two lines. Enter is a new line. */
function GrowingTextarea({
  value,
  onChange,
  className,
  placeholder,
  maxLength,
  ...rest
}: {
  value: string
  onChange: (v: string) => void
  className?: string
  placeholder?: string
  maxLength?: number
  'aria-label'?: string
}) {
  const ref = React.useRef<HTMLTextAreaElement>(null)
  React.useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = Math.min(el.scrollHeight + 2, 240) + 'px'
  }, [value])
  return (
    <textarea
      ref={ref}
      rows={2}
      className={className}
      value={value}
      placeholder={placeholder}
      maxLength={maxLength}
      onChange={(e) => onChange(e.target.value)}
      {...rest}
    />
  )
}

/**
 * Reordering the column list: drag a column by its handle (pointer events, so
 * touch works too), or focus the handle and press ↑ / ↓. While dragging, the
 * held column follows the pointer and swaps with a neighbour once it is
 * dragged past that neighbour's middle; the list itself is the preview.
 */
function useColumnOrder(cols: PlanAskCol[], setCols: React.Dispatch<React.SetStateAction<PlanAskCol[]>>) {
  const rows = React.useRef(new Map<number, HTMLDivElement>())
  const [dragging, setDragging] = useState<number | null>(null)
  const [dy, setDy] = useState(0)
  const drag = React.useRef<{ id: number; startY: number } | null>(null)
  const colsRef = React.useRef(cols)
  colsRef.current = cols
  // Columns made before ids existed (or handed in) get one.
  React.useEffect(() => {
    if (cols.some((c) => c.id === undefined)) setCols((cs) => cs.map((c) => (c.id === undefined ? { ...c, id: nextColId++ } : c)))
  }, [cols, setCols])

  const rowRef = (id?: number) => (el: HTMLDivElement | null) => {
    if (id === undefined) return
    if (el) rows.current.set(id, el)
    else rows.current.delete(id)
  }

  const move = (from: number, to: number) =>
    setCols((cs) => {
      if (to < 0 || to >= cs.length || from === to) return cs
      const next = cs.slice()
      const [c] = next.splice(from, 1)
      next.splice(to, 0, c)
      return next
    })

  const start = (e: React.PointerEvent, id?: number) => {
    if (id === undefined || e.button !== 0) return
    e.preventDefault()
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
    drag.current = { id, startY: e.clientY }
    setDragging(id)
    setDy(0)
    const onMove = (ev: PointerEvent) => {
      const d = drag.current
      if (!d) return
      let offset = ev.clientY - d.startY
      const list = colsRef.current
      const i = list.findIndex((c) => c.id === d.id)
      const me = rows.current.get(d.id)
      const gap = 7
      // Past the middle of the next (or previous) column: swap, and move the
      // start point by that column's height so the held one stays under the
      // pointer.
      const below = list[i + 1] && rows.current.get(list[i + 1].id!)
      const above = list[i - 1] && rows.current.get(list[i - 1].id!)
      if (below && me && offset > below.offsetHeight / 2 + gap) {
        d.startY += below.offsetHeight + gap
        offset -= below.offsetHeight + gap
        move(i, i + 1)
      } else if (above && me && offset < -(above.offsetHeight / 2 + gap)) {
        d.startY -= above.offsetHeight + gap
        offset += above.offsetHeight + gap
        move(i, i - 1)
      }
      setDy(offset)
    }
    const onUp = () => {
      drag.current = null
      setDragging(null)
      setDy(0)
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
      window.removeEventListener('pointercancel', onUp)
    }
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp)
    window.addEventListener('pointercancel', onUp)
  }

  const key = (e: React.KeyboardEvent, i: number) => {
    const to = e.key === 'ArrowUp' ? i - 1 : e.key === 'ArrowDown' ? i + 1 : -1
    if (to < 0 || to >= colsRef.current.length) return
    e.preventDefault()
    const id = colsRef.current[i].id
    move(i, to)
    // Keep the keyboard on the column that moved.
    requestAnimationFrame(() => (document.querySelector(`[data-col-handle="${id}"]`) as HTMLElement | null)?.focus())
  }

  return { dragging, dy, start, key, rowRef }
}

/**
 * The column list: a name, a multi-line description and a drag handle per
 * column, plus "Add column". Shared by the home/new-plan options and a plan's
 * Edit tab, so the two cannot drift.
 */
export function ColumnsEditor({
  cols,
  setCols,
}: {
  cols: PlanAskCol[]
  setCols: React.Dispatch<React.SetStateAction<PlanAskCol[]>>
}) {
  const setCol = (i: number, patch: Partial<PlanAskCol>) => setCols((cs) => cs.map((c, n) => (n === i ? { ...c, ...patch } : c)))
  const order = useColumnOrder(cols, setCols)
  return (
    <>
    {cols.length === 0 && (
      <p className="muted sm cols-hint">Add columns to collect a table with exactly the fields you want.</p>
    )}
    {cols.map((c, i) => (
      <div
        className={'col-row col-item' + (order.dragging === c.id ? ' dragging' : '')}
        key={c.id ?? i}
        ref={order.rowRef(c.id)}
        style={order.dragging === c.id ? { transform: `translateY(${order.dy}px)` } : undefined}
      >
        <button
          type="button"
          className="col-handle"
          title="Drag to reorder (or focus and press ↑ / ↓)"
          aria-label={`Move column ${i + 1}${c.name ? ` (${c.name})` : ''} — drag, or press up or down`}
          onPointerDown={(e) => order.start(e, c.id)}
          onKeyDown={(e) => order.key(e, i)}
          data-col-handle={c.id}
        >
          <svg width="12" height="16" viewBox="0 0 12 16" aria-hidden>
            {[3, 8, 13].flatMap((y) => [3, 9].map((x) => <circle key={`${x}${y}`} cx={x} cy={y} r="1.4" fill="currentColor" />))}
          </svg>
        </button>
        <input
          value={c.name}
          onChange={(e) => setCol(i, { name: e.target.value })}
          placeholder="Column name"
          aria-label={`Column ${i + 1} name`}
        />
        <button
          type="button"
          className="iconbtn"
          title="Remove this column"
          aria-label="Remove column"
          onClick={() => setCols((cs) => cs.filter((_, n) => n !== i))}
        >
          ×
        </button>
        {/* Full width, and as many lines as it needs: a column's
            description is often a few sentences — what counts, what
            format, where to find it. */}
        <GrowingTextarea
          className="col-prompt"
          value={c.prompt}
          onChange={(v) => setCol(i, { prompt: v })}
          placeholder={'What goes in it (optional) — e.g. "The owner\'s direct email, not info@. Leave blank if only a contact form."'}
          aria-label={`Column ${i + 1} description`}
          maxLength={2000}
        />
      </div>
    ))}
    <button type="button" className="btn sm" onClick={() => setCols((cs) => [...cs, newCol()])}>
      + Add column
    </button>
    </>
  )
}


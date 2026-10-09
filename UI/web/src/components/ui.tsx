import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import { SkeletonRows } from './Skeleton'
import { createPortal } from 'react-dom'
import { CopyIcon, SearchIcon, TickIcon } from './icons'

// ---- toasts ----
//
// One call everywhere — `toast(text)`, or `toast(text, true)` for a failure —
// rendered as a card: an icon that says which it is, the message, a close
// button, and a thin bar for how long it has left. Hovering holds it, so a long
// message can be read to the end. Successes are announced politely to a screen
// reader; failures as alerts.

interface Toast {
  id: number
  text: string
  bad?: boolean
}

const TOAST_MS = 3500
const TOAST_BAD_MS = 6500

const ToastCtx = createContext<(text: string, bad?: boolean) => void>(() => {})

export function ToastProvider({ children }: { children: React.ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([])
  const push = useCallback((text: string, bad?: boolean) => {
    const id = Date.now() + Math.random()
    // The same message twice in a row replaces the first rather than stacking.
    setToasts((t) => [...t.filter((x) => !(x.text === text && !!x.bad === !!bad)), { id, text, bad }].slice(-4))
  }, [])
  const drop = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), [])
  return (
    <ToastCtx.Provider value={push}>
      {children}
      {createPortal(
        <div className="toast-wrap">
          {toasts.map((t) => (
            <ToastCard key={t.id} toast={t} onDone={() => drop(t.id)} />
          ))}
        </div>,
        document.body,
      )}
    </ToastCtx.Provider>
  )
}

function ToastCard({ toast, onDone }: { toast: Toast; onDone: () => void }) {
  const life = toast.bad ? TOAST_BAD_MS : TOAST_MS
  const [leaving, setLeaving] = useState(false)
  const [paused, setPaused] = useState(false)
  // Time left, kept across pauses: hovering stops the clock, leaving restarts it.
  const left = useRef(life)
  const started = useRef(Date.now())
  const close = useCallback(() => {
    setLeaving(true)
    setTimeout(onDone, 180)
  }, [onDone])
  useEffect(() => {
    if (paused || leaving) return
    started.current = Date.now()
    const t = setTimeout(close, left.current)
    return () => {
      clearTimeout(t)
      left.current = Math.max(0, left.current - (Date.now() - started.current))
    }
  }, [paused, leaving, close])
  return (
    <div
      className={'toast' + (toast.bad ? ' bad' : ' ok') + (leaving ? ' leaving' : '')}
      role={toast.bad ? 'alert' : 'status'}
      aria-live={toast.bad ? 'assertive' : 'polite'}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
    >
      <span className="toast-ico" aria-hidden>
        {toast.bad ? '!' : <TickIcon size={15} />}
      </span>
      <span className="toast-text">{toast.text}</span>
      <button type="button" className="toast-x" aria-label="Dismiss" onClick={close}>
        ✕
      </button>
      <span className="toast-life" style={{ animationDuration: `${life}ms`, animationPlayState: paused ? 'paused' : 'running' }} />
    </div>
  )
}

export function useToast() {
  return useContext(ToastCtx)
}

// ---- small pieces ----

export function Badge({ kind, children, pulse }: { kind?: string; children: React.ReactNode; pulse?: boolean }) {
  return <span className={'badge ' + (kind || '') + (pulse ? ' pulse' : '')}>{children}</span>
}

export function StatusBadge({ status }: { status: string }) {
  return (
    <Badge kind={status}>
      {status === 'running' && <Spinner />}
      {status}
    </Badge>
  )
}

/// The same status, one glyph wide.
///
/// For tables on a phone, where a word-shaped badge in every row is most of the
/// horizontal space. The glyph carries the colour and a `title`, and the status
/// word is still there for a screen reader — an icon-only control that says
/// nothing is worse than a truncated one.
export function StatusMark({ status }: { status: string }) {
  const glyph = status === 'succeeded' ? '✓' : status === 'failed' ? '✕' : status === 'cancelled' ? '⊘' : '●'
  return (
    <span className={'status-mark ' + status} title={status} aria-label={status} role="img">
      {glyph}
    </span>
  )
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <div className="field">
      <label>{label}</label>
      {children}
      {hint && <div className="hint">{hint}</div>}
    </div>
  )
}

/// The dimmed, blurred sheet. Portaled to `document.body` so a parent
/// transform or overflow (the results pane) cannot trap `position: fixed`.
export function ModalBackdrop({
  children,
  className = '',
  onClick,
}: {
  children: React.ReactNode
  className?: string
  onClick?: () => void
}) {
  // Close only on a real click on the backdrop: pressed *and* released on it.
  // A drag that starts inside the dialog — selecting text in an input, say —
  // and ends outside it still fires `click` on the backdrop (the nearest
  // common ancestor), which used to close the dialog mid-selection.
  const pressedHere = useRef(false)
  return createPortal(
    <div
      className={'modal-bg' + (className ? ' ' + className : '')}
      onPointerDown={(e) => {
        pressedHere.current = e.target === e.currentTarget
      }}
      onClick={(e) => {
        const real = pressedHere.current && e.target === e.currentTarget
        pressedHere.current = false
        if (real) onClick?.()
      }}
    >
      {children}
    </div>,
    document.body,
  )
}

export function Modal({ title, onClose, children, className = '' }: { title: string; onClose: () => void; children: React.ReactNode; className?: string }) {
  useEffect(() => {
    const fn = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', fn)
    return () => window.removeEventListener('keydown', fn)
  }, [onClose])
  return (
    <ModalBackdrop onClick={onClose}>
      <div className={'modal' + (className ? ' ' + className : '')} onClick={(e) => e.stopPropagation()}>
        <div className="row between" style={{ marginBottom: '0.8rem' }}>
          <h2 style={{ margin: 0 }}>{title}</h2>
          <button className="iconbtn" onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        {children}
      </div>
    </ModalBackdrop>
  )
}

export function Empty({ title, icon, children }: { title: string; icon?: React.ReactNode; children?: React.ReactNode }) {
  return (
    <div className="empty">
      <div className="glyph" aria-hidden>
        {icon || <SearchIcon size={38} />}
      </div>
      <h3>{title}</h3>
      {children}
    </div>
  )
}

export function Spinner() {
  return <span className="spinner" />
}

/// A region that is not ready to show yet. Used for a grid, a picker, a page
/// — anywhere the alternative would be an empty state that is a lie.
/// Something is loading. Inline (a count, a button) it is a spinner; as a
/// block it is a skeleton of rows, the shape most of what loads here takes.
export function Loading({ inline }: { inline?: boolean }) {
  if (!inline) {
    return (
      <div className="loading-skel" aria-busy="true" aria-label="Loading">
        <SkeletonRows />
      </div>
    )
  }
  return (
    <div className="loading-inline" aria-busy="true" aria-live="polite">
      <Spinner />
    </div>
  )
}

/// What the home ask box says while the agent writes the search. Single verbs,
/// the way Claude's status line does, held long enough to read.
const BUILDING_PHRASES = [
  'Thinking…',
  'Transmuting…',
  'Building…',
  'Creating…',
  'Pondering…',
  'Distilling…',
  'Weaving…',
  'Composing…',
  'Conjuring…',
  'Crystallizing…',
  'Sculpting…',
  'Almost there…',
]

const BUILDING_TICK_MS = 5000

/// A new verb every couple of seconds, looping, for as long as `active`.
export function useBuildingPhrase(active: boolean): string {
  const [tick, setTick] = useState(0)
  const start = useRef(0)
  useEffect(() => {
    if (!active) {
      setTick(0)
      return
    }
    start.current = Math.floor(Math.random() * BUILDING_PHRASES.length)
    setTick(0)
    const t = setInterval(() => setTick((n) => n + 1), BUILDING_TICK_MS)
    return () => clearInterval(t)
  }, [active])
  if (!active) return ''
  return BUILDING_PHRASES[(start.current + tick) % BUILDING_PHRASES.length]
}

export function BuildingPhrase({ text }: { text: string }) {
  return (
    <span key={text} className="ask-building" aria-live="polite">
      {text}
    </span>
  )
}

/// A copy icon for whatever it sits beside, shown when that thing is hovered
/// (always, on a touch screen). Put it inside an element with the `copyable`
/// class. `text` is copied, or — when it is a function — what it returns at
/// click time (handy for reading an element's own text). It becomes a tick
/// for a moment once copied, and says so in a toast.
export function CopyHover({ text, what, className = '' }: { text: string | (() => string); what: string; className?: string }) {
  const toast = useToast()
  const [done, setDone] = useState(false)
  useEffect(() => {
    if (!done) return
    const t = setTimeout(() => setDone(false), 1500)
    return () => clearTimeout(t)
  }, [done])
  if (typeof text === 'string' && !text.trim()) return null
  return (
    <button
      type="button"
      className={'copy-hover' + (done ? ' done' : '') + (className ? ' ' + className : '')}
      aria-label={`Copy ${what.toLowerCase()}`}
      title={`Copy ${what.toLowerCase()}`}
      onClick={async (e) => {
        e.stopPropagation()
        const value = (typeof text === 'function' ? text() : text).trim()
        if (!value) return
        try {
          await navigator.clipboard.writeText(value)
          setDone(true)
          toast(`${what} copied`)
        } catch {
          toast('Copy failed — your browser blocked the clipboard', true)
        }
      }}
    >
      {done ? <TickIcon size={15} /> : <CopyIcon size={15} />}
    </button>
  )
}

export function Copy({ text }: { text: string }) {
  const toast = useToast()
  return (
    <div className="copy">
      <span>{text}</span>
      <button
        className="btn sm"
        onClick={() => {
          navigator.clipboard?.writeText(text)
          toast('Copied')
        }}
      >
        Copy
      </button>
    </div>
  )
}

export type ConfirmAsk = {
  title?: string
  body: string
  confirm?: string
  danger?: boolean
}

type ConfirmFn = (ask: ConfirmAsk | string) => Promise<boolean>

const ConfirmCtx = createContext<ConfirmFn>(async () => false)

/// In-app confirm. `window.confirm` is silent in some browsers (and in the
/// embedded preview), which made Delete look like it did nothing.
export function ConfirmProvider({ children }: { children: React.ReactNode }) {
  const [job, setJob] = useState<{ ask: ConfirmAsk; resolve: (ok: boolean) => void } | null>(null)
  const ask = useCallback<ConfirmFn>((input) => {
    const next: ConfirmAsk = typeof input === 'string' ? { body: input } : input
    return new Promise((resolve) => {
      setJob((prev) => {
        prev?.resolve(false)
        return { ask: next, resolve }
      })
    })
  }, [])
  const finish = (ok: boolean) => {
    job?.resolve(ok)
    setJob(null)
  }
  return (
    <ConfirmCtx.Provider value={ask}>
      {children}
      {job && (
        <Modal title={job.ask.title || 'Are you sure?'} onClose={() => finish(false)}>
          <p className="muted" style={{ margin: '0 0 1.1rem' }}>
            {job.ask.body}
          </p>
          <div className="row" style={{ justifyContent: 'flex-end' }}>
            <button className="btn ghost" type="button" onClick={() => finish(false)}>
              Cancel
            </button>
            <button
              className={'btn ' + (job.ask.danger === false ? 'primary' : 'danger')}
              type="button"
              onClick={() => finish(true)}
            >
              {job.ask.confirm || 'Delete'}
            </button>
          </div>
        </Modal>
      )}
    </ConfirmCtx.Provider>
  )
}

export function useConfirm() {
  return useContext(ConfirmCtx)
}

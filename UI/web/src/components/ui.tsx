import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import { SearchIcon } from './icons'

// ---- toasts ----

interface Toast {
  id: number
  text: string
  bad?: boolean
}
const ToastCtx = createContext<(text: string, bad?: boolean) => void>(() => {})

export function ToastProvider({ children }: { children: React.ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([])
  const push = useCallback((text: string, bad?: boolean) => {
    const id = Date.now() + Math.random()
    setToasts((t) => [...t, { id, text, bad }])
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), bad ? 6000 : 3500)
  }, [])
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="toast-wrap">
        {toasts.map((t) => (
          <div key={t.id} className={'toast' + (t.bad ? ' bad' : '')}>
            {t.text}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
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

export function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: React.ReactNode }) {
  useEffect(() => {
    const fn = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', fn)
    return () => window.removeEventListener('keydown', fn)
  }, [onClose])
  return (
    <div className="modal-bg" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="row between" style={{ marginBottom: '0.8rem' }}>
          <h2 style={{ margin: 0 }}>{title}</h2>
          <button className="iconbtn" onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
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

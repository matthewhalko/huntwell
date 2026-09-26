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

export type PlanAskCol = { name: string; prompt: string }

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
    columns: cols.filter((c) => c.name.trim()),
    sites: sites.join(', '),
    kind,
  })
  return { name, setName, target, setTarget, effort, setEffort, kind, setKind, cols, setCols, sites, setSites, kindOptions, body }
}

export function PlanAskOptions({ ask }: { ask: ReturnType<typeof usePlanAsk> }) {
  const { name, setName, target, setTarget, effort, setEffort, kind, setKind, cols, setCols, sites, setSites, kindOptions } = ask
  const [siteDraft, setSiteDraft] = useState('')
  const setCol = (i: number, patch: Partial<PlanAskCol>) => setCols((cs) => cs.map((c, n) => (n === i ? { ...c, ...patch } : c)))
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
          {cols.length === 0 && (
            <p className="muted sm cols-hint">Add columns to collect a table with exactly the fields you want.</p>
          )}
          {cols.map((c, i) => (
            <div className="col-row" key={i}>
              <input
                value={c.name}
                onChange={(e) => setCol(i, { name: e.target.value })}
                placeholder="Column name"
                aria-label={`Column ${i + 1} name`}
              />
              <input
                value={c.prompt}
                onChange={(e) => setCol(i, { prompt: e.target.value })}
                placeholder="What goes in it (optional)"
                aria-label={`Column ${i + 1} contents`}
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
            </div>
          ))}
          <button type="button" className="btn sm" onClick={() => setCols((cs) => [...cs, { name: '', prompt: '' }])}>
            + Add column
          </button>
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

import React, { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { api, Billing, Effort, EFFORTS, Plan, PlanSummary } from '../api'
import { useAuth } from '../auth'
import { BuildingPhrase, Spinner, useBuildingPhrase, useToast } from '../components/ui'
import { CardDialog } from '../components/CardDialog'
import { Picker } from '../components/Picker'
import Setup from '../components/Setup'
import { ClockIcon } from '../components/icons'

const EXAMPLES = [
  'Small-batch hot sauce makers in the South who list a wholesale email',
  'A report on who is buying farmland along the Platte River',
  'Vintage analog synths for sale in the US under $800',
  'Climbing gyms that opened this year — owner or manager, and a booking link',
  'Municipal budget PDFs from Vermont towns under 20,000 people',
  'Board-game cafés in the Pacific Northwest hiring a weekend manager',
  'Independent bicycle frame builders who take custom orders',
]

const TAGLINES = [
  'What are we hunting today?',
  "Describe it — we'll go find it.",
  'The web is big. Bring back just the good part.',
  'Ask for a list. Get a list.',
]

/// Polls a freshly created plan until the agent has finished writing it.
/// Gives up after three minutes — the drafting call itself times out long
/// before that, so this only ever fires if the server went away.
async function waitForPlan(planId: number): Promise<Plan> {
  for (let i = 0; i < 90; i++) {
    const p = await api.get<Plan>(`/api/plans/${planId}`)
    if (p.Status !== 'drafting') return p
    await new Promise((r) => setTimeout(r, 2000))
  }
  throw new Error('Still building — check the search plans page in a moment.')
}

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

export default function Dashboard() {
  const { me } = useAuth()
  const nav = useNavigate()
  const toast = useToast()
  const [plans, setPlans] = useState<PlanSummary[]>([])
  // Returning from billing carries the prompt that was parked when the card
  // gate fired, so nobody retypes it.
  const parked = (useLocation() as { state?: { icp?: string } }).state?.icp || ''
  const [q, setQ] = useState(parked)
  const [busy, setBusy] = useState(false)
  const building = useBuildingPhrase(busy)
  // Raised by Go now when there is nothing to bill; cleared by the dialog,
  // which then starts the run that was interrupted.
  const [needCard, setNeedCard] = useState(false)
  // The welcome. Raised by Go now / Build plan on an account that has not been
  // through setup — never on arrival — and it carries on with whatever it
  // interrupted. Holds the action to resume, or null when closed.
  const [setup, setSetup] = useState<null | (() => void)>(null)
  // The same three choices the new-plan page offers, folded away until asked
  // for: most searches want none of them.
  const [adv, setAdv] = useState(false)
  const [name, setName] = useState('')
  const [target, setTarget] = useState(10)
  const [effort, setEffort] = useState<Effort>('normal')
  // What to come back with. 'auto' reads it off the brief, which is right
  // almost always; the picker is for the times it guesses wrong.
  const [kind, setKind] = useState('auto')
  // Only what this workspace may build. Reports and files are behind a flag,
  // so offering them to everyone would be offering an error message.
  const allowedKinds = me?.kinds
  const kindOptions = React.useMemo(
    () => (allowedKinds?.length ? KIND_OPTIONS.filter((o) => o.value === 'auto' || allowedKinds.includes(o.value)) : KIND_OPTIONS),
    [allowedKinds?.join(',')],
  )
  // Columns for a database plan. Naming even one says "I want a table with
  // exactly these columns", and the plan is built to fill them.
  const [cols, setCols] = useState<{ name: string; prompt: string }[]>([])
  const setCol = (i: number, patch: Partial<{ name: string; prompt: string }>) =>
    setCols((cs) => cs.map((c, n) => (n === i ? { ...c, ...patch } : c)))
  // Websites to search first. Chips rather than a text field: a list you can
  // see and remove one at a time, since a typo here quietly costs a run.
  const [sites, setSites] = useState<string[]>([])
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
  const [tagline] = useState(() => TAGLINES[Math.floor(Math.random() * TAGLINES.length)])
  const [ph, setPh] = useState('')

  const loadPlans = () => api.get<PlanSummary[]>('/api/plans').then(setPlans).catch(() => {})
  useEffect(() => {
    loadPlans()
  }, [])

  // Typewriter placeholder: the box quietly demos what you can ask it.
  useEffect(() => {
    let ex = Math.floor(Math.random() * EXAMPLES.length)
    let ch = 0
    let hold = 0
    let deleting = false
    const t = setInterval(() => {
      if (hold > 0) {
        hold--
        return
      }
      const s = EXAMPLES[ex]
      if (!deleting) {
        ch++
        if (ch >= s.length) {
          deleting = true
          hold = 40 // linger so it can be read
        }
      } else {
        ch = Math.max(0, ch - 3)
        if (ch === 0) {
          deleting = false
          ex = (ex + 1) % EXAMPLES.length
          hold = 8
        }
      }
      setPh(s.slice(0, ch))
    }, 45)
    return () => clearInterval(t)
  }, [])

  // "Go now": the target is inferred from the prompt and the plan is drafted.
  // The plan page then asks them to Run now — starting here would skip that.
  const startSearch = async () => {
    const icp = q.trim()
    if (!icp) return
    // A run bills the account, so it needs a card. The server refuses this
    // anyway (web::runner::start); asking here means the card field opens over
    // what they were doing, and the search they typed carries on afterwards.
    try {
      const b = await api.get<Billing>('/api/billing')
      if (!b.has_card) {
        setNeedCard(true)
        return
      }
    } catch {
      // Billing unreachable is not a reason to block the attempt; the server
      // gate is the one that counts.
    }
    setBusy(true)
    try {
      // The plan row is created at once — it is already visible, spinning, on
      // the plans page — and the agent writes the search behind it. Wait for
      // that before starting the run it is for.
      const saved = await api.post<Plan>('/api/plans', {
        brief: icp,
        name: name.trim(),
        target,
        effort,
        columns: cols.filter((c) => c.name.trim()),
        sites: sites.join(', '),
        kind,
      })
      const ready = await waitForPlan(saved.PlanId)
      if (ready.Status === 'failed') {
        throw new Error("That search couldn't be built. Try describing it a different way.")
      }
      nav(`/app/plans/${saved.PlanId}`, { state: { welcome: true } })
    } catch (err: any) {
      toast(err.message || 'Could not build that search', true)
    } finally {
      setBusy(false)
    }
  }
  // The two entry points share one gate: an account that has not been through
  // setup sees the welcome first, and what it interrupted runs afterwards.
  // `startSearch`, not `goNow`, is what resumes — `goNow` closes over the `me`
  // of this render, which is still un-onboarded and would reopen the dialog.
  const goNow = (e?: React.FormEvent) => {
    e?.preventDefault()
    if (!q.trim() || busy) return
    if (me && !me.onboarded) {
      setSetup(() => startSearch)
      return
    }
    startSearch()
  }
  // "Build plan": open the wizard prefilled to specify target and parameters.
  const buildPlan = () => {
    const icp = q.trim()
    if (!icp || busy) return
    if (me && !me.onboarded) {
      setSetup(() => () => nav('/app/plans/new', { state: { icp } }))
      return
    }
    nav('/app/plans/new', { state: { icp } })
  }
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      goNow()
    }
  }

  const hour = new Date().getHours()
  const greet = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'

  return (
    <>
      <div className="ask-hero">
        <h1>
          {greet}, <span className="grad-text">{me?.display_name || 'there'}</span>.
        </h1>
        <p className="sub">{tagline}</p>

        <form className={'ask' + (busy ? ' busy' : '')} onSubmit={goNow}>
          <textarea
            className="ask-input"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={onKey}
            rows={3}
            placeholder={ph}
            autoFocus
          />
          <div className="ask-actions">
            <span className="muted sm">
              {building ? <BuildingPhrase text={building} /> : <span className="ask-hint">Enter to go · Shift+Enter for a new line</span>}
              {!busy && (
                <button type="button" className="linkish" onClick={() => setAdv(!adv)}>
                  {adv ? 'Hide options' : 'Options'}
                </button>
              )}
            </span>
            <div className="row" style={{ gap: '0.5rem' }}>
              <button className="btn" type="button" disabled={!q.trim() || busy} onClick={buildPlan} title="Open the wizard to pick the target and tune every parameter">
                Build plan
              </button>
              <button className="btn primary" type="submit" disabled={!q.trim() || busy} title="Infer the target and build the search plan">
                {busy ? (
                  <>
                    <Spinner /> {building}
                  </>
                ) : (
                  'Go now →'
                )}
              </button>
            </div>
          </div>
          {adv && (
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
              {/* Columns turn the answer into a table you defined. Left empty,
                  Huntwell works out the shape of the results itself. */}
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
              {/* Where to look. A preference, not a fence: the run can still go
                  elsewhere, which is why this does not touch the allowlist. */}
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
          )}
        </form>

        {plans.length > 0 && (
          <div className="ask-examples">
            {plans.slice(0, 6).map((p) => (
              <button key={p.PlanId} className="chip" onClick={() => nav(`/app/plans/${p.PlanId}`, { state: { tab: 'results' } })} title={p.Source}>
                <ClockIcon size={12} /> {p.Source.length > 42 ? p.Source.slice(0, 42) + '…' : p.Source}
              </button>
            ))}
          </div>
        )}
      </div>

      {setup && <Setup resume={setup} onClose={() => setSetup(null)} />}

      {needCard && (
        <CardDialog
          reason="Executions cost money, so we need a card before the first one. Add it here and your search starts straight away."
          onClose={() => setNeedCard(false)}
          onDone={() => {
            setNeedCard(false)
            // Pick up exactly where the gate interrupted.
            goNow()
          }}
        />
      )}
    </>
  )
}

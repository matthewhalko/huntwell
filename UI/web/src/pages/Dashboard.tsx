import React, { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { api, Billing, can, Plan, PlanSummary } from '../api'
import { useAuth } from '../auth'
import { BuildingPhrase, Modal, Spinner, useBuildingPhrase, useToast } from '../components/ui'
import { CardDialog } from '../components/CardDialog'
import { PlanAskOptions, usePlanAsk } from '../components/PlanAskOptions'
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
  // Why the card is being asked for: a first search, or free credit used up.
  const [freeSpent, setFreeSpent] = useState(false)
  // The welcome. Raised by Go now / Build plan on an account that has not been
  // through setup — never on arrival — and it carries on with whatever it
  // interrupted. Holds the action to resume, or null when closed.
  const [setup, setSetup] = useState<null | (() => void)>(null)
  // Same options as New search plan, folded away until asked for.
  const [adv, setAdv] = useState(false)
  const [recentOpen, setRecentOpen] = useState(false)
  const ask = usePlanAsk()
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
      // Free credit an operator added runs without a card; the card is only
      // needed once that is spent (the server holds the same rule).
      if (b.card_required ?? !b.has_card) {
        setFreeSpent(!!b.free_credit_spent)
        if (!can(me, 'credits')) {
          toast('Ask an admin to add a payment method before you can start a search.', true)
          return
        }
        setNeedCard(true)
        return
      }
      if (b.has_credits === false) {
        if (!can(me, 'credits')) {
          toast('This workspace needs more credits. Ask an admin to buy some.', true)
          return
        }
        nav('/app/usage', { state: { needCredits: true, icp } })
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
      const saved = await api.post<Plan>('/api/plans', ask.body(icp))
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

        {can(me, 'plans') ? (
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
                <>
                  <button type="button" className="linkish" onClick={() => setAdv(!adv)}>
                    {adv ? 'Hide options' : 'Options'}
                  </button>
                  {plans.length > 0 && (
                    <button type="button" className="linkish ask-recent" onClick={() => setRecentOpen(true)}>
                      Recent
                    </button>
                  )}
                </>
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
          {adv && <PlanAskOptions ask={ask} />}
        </form>
        ) : (
          <p className="muted" style={{ margin: '0.8rem 0 0' }}>
            This workspace is read-only for you. Open a search plan to see what it found, or ask an admin if you need to start a new one.
          </p>
        )}

        {plans.length > 0 && (
          <div className="ask-examples">
            <div className="ask-examples-chips">
              {plans.slice(0, 6).map((p) => (
                <button key={p.PlanId} className="chip" onClick={() => nav(`/app/plans/${p.PlanId}`, { state: { tab: 'results' } })} title={p.Source}>
                  <ClockIcon size={12} /> {p.Source.length > 42 ? p.Source.slice(0, 42) + '…' : p.Source}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {recentOpen && (
        <Modal title="Recent searches" onClose={() => setRecentOpen(false)}>
          <div className="ask-recent-list">
            {[...plans]
              .sort((a, b) => +new Date(b.UpdatedAt) - +new Date(a.UpdatedAt))
              .slice(0, 20)
              .map((p) => (
                <button
                  key={p.PlanId}
                  type="button"
                  className="ask-recent-item"
                  onClick={() => {
                    setQ((p.Description || p.Source).trim())
                    setRecentOpen(false)
                  }}
                >
                  <b>{p.Source}</b>
                  {p.Description && p.Description !== p.Source && <span className="muted">{p.Description}</span>}
                </button>
              ))}
          </div>
        </Modal>
      )}

      {setup && <Setup resume={setup} onClose={() => setSetup(null)} />}

      {needCard && (
        <CardDialog
          reason={
            freeSpent
              ? "Your free credit is used up. Add a card to keep searching — then buy credits as you need them. Your search starts as soon as it's added."
              : 'Executions cost money, so we need a card before the first one. Add it here and your search starts straight away.'
          }
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

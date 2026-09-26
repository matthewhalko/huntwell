import React, { useEffect, useState } from 'react'
import { Link, useLocation, useNavigate, useSearchParams } from 'react-router-dom'
import { ago, api, Billing, can, fmtDate, fmtTokens, money, Overview, Prospect, Run, Usage as UsageT, usd } from '../api'
import { useAuth } from '../auth'
import { StatusBadge, useConfirm, useToast } from '../components/ui'
import { CardBrandIcon, cardBrandLabel } from '../components/icons'
import { CardDialog } from '../components/CardDialog'
import { ResultDialog } from '../components/ResultDialog'
import { ChartUnit, TokenChart } from '../components/TokenChart'

export default function Usage() {
  const { me } = useAuth()
  const pay = can(me, 'credits')
  const write = can(me, 'plans')
  const [data, setData] = useState<Overview | null>(null)
  const [usage, setUsage] = useState<UsageT | null>(null)
  const [billing, setBilling] = useState<Billing | null>(null)
  const [runs, setRuns] = useState<Run[] | null>(null)
  const [chartUnit, setChartUnit] = useState<ChartUnit>('tokens')
  const [lookback, setLookback] = useState(100)
  const [lookbackText, setLookbackText] = useState('100')
  const [cardBusy, setCardBusy] = useState(false)
  const [adding, setAdding] = useState(false)
  const [open, setOpen] = useState<Prospect | null>(null)
  const [params, setParams] = useSearchParams()
  const loc = useLocation() as { state?: { needCard?: boolean; needCredits?: boolean; icp?: string } }
  const nav = useNavigate()
  const toast = useToast()
  const confirm = useConfirm()
  // Sent here by "Go now" with nothing to bill: say so, and keep the prompt so
  // the trip back to Home is one click and no retyping.
  const needCard = !!loc.state?.needCard
  const needCredits = !!loc.state?.needCredits
  const parkedIcp = loc.state?.icp || ''

  const load = async () => {
    const [o, u, b, r] = await Promise.all([
      api.get<Overview>('/api/overview'),
      api.get<UsageT>('/api/usage'),
      api.get<Billing>('/api/billing'),
      api.get<Run[]>('/api/executions?limit=200'),
    ])
    setData(o)
    setUsage(u)
    setBilling(b)
    setRuns(r)
  }

  /// The card field opens here too — same dialog the Go now gate raises.

  const removeCard = async () => {
    setCardBusy(true)
    try {
      await api.del('/api/billing/card')
      await load()
    } catch (e: any) {
      toast(e.message || 'Could not remove the card', true)
    } finally {
      setCardBusy(false)
    }
  }
  const PACKS = [10, 25, 50, 100]
  const [buying, setBuying] = useState<number | null>(null)
  const buy = async (usd: number) => {
    if (!billing?.has_card) {
      setAdding(true)
      return
    }
    // Charge is immediate on the card on file — say so before it happens.
    const card = billing.card
    const onCard = card ? ` on the ${cardBrandLabel(card.brand)} ending ${card.last4}` : ' on the card on file'
    if (
      !(await confirm({
        title: `Buy $${usd} of credits?`,
        body: `We'll charge $${usd}${onCard}. Credits are prepaid and land as soon as the charge succeeds.`,
        confirm: `Buy $${usd}`,
        danger: false,
      }))
    )
      return
    setBuying(usd)
    try {
      const purchase_id =
        globalThis.crypto?.randomUUID?.() ||
        `${Date.now().toString(36)}_${Math.random().toString(36).slice(2)}`
      const r = await api.post<{ requires_action?: boolean; credits_usd?: number }>('/api/billing/credits', {
        usd,
        purchase_id,
      })
      if (r.requires_action) {
        toast('That card needs another check — remove it and add it again, then retry.', true)
        return
      }
      toast(`Purchase confirmed — $${usd} in credits added.`)
      await load()
    } catch (e: any) {
      toast(e.message || 'Could not buy credits', true)
    } finally {
      setBuying(null)
    }
  }
  useEffect(() => {
    load()
    const t = setInterval(load, 10000)
    return () => clearInterval(t)
  }, [])

  // Coming back from Stripe Checkout: the session id in the URL is what we ask
  // Stripe about, and it is dropped from the address bar either way.
  useEffect(() => {
    const setup = params.get('setup')
    if (!setup) return
    params.delete('setup')
    setParams(params, { replace: true })
    if (setup === 'cancelled') {
      toast('Card setup cancelled', true)
      return
    }
    api
      .post('/api/billing/confirm', { session_id: setup })
      .then(() => {
        toast('Card saved')
        load()
      })
      .catch((e: any) => toast(e.message || 'Could not confirm the card', true))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const o = data?.overview

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Usage &amp; activity</h1>
          <div className="sub">Prepaid credits, live token spend, and what your search plans have found.</div>
        </div>
      </div>

      {(needCard || needCredits) && (
        <div className="card" style={{ marginBottom: '1.4rem', borderColor: 'var(--warn)' }}>
          <h3 style={{ margin: 0 }}>{needCard && !billing?.has_card ? 'Add a card to start hunting' : 'Buy credits to start hunting'}</h3>
          <p className="muted" style={{ margin: '0.35rem 0 0' }}>
            {needCard && !billing?.has_card
              ? 'Executions cost money, so we need a payment method and prepaid credits before the first one.'
              : 'A card is on file, but there are no prepaid credits left. Buy some below — a run cannot spend past what you have purchased.'}
            {parkedIcp && ' Your search is saved — we will take you back to it.'}
          </p>
        </div>
      )}

      <div className="card" style={{ marginBottom: '1.4rem' }}>
        <div className="row between">
          <h3 style={{ margin: 0 }}>Payment method</h3>
          {pay &&
            (billing?.card ? (
              <button className="btn" onClick={removeCard} disabled={cardBusy}>
                Remove
              </button>
            ) : (
              <button className="btn primary" onClick={() => setAdding(true)}>
                Add payment method
              </button>
            ))}
        </div>
        {billing?.card ? (
          <div className="pay-method">
            <CardBrandIcon brand={billing.card.brand} />
            <div className="pay-method-meta">
              <div className="pay-method-line">
                <span style={{ fontWeight: 700 }}>{cardBrandLabel(billing.card.brand)}</span>
                <span className="muted">•••• {billing.card.last4}</span>
              </div>
              {billing.card.added_at && <span className="muted sm">added {fmtDate(billing.card.added_at)}</span>}
            </div>
          </div>
        ) : (
          <p className="muted" style={{ margin: '0.35rem 0 0' }}>
            No card on file — runs are paused until one is added.
            {billing?.production && !billing.stripe
              ? ' Stripe is not configured on this server, so a card cannot be added yet.'
              : billing && !billing.stripe
                ? ' Stripe is not configured here, so this will attach a test card.'
                : ''}
          </p>
        )}
        {billing?.has_card && (billing.has_credits || (usage && usage.credits_usd > 0)) && parkedIcp && (
          <button className="btn primary" style={{ marginTop: '0.9rem' }} onClick={() => nav('/app', { state: { icp: parkedIcp } })}>
            Back to your search →
          </button>
        )}
      </div>

      {adding && pay && (
        <CardDialog
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false)
            load()
            toast('Card saved')
          }}
        />
      )}

      {usage && (() => {
        const credits = usage.credits_usd ?? usage.remaining_usd
        const empty = credits <= 0
        return (
          <div className="card" style={{ marginBottom: '1.4rem' }}>
            <div className="row between" style={{ marginBottom: '0.5rem' }}>
              <h3 style={{ margin: 0 }}>Prepaid credits</h3>
              <span className={empty ? 'danger' : 'muted'}>
                {usd(credits)} available · {fmtTokens(usage.tokens_used)} tokens this period
              </span>
            </div>
            <p className="muted" style={{ margin: '0 0 0.7rem' }}>
              {empty
                ? 'No credits left — runs are paused until you buy more. A running job stops the moment this hits zero.'
                : `${usd(usage.used_usd)} spent this period. Credits are purchased in advance and cannot go below zero.`}
            </p>
            {pay && (
              <div className="row" style={{ gap: '0.5rem', flexWrap: 'wrap' }}>
                {PACKS.map((n) => (
                  <button key={n} className="btn" disabled={buying !== null} onClick={() => buy(n)}>
                    {buying === n ? 'Buying…' : `Buy $${n}`}
                  </button>
                ))}
              </div>
            )}
          </div>
        )
      })()}

      {runs && runs.length > 0 && (
        <div className="card" style={{ marginBottom: '1.4rem' }}>
          <div className="row between" style={{ marginBottom: '0.55rem', gap: '0.6rem', flexWrap: 'wrap' }}>
            <div className="row" style={{ gap: '0.7rem' }}>
              <h3 style={{ margin: 0 }}>{chartUnit === 'usd' ? 'Dollars per execution' : 'Tokens per execution'}</h3>
              <div className="seg small" role="group" aria-label="Chart units">
                <button type="button" className={chartUnit === 'tokens' ? 'active' : ''} onClick={() => setChartUnit('tokens')}>
                  Tokens
                </button>
                <button type="button" className={chartUnit === 'usd' ? 'active' : ''} onClick={() => setChartUnit('usd')}>
                  Dollars
                </button>
              </div>
              <label className="chart-lookback">
                Last
                <input
                  type="number"
                  min={10}
                  max={200}
                  step={10}
                  value={lookbackText}
                  onChange={(e) => {
                    const t = e.target.value
                    setLookbackText(t)
                    const n = Number(t)
                    if (Number.isFinite(n) && n >= 10 && n <= 200) setLookback(Math.round(n))
                  }}
                  onBlur={() => {
                    const n = Math.min(200, Math.max(10, Number(lookbackText) || 100))
                    setLookback(n)
                    setLookbackText(String(n))
                  }}
                />
                executions
              </label>
            </div>
            <span className="muted">
              {(() => {
                const shown = [...runs].sort((a, b) => a.execution_id - b.execution_id).slice(-lookback)
                const total = shown.reduce((a, r) => a + (chartUnit === 'usd' ? r.cost_usd || 0 : r.tokens || 0), 0)
                const amount = chartUnit === 'usd' ? usd(total) : `${fmtTokens(total)} tokens`
                return `${shown.length} run${shown.length === 1 ? '' : 's'} · ${amount}`
              })()}
            </span>
          </div>
          <TokenChart runs={runs} unit={chartUnit} lookback={lookback} />
        </div>
      )}

      <div className="grid cols-4" style={{ marginBottom: '1.4rem' }}>
        <div className="stat">
          <div className="n">{o?.prospects ?? '—'}</div>
          <div className="l">artifacts stored</div>
        </div>
        <div className="stat">
          <div className="n">{o?.prospects_7d ?? '—'}</div>
          <div className="l">new in the last 7 days</div>
        </div>
        <div className="stat">
          <div className="n">{o?.plans ?? '—'}</div>
          <div className="l">plans</div>
        </div>
        <div className="stat">
          <div className="n">{o?.active_executions ?? '—'}</div>
          <div className="l">running now · {o?.executions ?? 0} total executions</div>
        </div>
      </div>

      <div className="two">
        <div className="card">
          <div className="row between">
            <h3 style={{ margin: 0 }}>Recent executions</h3>
            <Link to="/app/executions">All →</Link>
          </div>
          {data?.recent_executions.length ? (
            <table className="recent-runs" style={{ marginTop: '0.6rem' }}>
              <tbody>
                {data.recent_executions.map((r) => (
                  <tr
                    key={r.execution_id}
                    className="clickable"
                    onClick={() => nav(`/app/executions/${r.execution_id}`)}
                  >
                    <td>
                      <Link to={`/app/executions/${r.execution_id}`} onClick={(e) => e.stopPropagation()}>
                        #{r.execution_id}
                      </Link>{' '}
                      <span className="muted">{r.source}</span>
                    </td>
                    <td className="status-cell">
                      <StatusBadge status={r.status} />
                    </td>
                    <td className="num muted">{ago(r.started_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <p className="muted" style={{ marginTop: '0.5rem' }}>
              Nothing has run yet.
            </p>
          )}
        </div>
        <div className="card">
          <div className="row between">
            <h3 style={{ margin: 0 }}>Latest artifacts</h3>
            <Link to="/app/prospects">All →</Link>
          </div>
          {data?.latest_prospects.length ? (
            <table style={{ marginTop: '0.6rem' }}>
              <tbody>
                {data.latest_prospects.map((p) => (
                  <tr key={p.prospect_id} className="clickable" onClick={() => setOpen(p)}>
                    <td>
                      <b>{p.name || p.company}</b>
                      <br />
                      <span className="muted">{p.name ? p.company : p.website}</span>
                    </td>
                    <td className="num muted">{fmtDate(p.first_seen_utc)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <p className="muted" style={{ marginTop: '0.5rem' }}>
              Run a search plan and they'll show up here.
            </p>
          )}
        </div>
      </div>
      {open && (
        <ResultDialog
          title={open.name || open.company}
          fields={[
            ['Title', open.title],
            ['Company', open.company],
            ['Industry', open.industry],
            ['Email', open.email + (open.email_status ? ` (${open.email_status})` : '')],
            ['Phone', open.phone],
            ['Website', open.website],
            ['LinkedIn', open.linkedin],
            ['Location', open.location],
            ['Notes', open.notes],
            ['Value', money(open.estimated_value)],
            ['Plan', open.source],
            ['Key', open.source_key],
            ['First seen', fmtDate(open.first_seen_utc)],
          ]}
          onClose={() => setOpen(null)}
          onDelete={
            write
              ? async () => {
                  await api.del(`/api/prospects/${open.prospect_id}`)
                  setOpen(null)
                  toast('Deleted')
                  load()
                }
              : undefined
          }
        />
      )}
    </>
  )
}

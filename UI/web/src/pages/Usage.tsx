import React, { useEffect, useState } from 'react'
import { Link, useLocation, useNavigate, useSearchParams } from 'react-router-dom'
import { ago, api, Billing, fmtDate, fmtTokens, Overview, Usage as UsageT, usd } from '../api'
import { StatusBadge, useToast } from '../components/ui'
import { CardDialog } from '../components/CardDialog'

export default function Usage() {
  const [data, setData] = useState<Overview | null>(null)
  const [usage, setUsage] = useState<UsageT | null>(null)
  const [billing, setBilling] = useState<Billing | null>(null)
  const [cardBusy, setCardBusy] = useState(false)
  const [adding, setAdding] = useState(false)
  const [params, setParams] = useSearchParams()
  const loc = useLocation() as { state?: { needCard?: boolean; icp?: string } }
  const nav = useNavigate()
  const toast = useToast()
  // Sent here by "Go now" with nothing to bill: say so, and keep the prompt so
  // the trip back to Home is one click and no retyping.
  const needCard = !!loc.state?.needCard
  const parkedIcp = loc.state?.icp || ''

  const load = async () => {
    const [o, u, b] = await Promise.all([
      api.get<Overview>('/api/overview'),
      api.get<UsageT>('/api/usage'),
      api.get<Billing>('/api/billing'),
    ])
    setData(o)
    setUsage(u)
    setBilling(b)
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
  const topup = async () => {
    await api.post('/api/usage/topup', { usd: 25 })
    load()
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
          <div className="sub">Your token spend this month and what your search plans have found.</div>
        </div>
      </div>

      {needCard && !billing?.has_card && (
        <div className="card" style={{ marginBottom: '1.4rem', borderColor: 'var(--warn)' }}>
          <h3 style={{ margin: 0 }}>Add a card to start hunting</h3>
          <p className="muted" style={{ margin: '0.35rem 0 0' }}>
            Executions cost money, so we need a payment method before the first one.
            {parkedIcp && ' Your search is saved — add a card and we will take you back to it.'}
          </p>
        </div>
      )}

      <div className="card" style={{ marginBottom: '1.4rem' }}>
        <div className="row between">
          <h3 style={{ margin: 0 }}>Payment method</h3>
          {billing?.card ? (
            <button className="btn" onClick={removeCard} disabled={cardBusy}>
              Remove
            </button>
          ) : (
            <button className="btn primary" onClick={() => setAdding(true)}>
              Add payment method
            </button>
          )}
        </div>
        {billing?.card ? (
          <div className="row" style={{ marginTop: '0.5rem', gap: '0.6rem' }}>
            <span style={{ fontWeight: 700, textTransform: 'capitalize' }}>{billing.card.brand}</span>
            <span className="muted">•••• {billing.card.last4}</span>
            {billing.card.added_at && <span className="muted">· added {fmtDate(billing.card.added_at)}</span>}
          </div>
        ) : (
          <p className="muted" style={{ margin: '0.35rem 0 0' }}>
            No card on file — runs are paused until one is added.
            {billing && !billing.stripe && ' Stripe is not configured here, so this will attach a test card.'}
          </p>
        )}
        {billing?.has_card && parkedIcp && (
          <button className="btn primary" style={{ marginTop: '0.9rem' }} onClick={() => nav('/app', { state: { icp: parkedIcp } })}>
            Back to your search →
          </button>
        )}
      </div>

      {adding && (
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
        const pct = usage.available_usd > 0 ? Math.min(100, (usage.used_usd / usage.available_usd) * 100) : 0
        const over = usage.remaining_usd <= 0
        const color = over ? 'var(--bad)' : pct >= 80 ? 'var(--warn)' : 'var(--accent)'
        return (
          <div className="card" style={{ marginBottom: '1.4rem' }}>
            <div className="row between" style={{ marginBottom: '0.5rem' }}>
              <h3 style={{ margin: 0 }}>Usage this month</h3>
              <span className="muted">
                {usd(usage.used_usd)} / {usd(usage.available_usd)} · {fmtTokens(usage.tokens_used)} tokens
              </span>
            </div>
            <div className="meter">
              <div className="meter-fill" style={{ width: `${pct}%`, background: color }} />
            </div>
            <div className="row between" style={{ marginTop: '0.5rem' }}>
              <span className={over ? 'danger' : 'muted'}>
                {over ? 'Limit reached — runs are paused until you add credit.' : `${usd(usage.remaining_usd)} left this month`}
              </span>
              <button className="btn" onClick={topup}>
                Add $25
              </button>
            </div>
          </div>
        )
      })()}

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
            <table style={{ marginTop: '0.6rem' }}>
              <tbody>
                {data.recent_executions.map((r) => (
                  <tr key={r.execution_id}>
                    <td>
                      <Link to={`/app/executions/${r.execution_id}`}>#{r.execution_id}</Link> <span className="muted">{r.source}</span>
                    </td>
                    <td>
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
                  <tr key={p.prospect_id}>
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
    </>
  )
}

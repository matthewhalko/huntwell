import React, { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ago, api, usd, fmtDate, PlanSummary, scheduleLabel } from '../api'
import { Badge, Empty, Spinner, StatusBadge, StatusMark, useToast } from '../components/ui'
import { ClockIcon } from '../components/icons'

/// One plan as a table row.
///
/// A list of plans is scanned — which is running, which is costing money, which
/// found nothing last time — and a grid of cards makes every one of those a
/// separate hunt. Columns line the answers up.
function PlanRow({ plan, onChange }: { plan: PlanSummary; onChange: () => void }) {
  const nav = useNavigate()
  const toast = useToast()
  const run = async (e: React.MouseEvent) => {
    e.stopPropagation()
    try {
      const r = await api.post<{ execution_id: number }>('/api/executions', { plan_id: plan.PlanId, target: plan.TargetProspects || undefined })
      nav(`/app/executions/${r.execution_id}`)
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const star = async (e: React.MouseEvent) => {
    e.stopPropagation()
    await api.post(`/api/plans/${plan.PlanId}/favorite`, { favorite: !plan.Favorite })
    onChange()
  }
  return (
    <tr className="clickable" onClick={() => nav(`/app/plans/${plan.PlanId}`)}>
      {/* How it went last time, first thing on the row: it is what people scan
          this table for. Word on a desk, one glyph on a phone. */}
      <td className="col-status">
        {plan.last_execution_status ? (
          <>
            <span className="status-wide">
              <StatusBadge status={plan.last_execution_status} />
            </span>
            <span className="status-narrow">
              <StatusMark status={plan.last_execution_status} />
            </span>
          </>
        ) : (
          <span className="muted status-wide">never run</span>
        )}
      </td>
      <td className="col-star">
        <button className={'star' + (plan.Favorite ? ' on' : '')} onClick={star} title="Favourite">
          {plan.Favorite ? '★' : '☆'}
        </button>
      </td>
      <td>
        <div className="plan-name">{plan.Source}</div>
        {plan.Description && <div className="plan-desc">{plan.Description}</div>}
      </td>
      <td>
        {plan.Status === 'drafting' ? (
          <Badge kind="info" pulse>
            <Spinner /> building…
          </Badge>
        ) : plan.Status === 'failed' ? (
          <Badge kind="bad">couldn't be built</Badge>
        ) : (
          <Badge>{plan.Kind === 'report' || plan.Kind === 'assets' ? plan.Kind : 'artifacts'}</Badge>
        )}
        {plan.ScheduleEnabled && (
          <Badge kind="yellow">
            <ClockIcon size={12} /> {scheduleLabel(plan)}
          </Badge>
        )}
      </td>
      <td className="num">{plan.prospects}</td>
      <td className="num">{plan.executions}</td>
      <td className="num">{plan.spend_usd ? usd(plan.spend_usd) : '—'}</td>
      <td className="nowrap muted">{plan.last_execution_status ? ago(plan.last_execution_at) : 'never run'}</td>
      <td className="num nowrap">
        {plan.active_execution_id ? (
          <Link to={`/app/executions/${plan.active_execution_id}`} className="btn sm" onClick={(e) => e.stopPropagation()}>
            Watch
          </Link>
        ) : (
          <button className="btn sm" onClick={run} disabled={plan.Status !== 'ready'} title="Run now" aria-label="Run now">
            ▶ <span className="run-label">Run</span>
          </button>
        )}
      </td>
    </tr>
  )
}

export default function Plans() {
  const [plans, setPlans] = useState<PlanSummary[] | null>(null)
  const [q, setQ] = useState('')
  const load = () => api.get<PlanSummary[]>('/api/plans').then(setPlans)
  const building = (plans || []).some((p) => p.Status === 'drafting')
  useEffect(() => {
    load()
    // A plan still being written finishes in under a minute, so watch it at a
    // pace that shows it; settle back once nothing is in flight.
    const t = setInterval(load, building ? 2500 : 10000)
    return () => clearInterval(t)
  }, [building])
  const shown = (plans || []).filter((p) => p.Source.toLowerCase().includes(q.toLowerCase()))
  return (
    <>
      <div className="page-head">
        <div>
          <h1>Search plans</h1>
          <div className="sub">Active search plans you or your team have created.</div>
        </div>
        <div className="row">
          <input type="text" placeholder="Filter…" value={q} onChange={(e) => setQ(e.target.value)} className="head-filter" />
          <Link to="/app/plans/new" className="btn primary">
            + New search plan
          </Link>
        </div>
      </div>
      {plans === null ? (
        <p className="muted">Loading…</p>
      ) : shown.length === 0 ? (
        <Empty title={plans.length ? 'No search plans match' : 'No search plans yet'}>
          {!plans.length && (
            /* Home is where a plan is made — one box and a sentence. The
               wizard is for tuning one, not for starting from nothing. */
            <Link to="/app" className="btn primary">
              Create your first search plan
            </Link>
          )}
        </Empty>
      ) : (
        <div className="card pad0 table-wrap">
          <table className="plans-table">
            <thead>
              <tr>
                <th className="col-status">Last</th>
                <th className="col-star" />
                <th>Plan</th>
                <th>Collects</th>
                <th className="num">Artifacts</th>
                <th className="num">Executions</th>
                <th className="num">Spent</th>
                <th>Last run</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.map((p) => (
                <PlanRow key={p.PlanId} plan={p} onChange={load} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  )
}

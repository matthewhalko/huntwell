import React, { useEffect, useState } from 'react'
import { Link, useLocation, useNavigate, useParams } from 'react-router-dom'
import { ago, api, FieldSpec, fmtDate, Plan, Run, scheduleLabel, usd } from '../api'
import { Badge, Empty, Modal, Spinner, StatusBadge, useConfirm, useToast } from '../components/ui'
import { CheckIcon, ClockIcon } from '../components/icons'
import { SearchSeeds } from '../components/SearchSeeds'
import { ArtifactTable } from '../components/ArtifactTable'
import { KnowledgeGraph } from '../components/KnowledgeGraph'
import { ReportView } from '../components/ReportView'
import { AssetGrid } from '../components/AssetGrid'
import { planHasCustomModels, PlanModels } from '../components/PlanModels'
import { PlanSettings } from './PlanSettings'
import { ProspectTable } from './Prospects'

// Parse a plan's custom-artifact column schema (tolerant of malformed JSON).
export function parseSchema(json: string): FieldSpec[] {
  try {
    const v = JSON.parse(json || '[]')
    return Array.isArray(v) ? v.filter((f) => f && f.key) : []
  } catch {
    return []
  }
}

type Tab = 'overview' | 'results' | 'graph' | 'trail' | 'edit'

export default function PlanDetail() {
  const { id } = useParams()
  const nav = useNavigate()
  const toast = useToast()
  const confirm = useConfirm()
  const [plan, setPlan] = useState<Plan | null>(null)
  const [runs, setRuns] = useState<Run[]>([])
  const loc = useLocation()
  const locState = (loc.state || {}) as { tab?: Tab; welcome?: boolean }
  const [tab, setTab] = useState<Tab>(locState.tab || 'overview')
  // 'first' is the just-created welcome; 'run' is the header button. Same
  // dialog, different copy, so the first visit makes Run now the obvious next
  // step rather than a control they have to notice.
  const [runModal, setRunModal] = useState<'first' | 'run' | null>(null)
  const [advanced, setAdvanced] = useState(false)

  const load = async () => {
    try {
      const [p, r] = await Promise.all([api.get<Plan>(`/api/plans/${id}`), api.get<Run[]>(`/api/executions?plan_id=${id}&limit=50`)])
      setPlan(p)
      setRuns(r)
    } catch (e: any) {
      toast(e.message, true)
      nav('/app/plans')
    }
  }
  useEffect(() => {
    load()
  }, [id])
  const liveRun = runs.some((r) => r.status === 'running' || r.status === 'queued')
  useEffect(() => {
    const t = setInterval(
      () => api.get<Run[]>(`/api/executions?plan_id=${id}&limit=50`).then(setRuns).catch(() => {}),
      liveRun ? 2000 : 8000,
    )
    return () => clearInterval(t)
  }, [id, liveRun])
  // A plan still being written becomes a different page when it lands, so poll
  // the plan itself until it does.
  useEffect(() => {
    if (plan?.Status !== 'drafting') return
    const t = setInterval(() => api.get<Plan>(`/api/plans/${id}`).then(setPlan).catch(() => {}), 2500)
    return () => clearInterval(t)
  }, [plan?.Status, id])
  // A brand-new plan arrives with `welcome` in the history state. Open the
  // summary once it is ready, then drop the flag so a refresh is just the page.
  useEffect(() => {
    if (!locState.welcome || !plan || plan.Status !== 'ready') return
    setRunModal('first')
    nav(loc.pathname, { replace: true, state: {} })
  }, [locState.welcome, plan?.Status, plan?.PlanId, loc.pathname, nav])

  if (!plan) return <p className="muted">Loading…</p>
  const active = runs.find((r) => r.status === 'running' || r.status === 'queued')

  const save = async (p: Plan) => {
    try {
      const saved = await api.put<Plan>(`/api/plans/${id}`, p)
      setPlan(saved)
      toast('Saved')
      setTab('overview')
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const rebuild = async () => {
    try {
      setPlan(await api.post<Plan>(`/api/plans/${id}/rebuild`, {}))
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const remove = async () => {
    const ok = await confirm({
      title: 'Delete this plan?',
      body: `Delete “${plan.Source}” and every artifact it found? This cannot be undone.`,
      confirm: 'Delete plan',
    })
    if (!ok) return
    try {
      await api.del(`/api/plans/${id}`)
      toast('Plan deleted')
      nav('/app/plans')
    } catch (e: any) {
      toast(e.message || 'Could not delete that plan', true)
    }
  }

  return (
    <>
      <div className="page-head plan-head">
        <div>
          <h1>{plan.Source}</h1>
          <div className="row" style={{ marginTop: '0.4rem' }}>
            {plan.ScheduleEnabled && (
              <Badge kind="yellow">
                <ClockIcon size={12} /> {scheduleLabel(plan)} · next {fmtDate(plan.NextRunAt)}
              </Badge>
            )}
          </div>
        </div>
        <div className="row plan-actions">
          {active ? (
            <Link to={`/app/executions/${active.execution_id}`} className="btn">
              <StatusBadge status="running" /> Watch execution #{active.execution_id}
            </Link>
          ) : (
            <button className="btn primary" onClick={() => setRunModal('run')} disabled={plan.Status !== 'ready'}>
              ▶ Run now
            </button>
          )}
          <button
            className={'btn' + (advanced ? ' on' : '')}
            type="button"
            onClick={() => setAdvanced((v) => !v)}
            aria-expanded={advanced}
          >
            Advanced{planHasCustomModels(plan) && !advanced ? ' · models' : ''}
          </button>
          <button className="btn" onClick={() => setTab('edit')}>
            Edit
          </button>
          <button className="btn danger" type="button" onClick={remove}>
            Delete
          </button>
        </div>
      </div>

      {advanced && <PlanModels plan={plan} onSaved={setPlan} />}

      <div className="tabs">
        {(['overview', 'results', 'graph', 'trail', 'edit'] as Tab[]).map((t) => (
          <button key={t} className={tab === t ? 'active' : ''} onClick={() => setTab(t)}>
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </div>

      {tab === 'overview' && plan.Status === 'drafting' && (
        <div className="empty">
          <Spinner />
          <h2 style={{ marginTop: '1rem' }}>Building this search…</h2>
          <p className="muted">Working out where to look and what a good result looks like. This takes up to a minute.</p>
        </div>
      )}
      {tab === 'overview' && plan.Status === 'failed' && (
        <div className="card">
          <h3>This search couldn't be built</h3>
          <p className="muted" style={{ margin: '0.4rem 0 0.9rem' }}>
            The description may be too vague, or the agent was unavailable. Edit what you're looking for, or try again.
          </p>
          <button className="btn primary" onClick={rebuild}>
            Try again
          </button>
        </div>
      )}
      {tab === 'overview' && plan.Status === 'ready' && <Overview runs={runs} />}
      {tab === 'results' &&
        (plan.Kind === 'artifacts' ? (
          <ArtifactTable planId={plan.PlanId} />
        ) : plan.Kind === 'report' ? (
          <ReportView planId={plan.PlanId} />
        ) : plan.Kind === 'assets' ? (
          <AssetGrid planId={plan.PlanId} />
        ) : (
          <ProspectTable planId={plan.PlanId} />
        ))}
      {tab === 'graph' && <KnowledgeGraph planId={plan.PlanId} live={!!active} />}
      {tab === 'trail' && <TrailTab planId={plan.PlanId} />}
      {tab === 'edit' && (
        <PlanSettings
          key={plan.UpdatedAt || 'x'}
          plan={plan}
          onSaved={(saved) => {
            setPlan(saved)
            setTab('overview')
          }}
        />
      )}

      {runModal && <RunModal plan={plan} first={runModal === 'first'} onClose={() => setRunModal(null)} />}
    </>
  )
}

/// Overview is what the plan has done. What it is — the description, the
/// effort, the schedule — is on the Edit tab, where it can be changed rather
/// than only read back.
function Overview({ runs }: { runs: Run[] }) {
  return <RunsTab runs={runs} />
}

export function RunsTab({ runs }: { runs: Run[] }) {
  const nav = useNavigate()
  if (!runs.length) return <Empty title="No executions yet" />
  const spent = runs.reduce((a, r) => a + (r.cost_usd || 0), 0)
  const found = runs.reduce((a, r) => a + (r.new_prospects || 0), 0)
  return (
    <>
      <div className="run-total">
        <span>
          <b>{usd(spent)}</b> across {runs.length} execution{runs.length === 1 ? '' : 's'}
        </span>
        <span className="muted">
          {found} found{found > 0 ? ` · ${usd(spent / found)} each` : ' · nothing yet'}
        </span>
      </div>
      <div className="card pad0 table-wrap">
        <table className="runs-table">
          <thead>
            <tr>
              <th>Execution</th>
              <th>Plan</th>
              <th>Status</th>
              <th>Started</th>
              <th className="num">Found</th>
              <th className="num">Cost</th>
              <th className="num">Each</th>
            </tr>
          </thead>
          <tbody>
            {runs.map((r) => {
              // A run that spent money and came back empty is the thing worth
              // spotting, so it is said in words rather than left as a dash.
              const barren = r.status === 'succeeded' && !r.new_prospects && (r.cost_usd || 0) > 0
              return (
                // The whole row opens the run. The link stays inside it so
                // middle-click and "open in new tab" still work, and so the
                // row reads as something you can click.
                <tr
                  key={r.execution_id}
                  className={'clickable' + (barren ? ' run-barren' : '')}
                  onClick={() => nav(`/app/executions/${r.execution_id}`)}
                >
                  <td>
                    <Link to={`/app/executions/${r.execution_id}`} onClick={(e) => e.stopPropagation()}>
                      #{r.execution_id}
                    </Link>
                  </td>
                  <td>{r.source}</td>
                  <td>
                    <StatusBadge status={r.status} />
                  </td>
                  <td>{fmtDate(r.started_at)}</td>
                  <td className="num">{r.new_prospects}</td>
                  <td className="num">{r.cost_usd ? usd(r.cost_usd) : '—'}</td>
                  <td className="num">
                    {r.cost_per_result != null ? (
                      usd(r.cost_per_result)
                    ) : barren ? (
                      <span className="danger">nothing found</span>
                    ) : (
                      '—'
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
    </>
  )
}

function TrailTab({ planId }: { planId: number }) {
  const [t, setT] = useState<any>(null)
  const load = () => api.get(`/api/plans/${planId}/trail`).then(setT)
  useEffect(() => {
    load()
  }, [planId])
  if (!t) return <p className="muted">Loading…</p>
  return (
    <div className="stack trail">
      <div className="two">
        <div className="card pad0">
          <div className="trail-pane-head">
            Searches <b>{t.queries.length}</b>
          </div>
          <div className="table-wrap trail-scroll">
            <table>
              <thead>
                <tr>
                  <th>Search</th>
                  <th>Engine</th>
                  <th>Uses</th>
                  <th>Deepest</th>
                  <th>Artifacts</th>
                  <th>Last</th>
                </tr>
              </thead>
              <tbody>
                {t.queries.length === 0 && (
                  <tr>
                    <td colSpan={6} className="muted">
                      No searches recorded yet.
                    </td>
                  </tr>
                )}
                {t.queries.map((q: any) => (
                  <tr key={q.query_key}>
                    <td>{q.query}</td>
                    <td className="muted">{q.engine}</td>
                    <td className="num">{q.hits}</td>
                    <td className="num">p{q.max_depth}</td>
                    <td className="num">{q.new_prospects}</td>
                    <td className="muted">{ago(q.last_used_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
        <div className="card pad0">
          <div className="trail-pane-head">
            Pages opened <b>{t.pages.length}</b>
          </div>
          <div className="table-wrap trail-scroll">
            <table>
              <thead>
                <tr>
                  <th>Page</th>
                  <th>Visits</th>
                </tr>
              </thead>
              <tbody>
                {t.pages.length === 0 && (
                  <tr>
                    <td colSpan={2} className="muted">
                      No pages recorded yet.
                    </td>
                  </tr>
                )}
                {t.pages.map((p: any) => (
                  <tr key={p.url}>
                    <td>
                      <span className="cell" title={p.url}>
                        {p.url}
                      </span>
                    </td>
                    <td className="num">{p.visits}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </div>
      <SearchSeeds planId={planId} rows={t.frontier || []} onChange={load} />
    </div>
  )
}

function RunModal({ plan, first, onClose }: { plan: Plan; first?: boolean; onClose: () => void }) {
  const nav = useNavigate()
  const toast = useToast()
  const [target, setTarget] = useState(plan.TargetProspects)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    if (!first) return
    const fn = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', fn)
    return () => window.removeEventListener('keydown', fn)
  }, [first, onClose])
  const start = async () => {
    setBusy(true)
    try {
      const r = await api.post<{ execution_id: number }>('/api/executions', { plan_id: plan.PlanId, target: target || undefined })
      nav(`/app/executions/${r.execution_id}`)
    } catch (e: any) {
      toast(e.message, true)
      setBusy(false)
    }
  }
  if (first) {
    return (
      <div className="modal-bg" onClick={onClose}>
        <div className="modal plan-ready" onClick={(e) => e.stopPropagation()}>
          <button className="iconbtn plan-ready-close" type="button" onClick={onClose} aria-label="Close">
            ✕
          </button>
          <div className="plan-ready-mark" aria-hidden>
            <CheckIcon size={40} />
          </div>
          <h2>Your search is ready</h2>
          {plan.Source && <p className="muted">{plan.Source}</p>}
          <div className="row" style={{ justifyContent: 'center' }}>
            <button className="btn ghost" type="button" onClick={onClose}>
              Not yet
            </button>
            <button className="btn primary" type="button" disabled={busy} onClick={start}>
              {busy ? 'Starting…' : '▶ Run now'}
            </button>
          </div>
        </div>
      </div>
    )
  }
  return (
    <Modal title={`Run "${plan.Source}"`} onClose={onClose}>
      <div className="field">
        <label>Target results</label>
        <input type="number" min={0} max={500} value={target} onChange={(e) => setTarget(+e.target.value)} />
      </div>
      <p className="muted">Anything already stored is skipped, so a repeat run only brings back what is new.</p>
      <div className="row" style={{ justifyContent: 'flex-end' }}>
        <button className="btn ghost" type="button" onClick={onClose}>
          Cancel
        </button>
        <button className="btn primary" type="button" disabled={busy} onClick={start}>
          {busy ? 'Starting…' : '▶ Run now'}
        </button>
      </div>
    </Modal>
  )
}

import React, { useEffect, useState } from 'react'
import { Picker } from '../components/Picker'
import { Link, useSearchParams } from 'react-router-dom'
import { api, FieldSpec, fmtDate, money, PlanSummary, Prospect } from '../api'
import { Badge, Empty, Modal, useConfirm, useToast } from '../components/ui'
import { MapIcon } from '../components/icons'
import { ArtifactTable } from '../components/ArtifactTable'
import { ReportView } from '../components/ReportView'
import { AssetGrid } from '../components/AssetGrid'


export function ProspectTable({ planId }: { planId?: number }) {
  const [rows, setRows] = useState<Prospect[]>([])
  const [total, setTotal] = useState(0)
  const [q, setQ] = useState('')
  const [minValue, setMinValue] = useState(0)
  const [page, setPage] = useState(0)
  const [open, setOpen] = useState<Prospect | null>(null)
  const toast = useToast()
  const confirm = useConfirm()
  const limit = 50

  const params = () => {
    const p = new URLSearchParams()
    if (planId) p.set('plan_id', String(planId))
    if (q) p.set('search', q)
    if (minValue) p.set('min_value', String(minValue))
    return p
  }
  const load = async () => {
    const p = params()
    p.set('limit', String(limit))
    p.set('offset', String(page * limit))
    const r = await api.get<{ rows: Prospect[]; total: number }>(`/api/prospects?${p}`)
    setRows(r.rows)
    setTotal(r.total)
  }
  useEffect(() => {
    const t = setTimeout(load, 200)
    return () => clearTimeout(t)
  }, [planId, q, minValue, page])

  const csvUrl = () => {
    const p = params()
    return `/api/prospects.csv?${p}`
  }
  const clear = async () => {
    if (
      !(await confirm({
        title: planId ? 'Delete these artifacts?' : 'Delete every artifact?',
        body: planId ? 'Delete every artifact in this plan? This cannot be undone.' : 'Delete EVERY artifact in your account? This cannot be undone.',
        confirm: 'Delete',
      }))
    )
      return
    await api.del(`/api/prospects?${params()}`)
    toast('Deleted')
    load()
  }
  const del = async (id: number) => {
    await api.del(`/api/prospects/${id}`)
    setOpen(null)
    load()
  }

  return (
    <div className="stack">
      <div className="row between">
        <div className="row">
          <input type="text" placeholder="Search name, company, email…" value={q} onChange={(e) => (setPage(0), setQ(e.target.value))} style={{ width: 260 }} />
          <input type="number" placeholder="Min value" min={0} value={minValue || ''} onChange={(e) => (setPage(0), setMinValue(+e.target.value || 0))} style={{ width: 130 }} />
          <span className="muted">{total} artifacts</span>
        </div>
        <div className="row">
          <a className="btn" href={csvUrl()}>
            ⇩ Export CSV
          </a>
          {total > 0 && (
            <button className="btn danger sm" onClick={clear}>
              Clear
            </button>
          )}
        </div>
      </div>
      {rows.length === 0 ? (
        <Empty title="No artifacts here yet">Run a search plan and the rows land here, deduped and cleansed.</Empty>
      ) : (
        <div className="card pad0 table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Title</th>
                <th>Company</th>
                <th>Email</th>
                <th>Location</th>
                {!planId && <th>Plan</th>}
                <th>Value</th>
                <th>Seen</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((p) => (
                <tr key={p.prospect_id} className="clickable" onClick={() => setOpen(p)}>
                  <td>
                    <b>{p.name}</b>
                  </td>
                  <td>
                    <span className="cell">{p.title}</span>
                  </td>
                  <td>
                    <span className="cell">{p.company}</span>
                  </td>
                  <td>
                    <span className="cell">
                      {p.email} {p.email_status && <Badge kind={p.email_status === 'verified' ? 'ok' : ''}>{p.email_status}</Badge>}
                    </span>
                  </td>
                  <td>
                    <span className="cell">{p.location}</span>
                  </td>
                  {!planId && <td className="muted">{p.source}</td>}
                  <td className="num">{money(p.estimated_value)}</td>
                  <td className="muted">{fmtDate(p.last_seen_utc)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {total > limit && (
        <div className="row between">
          <button className="btn sm" disabled={page === 0} onClick={() => setPage(page - 1)}>
            ← Previous
          </button>
          <span className="muted">
            page {page + 1} of {Math.ceil(total / limit)}
          </span>
          <button className="btn sm" disabled={(page + 1) * limit >= total} onClick={() => setPage(page + 1)}>
            Next →
          </button>
        </div>
      )}
      {open && (
        <Modal title={open.name || open.company} onClose={() => setOpen(null)}>
          <table>
            <tbody>
              {(
                [
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
                ] as [string, string][]
              )
                .filter(([, v]) => v)
                .map(([k, v]) => (
                  <tr key={k}>
                    <td className="muted" style={{ width: 110 }}>
                      {k}
                    </td>
                    <td style={{ wordBreak: 'break-word' }}>{/^https?:\/\//.test(v) ? <a href={v} target="_blank" rel="noreferrer">{v}</a> : v}</td>
                  </tr>
                ))}
            </tbody>
          </table>
          <div className="row" style={{ justifyContent: 'flex-end', marginTop: '1rem' }}>
            <button className="btn danger sm" onClick={() => del(open.prospect_id)}>
              Delete
            </button>
          </div>
        </Modal>
      )}
    </div>
  )
}

type ResultRow = { kind: string; plan_id: number; plan: string; label: string; sublabel: string; url: string; last_seen_utc: string }

// Cross-plan searchable list — prospects and custom artifacts together, projected
// onto common columns (schemas differ, so this is the shared view).
/// The cross-plan table. The search box belongs to the page head above it, so
/// it arrives as a prop: one query, one row of controls, whichever view is
/// underneath.
function UnifiedResults({ q }: { q: string }) {
  const [rows, setRows] = useState<ResultRow[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(0)
  const LIMIT = 50
  // A new query starts at the first page, never mid-way through the old one.
  useEffect(() => setPage(0), [q])
  useEffect(() => {
    const t = setTimeout(async () => {
      const p = new URLSearchParams({ search: q, limit: String(LIMIT), offset: String(page * LIMIT) })
      const r = await api.get<{ rows: ResultRow[]; total: number }>(`/api/results?${p}`)
      setRows(r.rows)
      setTotal(r.total)
    }, 200)
    return () => clearTimeout(t)
  }, [q, page])
  return (
    <div className="stack">
      <span className="muted">
        {total} result{total === 1 ? '' : 's'}
      </span>
      {rows.length === 0 ? (
        <Empty title="Nothing here yet" icon={<MapIcon size={38} />}>Run a search and everything it finds lands here, searchable across all of it.</Empty>
      ) : (
        <div className="card pad0 table-wrap">
          <table>
            <thead>
              <tr>
                <th>Result</th>
                <th>From</th>
                <th>Type</th>
                <th>Link</th>
                <th>Seen</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i}>
                  <td>
                    <b>{r.label || '—'}</b>
                    {r.sublabel && (
                      <>
                        <br />
                        <span className="muted">{r.sublabel}</span>
                      </>
                    )}
                  </td>
                  <td>
                    <Link to={`/app/plans/${r.plan_id}`}>{r.plan}</Link>
                  </td>
                  <td>
                    <Badge>{r.kind}</Badge>
                  </td>
                  <td>
                    {r.url ? (
                      <a href={r.url} target="_blank" rel="noreferrer">
                        open ↗
                      </a>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                  <td className="muted">{fmtDate(r.last_seen_utc)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {total > LIMIT && (
        <div className="row between">
          <button className="btn sm" disabled={page === 0} onClick={() => setPage(page - 1)}>
            ← Previous
          </button>
          <span className="muted">
            page {page + 1} of {Math.ceil(total / LIMIT)}
          </span>
          <button className="btn sm" disabled={(page + 1) * LIMIT >= total} onClick={() => setPage(page + 1)}>
            Next →
          </button>
        </div>
      )}
    </div>
  )
}

export default function Results() {
  const [params, setParams] = useSearchParams()
  const [plans, setPlans] = useState<PlanSummary[]>([])
  const [q, setQ] = useState('')
  const planId = Number(params.get('plan_id') || 0) || undefined
  useEffect(() => {
    api.get<PlanSummary[]>('/api/plans').then(setPlans)
  }, [])
  const plan = plans.find((p) => p.PlanId === planId)
  return (
    <>
      <div className="page-head">
        <div>
          <h1>Results</h1>
          <div className="sub">Everything your search plans have found — search across all of them, or pick one.</div>
        </div>
        <div className="row head-controls">
          {/* Only the cross-plan view searches from here; a single plan has its
              own filters, which are specific to what that plan collects. */}
          {!planId && (
            <input type="search" placeholder="Search across every plan…" value={q} onChange={(e) => setQ(e.target.value)} />
          )}
          <Picker
            className="plan-picker"
            value={planId ? String(planId) : ''}
            onChange={(v) => setParams(v ? { plan_id: v } : {})}
            options={[{ value: '', label: 'All search plans' }, ...plans.map((p) => ({ value: String(p.PlanId), label: p.Source }))]}
          />
        </div>
      </div>
      {!planId ? (
        <UnifiedResults q={q} />
      ) : plan?.Kind === 'artifacts' ? (
        <ArtifactTable key={planId} planId={planId} />
      ) : plan?.Kind === 'report' ? (
        <ReportView key={planId} planId={planId} />
      ) : plan?.Kind === 'assets' ? (
        <AssetGrid key={planId} planId={planId} />
      ) : (
        <ProspectTable key={planId} planId={planId} />
      )}
    </>
  )
}

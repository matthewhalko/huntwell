import React, { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { api, can, fmtDate, money, Prospect } from '../api'
import { useAuth } from '../auth'
import { Badge, Empty, Loading, useConfirm, useToast } from '../components/ui'
import { ResultDialog } from '../components/ResultDialog'
import { useDraftOutreach } from './Outreach'
import { MapIcon } from '../components/icons'
import Grid, { LINK_COLUMN, useNarrow } from '../components/Grid'
import type { ColDef } from 'ag-grid-community'


export function ProspectTable({ planId }: { planId?: number }) {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const outreach = useDraftOutreach()
  const [rows, setRows] = useState<Prospect[] | null>(null)
  const [loading, setLoading] = useState(true)
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
    let cancelled = false
    setLoading(true)
    const t = setTimeout(async () => {
      try {
        const p = params()
        p.set('limit', String(limit))
        p.set('offset', String(page * limit))
        const r = await api.get<{ rows: Prospect[]; total: number }>(`/api/prospects?${p}`)
        if (!cancelled) {
          setRows(r.rows)
          setTotal(r.total)
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    }, 200)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
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

  // The same columns the table had, in the same order. Widths are relative:
  // `flex` shares the space, so the grid fills its card at any window size.
  const columns = useMemo<ColDef<Prospect>[]>(
    () => [
      { field: 'name', headerName: 'Name', flex: 1.4, cellRenderer: (p: { value: string }) => <b>{p.value}</b> },
      { field: 'title', headerName: 'Title', flex: 1.4 },
      { field: 'company', headerName: 'Company', flex: 1.4 },
      {
        field: 'email',
        headerName: 'Email',
        flex: 1.6,
        cellRenderer: (p: { data: Prospect }) => (
          <span className="cell">
            {p.data.email}{' '}
            {p.data.email_status && <Badge kind={p.data.email_status === 'verified' ? 'ok' : ''}>{p.data.email_status}</Badge>}
          </span>
        ),
      },
      { field: 'location', headerName: 'Location', flex: 1.1 },
      ...(planId ? [] : [{ field: 'source' as const, headerName: 'Plan', flex: 1.1, cellClass: 'muted' }]),
      {
        field: 'estimated_value',
        headerName: 'Value',
        flex: 0.8,
        type: 'rightAligned',
        valueFormatter: (p: { value: number | null }) => money(p.value),
        // Sorted as a number, not as the "$1,200" a reader sees.
        comparator: (a: number | null, b: number | null) => (a ?? -1) - (b ?? -1),
      },
      {
        field: 'last_seen_utc',
        headerName: 'Seen',
        flex: 1,
        cellClass: 'muted',
        valueFormatter: (p: { value: string }) => fmtDate(p.value),
      },
    ],
    [planId],
  )

  return (
    <div className="stack">
      <div className="row between">
        <div className="row">
          <input type="text" placeholder="Search name, company, email…" value={q} onChange={(e) => (setPage(0), setQ(e.target.value))} style={{ width: 260 }} />
          <input type="number" placeholder="Min value" min={0} value={minValue || ''} onChange={(e) => (setPage(0), setMinValue(+e.target.value || 0))} style={{ width: 130 }} />
          {rows === null ? <Loading inline /> : <span className="muted">{total} artifacts</span>}
        </div>
        <div className="row">
          <a className="btn" href={csvUrl()}>
            ⇩ Export CSV
          </a>
          {write && total > 0 && (
            <button className="btn danger sm" onClick={clear}>
              Clear
            </button>
          )}
        </div>
      </div>
      {rows === null || (loading && rows.length === 0) ? (
        <Grid loading rows={[]} columns={columns} />
      ) : rows.length === 0 ? (
        <Empty title="No artifacts here yet">Run a search plan and the rows land here, deduped and cleansed.</Empty>
      ) : (
        <Grid
          rows={rows}
          loading={loading}
          getRowId={(p) => String(p.prospect_id)}
          onRowClick={setOpen}
          columns={columns}
        />
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
          onDelete={write ? () => del(open.prospect_id) : undefined}
          actions={
            write && (
              <button className="btn primary sm" onClick={() => outreach.start(open.prospect_id)} disabled={outreach.busy}>
                {outreach.busy ? 'Writing…' : 'Draft outreach'}
              </button>
            )
          }
        />
      )}
    </div>
  )
}

type ResultRow = {
  kind: string
  id: number
  plan_id: number
  plan: string
  label: string
  sublabel: string
  url: string
  last_seen_utc: string
  detail?: Record<string, string | number | boolean | null>
}

function resultDeletePath(row: ResultRow): string | null {
  if (row.kind === 'prospect') return `/api/prospects/${row.id}`
  if (row.kind === 'artifact') return `/api/artifacts/${row.id}`
  if (row.kind === 'report') return `/api/reports/${row.id}`
  if (row.kind === 'file') return `/api/assets/${row.id}`
  return null
}

function resultField(v: string | number | boolean | null): React.ReactNode {
  if (v === null || v === undefined || v === '') return null
  if (typeof v === 'number' && Number.isFinite(v)) {
    return String(v)
  }
  const s = String(v)
  if (/^https?:\/\//.test(s)) {
    return (
      <a href={s} target="_blank" rel="noreferrer">
        {s}
      </a>
    )
  }
  return s
}

// Cross-plan searchable list — prospects and custom artifacts together, projected
// onto common columns (schemas differ, so this is the shared view).
/// The cross-plan table. The search box belongs to the page head above it, so
/// it arrives as a prop: one query, one row of controls, whichever view is
/// underneath.
/// Same cutoff as the bottom rail: under it, a five-column grid does not fit.
function UnifiedResults({ q }: { q: string }) {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const outreach = useDraftOutreach()
  const [rows, setRows] = useState<ResultRow[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(0)
  const [open, setOpen] = useState<ResultRow | null>(null)
  const toast = useToast()
  const narrow = useNarrow()
  const LIMIT = 50
  // A new query starts at the first page, never mid-way through the old one.
  useEffect(() => setPage(0), [q])
  useEffect(() => {
    let cancelled = false
    setLoading(true)
    const t = setTimeout(async () => {
      try {
        const p = new URLSearchParams({ search: q, limit: String(LIMIT), offset: String(page * LIMIT) })
        const r = await api.get<{ rows: ResultRow[]; total: number }>(`/api/results?${p}`)
        if (!cancelled) {
          setRows(r.rows)
          setTotal(r.total)
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    }, 200)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
  }, [q, page])
  const columns = useMemo<ColDef<ResultRow>[]>(() => {
    const all: ColDef<ResultRow>[] = [
      {
        colId: 'result',
        headerName: 'Result',
        flex: 1.6,
        valueGetter: (p) => p.data?.label,
        cellRenderer: (p: { data?: ResultRow }) =>
          p.data ? (
            <span>
              <b>{p.data.label || '—'}</b>
              {p.data.sublabel && <span className="muted"> · {p.data.sublabel}</span>}
            </span>
          ) : null,
      },
      {
        field: 'plan',
        headerName: 'From',
        flex: 1.1,
        cellRenderer: (p: { data?: ResultRow }) =>
          p.data ? (
            <Link to={`/app/plans/${p.data.plan_id}`} onClick={(e) => e.stopPropagation()}>
              {p.data.plan}
            </Link>
          ) : null,
      },
      {
        field: 'kind',
        headerName: 'Type',
        flex: 0.7,
        cellRenderer: (p: { value: string }) => (
          <span>
            <Badge>{p.value}</Badge>
          </span>
        ),
      },
      {
        field: 'url',
        headerName: 'Link',
        ...LINK_COLUMN,
        cellRenderer: (p: { value: string }) =>
          p.value ? (
            <a href={p.value} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()}>
              open ↗
            </a>
          ) : (
            <span className="muted">—</span>
          ),
      },
      {
        field: 'last_seen_utc',
        headerName: 'Seen',
        flex: 1,
        cellClass: 'muted',
        valueFormatter: (p: { value: string }) => fmtDate(p.value),
      },
    ]
    // A phone keeps the result. Plan, type, link and when it was seen are in
    // the row dialog — extra columns here only crush the name.
    return all.map((c) => {
      const id = c.colId || c.field
      const keep = id === 'result'
      return { ...c, hide: narrow && !keep, minWidth: narrow ? 72 : c.minWidth }
    })
  }, [narrow])
  return (
    <div className="stack">
      {rows === null ? (
        <Loading inline />
      ) : (
        <span className="muted">
          {total} result{total === 1 ? '' : 's'}
        </span>
      )}
      {rows === null || (loading && rows.length === 0) ? (
        <Grid loading fill={narrow} rows={[]} columns={columns} />
      ) : rows.length === 0 ? (
        <Empty title="Nothing here yet" icon={<MapIcon size={38} />}>Run a search and everything it finds lands here, searchable across all of it.</Empty>
      ) : (
        <Grid
          rows={rows}
          loading={loading}
          fill={narrow}
          getRowId={(r) => `${r.kind}-${r.id || 'x'}-${r.plan_id}-${r.label}-${r.last_seen_utc}-${r.url}`}
          onRowClick={setOpen}
          columns={columns}
        />
      )}
      {open && (
        <ResultDialog
          title={open.label || open.plan}
          fields={[
            ['Type', open.kind],
            ['Plan', <Link to={`/app/plans/${open.plan_id}`}>{open.plan}</Link>],
            ...Object.entries(open.detail || {}).map(([k, v]): [string, React.ReactNode] => [
              k,
              k === 'Value' && typeof v === 'number' ? money(v) : k === 'First seen' ? fmtDate(String(v)) : resultField(v),
            ]),
            ['Link', open.url],
            ['Seen', fmtDate(open.last_seen_utc)],
          ]}
          extra={
            open.kind === 'report' ? (
              <div className="row" style={{ marginTop: '1rem' }}>
                <a className="btn sm" href={`/api/reports/${open.id}/print`} target="_blank" rel="noreferrer">
                  Read report
                </a>
              </div>
            ) : open.kind === 'file' ? (
              <div className="row" style={{ marginTop: '1rem' }}>
                <a className="btn sm" href={`/api/assets/${open.id}`}>
                  Download
                </a>
              </div>
            ) : undefined
          }
          actions={
            write &&
            open.kind === 'prospect' &&
            open.id > 0 && (
              <button className="btn primary sm" onClick={() => outreach.start(open.id)} disabled={outreach.busy}>
                {outreach.busy ? 'Writing…' : 'Draft outreach'}
              </button>
            )
          }
          onClose={() => setOpen(null)}
          onDelete={
            write && resultDeletePath(open)
              ? async () => {
                  const path = resultDeletePath(open)
                  if (!path) return
                  await api.del(path)
                  setOpen(null)
                  toast('Deleted')
                  setRows((cur) => (cur ? cur.filter((r) => !(r.kind === open.kind && r.id === open.id)) : cur))
                  setTotal((n) => Math.max(0, n - 1))
                }
              : undefined
          }
        />
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
  const [q, setQ] = useState('')
  return (
    <div className="results-page">
      <div className="page-head">
        <div>
          <h1>Results</h1>
          <div className="sub">Everything your search plans have found.</div>
        </div>
        <div className="row head-controls">
          <input type="search" placeholder="Search across every plan…" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      </div>
      <UnifiedResults q={q} />
    </div>
  )
}

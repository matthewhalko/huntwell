import React, { useEffect, useMemo, useState } from 'react'
import type { ColDef } from 'ag-grid-community'
import { api, Artifact, can, FieldSpec, fmtDate, usd } from '../api'
import { useAuth } from '../auth'
import { ResultDialog } from './ResultDialog'
import { useDraftOutreach } from '../pages/Outreach'
import { Loading } from './ui'
import Grid, { LINK_COLUMN, useNarrow } from './Grid'

const NUMERIC = (t: string) => t === 'money' || t === 'number'

// A schema-driven results table for custom-artifact plans (the counterpart of
// ProspectTable). The columns arrive with the rows: a plan's own definition
// stays on the server, so the results endpoint is what describes their shape.
export function ArtifactTable({ planId }: { planId: number }) {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const outreach = useDraftOutreach()
  const [rows, setRows] = useState<Artifact[] | null>(null)
  const [schema, setSchema] = useState<FieldSpec[]>([])
  const [total, setTotal] = useState(0)
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
  const [open, setOpen] = useState<Artifact | null>(null)
  const LIMIT = 50

  const load = async () => {
    const p = new URLSearchParams({ plan_id: String(planId), search, limit: String(LIMIT), offset: String(page * LIMIT) })
    const r = await api.get<{ rows: Artifact[]; total: number; columns: FieldSpec[] | null }>(`/api/artifacts?${p}`)
    setRows(r.rows)
    setTotal(r.total)
    if (r.columns) setSchema(r.columns)
  }
  useEffect(() => {
    let cancelled = false
    const p = new URLSearchParams({ plan_id: String(planId), search, limit: String(LIMIT), offset: String(page * LIMIT) })
    api.get<{ rows: Artifact[]; total: number; columns: FieldSpec[] | null }>(`/api/artifacts?${p}`).then((r) => {
      if (cancelled) return
      setRows(r.rows)
      setTotal(r.total)
      if (r.columns) setSchema(r.columns)
    }).catch(() => {
      if (!cancelled) setRows((cur) => cur ?? [])
    })
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planId, search, page])

  const del = async (id: number) => {
    await api.del(`/api/artifacts/${id}`)
    load()
  }

  const cols = schema.filter((f) => f.key)
  const cell = (a: Artifact, f: FieldSpec) => {
    const v = a.fields?.[f.key]
    if (v === null || v === undefined || v === '') return <span className="muted">—</span>
    const s = String(v)
    if (f.type === 'url' || /^https?:\/\//.test(s)) {
      return (
        <a href={s} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()}>
          open ↗
        </a>
      )
    }
    if (f.type === 'money' && typeof v === 'number') return usd(v)
    return s
  }

  // One column per field in the plan's schema, sized by what it holds: links
  // narrow, numbers compact, long text wide — and all of them together exactly
  // the grid's width (see Grid), so it never scrolls sideways.
  const narrow = useNarrow()
  const columns = useMemo<ColDef<Artifact>[]>(() => {
    // At phone width only what names the row stays: the title column, and the
    // first number or price if there is one. Everything else, delete included,
    // is one tap away in the row's dialog.
    const title = cols.find((f) => f.role === 'title') || cols.find((f) => f.type === 'text') || cols[0]
    const figure = cols.find((f) => NUMERIC(f.type))
    const kept = (f: FieldSpec) => !narrow || f === title || f === figure
    const out: ColDef<Artifact>[] = cols.filter(kept).map((f) => {
      const base: ColDef<Artifact> = {
        colId: f.key,
        headerName: f.label || f.key,
        valueGetter: (p) => p.data?.fields?.[f.key] ?? '',
        cellRenderer: (p: { data?: Artifact }) => (p.data ? cell(p.data, f) : null),
        tooltipValueGetter: (p) => {
          const v = p.data?.fields?.[f.key]
          return v === null || v === undefined || v === '' ? undefined : String(v)
        },
      }
      if (f.type === 'url') return { ...base, ...LINK_COLUMN }
      if (NUMERIC(f.type)) return { ...base, flex: 0.7, type: 'rightAligned' }
      if (f.type === 'date') return { ...base, flex: 0.8 }
      if (f.type === 'longtext') return { ...base, flex: 2 }
      return { ...base, flex: f.role === 'title' ? 1.6 : 1.1 }
    })
    if (write && !narrow) {
      out.push({
        colId: 'delete',
        headerName: '',
        flex: 0,
        width: 52,
        minWidth: 52,
        maxWidth: 52,
        suppressSizeToFit: true,
        sortable: false,
        resizable: false,
        cellRenderer: (p: { data?: Artifact }) =>
          p.data ? (
            <button
              className="btn ghost sm"
              onClick={(e) => {
                e.stopPropagation()
                del(p.data!.artifact_id)
              }}
              title="Delete"
            >
              ✕
            </button>
          ) : null,
      })
    }
    return out
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schema, write, narrow])

  if (rows === null) return <Loading />

  return (
    <>
      <div className="row between" style={{ marginBottom: '0.7rem' }}>
        <input
          type="search"
          placeholder="Search these results…"
          value={search}
          onChange={(e) => {
            setPage(0)
            setSearch(e.target.value)
          }}
          style={{ minWidth: 260 }}
        />
        <div className="row" style={{ gap: '0.7rem' }}>
          <span className="muted">
            {total} item{total === 1 ? '' : 's'}
          </span>
          <a className="btn" href={`/api/artifacts.csv?plan_id=${planId}`}>
            ⇩ Export CSV
          </a>
        </div>
      </div>

      {cols.length === 0 ? (
        <p className="muted">This search plan has no columns yet — add some in its Columns tab.</p>
      ) : rows.length === 0 ? (
        <p className="muted">No results yet. Run this search plan to collect items.</p>
      ) : (
        <Grid
          rows={rows}
          columns={columns}
          getRowId={(r) => String(r.artifact_id)}
          onRowClick={setOpen}
          options={{ tooltipShowDelay: 400 }}
        />
      )}

      {total > LIMIT && (
        <div className="row" style={{ gap: '0.7rem', marginTop: '0.8rem' }}>
          <button className="btn" disabled={page === 0} onClick={() => setPage((p) => p - 1)}>
            ← Prev
          </button>
          <span className="muted">
            Page {page + 1} of {Math.ceil(total / LIMIT)}
          </span>
          <button className="btn" disabled={(page + 1) * LIMIT >= total} onClick={() => setPage((p) => p + 1)}>
            Next →
          </button>
        </div>
      )}
      {open && (
        <ResultDialog
          title={open.title || open.source_key}
          fields={[
            ...cols.map((f): [string, React.ReactNode] => {
              const v = open.fields?.[f.key]
              if (v === null || v === undefined || v === '') return [f.label || f.key, '']
              if (f.type === 'money' && typeof v === 'number') return [f.label || f.key, usd(v)]
              return [f.label || f.key, String(v)]
            }),
            ['URL', open.url],
            ['Plan', open.source],
            ['First seen', fmtDate(open.first_seen_utc)],
          ]}
          onClose={() => setOpen(null)}
          actions={
            write && (
              <span className="row outreach-go">
                {outreach.picker}
                <button className="btn primary sm" onClick={() => outreach.start({ artifact_id: open.artifact_id })} disabled={outreach.busy}>
                  {outreach.busy ? 'Writing…' : 'Draft outreach'}
                </button>
              </span>
            )
          }
          onDelete={
            write
              ? async () => {
                  await del(open.artifact_id)
                  setOpen(null)
                }
              : undefined
          }
        />
      )}
    </>
  )
}

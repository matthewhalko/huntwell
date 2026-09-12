import React, { useEffect, useState } from 'react'
import { api, Artifact, FieldSpec, usd } from '../api'

const NUMERIC = (t: string) => t === 'money' || t === 'number'

// A schema-driven results table for custom-artifact plans (the counterpart of
// ProspectTable). The columns arrive with the rows: a plan's own definition
// stays on the server, so the results endpoint is what describes their shape.
export function ArtifactTable({ planId }: { planId: number }) {
  const [rows, setRows] = useState<Artifact[]>([])
  const [schema, setSchema] = useState<FieldSpec[]>([])
  const [total, setTotal] = useState(0)
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
  const LIMIT = 50

  const load = async () => {
    const p = new URLSearchParams({ plan_id: String(planId), search, limit: String(LIMIT), offset: String(page * LIMIT) })
    const r = await api.get<{ rows: Artifact[]; total: number; columns: FieldSpec[] | null }>(`/api/artifacts?${p}`)
    setRows(r.rows)
    setTotal(r.total)
    if (r.columns) setSchema(r.columns)
  }
  useEffect(() => {
    load()
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
        <a href={s} target="_blank" rel="noreferrer">
          open ↗
        </a>
      )
    }
    if (f.type === 'money' && typeof v === 'number') return usd(v)
    return s
  }

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
        <div style={{ overflowX: 'auto' }}>
          <table>
            <thead>
              <tr>
                {cols.map((f) => (
                  <th key={f.key} className={NUMERIC(f.type) ? 'num' : ''}>
                    {f.label || f.key}
                  </th>
                ))}
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => (
                <tr key={a.artifact_id}>
                  {cols.map((f) => (
                    <td key={f.key} className={NUMERIC(f.type) ? 'num' : ''}>
                      {cell(a, f)}
                    </td>
                  ))}
                  <td className="num">
                    <button className="btn ghost sm" onClick={() => del(a.artifact_id)} title="Delete">
                      ✕
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
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
    </>
  )
}

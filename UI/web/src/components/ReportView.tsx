import React, { useEffect, useState } from 'react'
import { api, fmtDate, Report } from '../api'
import { Empty, useConfirm, useToast } from './ui'
import { SearchIcon } from './icons'

// A report plan's result: one document. The server renders the Markdown (raw
// HTML escaped there, so this is safe to inject) and also serves a
// self-contained print page — which is how a PDF is produced.
export function ReportView({ planId }: { planId: number }) {
  const [rows, setRows] = useState<Report[] | null>(null)
  const [html, setHtml] = useState('')
  const [openId, setOpenId] = useState<number | null>(null)
  const toast = useToast()
  const confirm = useConfirm()

  const load = () =>
    api
      .get<{ rows: Report[] }>(`/api/reports?plan_id=${planId}`)
      .then((r) => {
        setRows(r.rows)
        if (r.rows.length && openId === null) setOpenId(r.rows[0].report_id)
      })
      .catch(() => setRows([]))
  useEffect(() => {
    load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planId])

  useEffect(() => {
    if (openId === null) return
    api.get<{ html: string }>(`/api/reports/${openId}`).then((r) => setHtml(r.html)).catch(() => setHtml(''))
  }, [openId])

  const remove = async (id: number) => {
    if (!(await confirm({ title: 'Delete this report?', body: 'The document is removed. This cannot be undone.', confirm: 'Delete report' }))) return
    await api.del(`/api/reports/${id}`)
    toast('Deleted')
    setOpenId(null)
    load()
  }

  if (rows === null) return <p className="muted">Loading…</p>
  if (rows.length === 0)
    return (
      <Empty title="No report yet" icon={<SearchIcon size={38} />}>
        Run this search plan and the finished document lands here.
      </Empty>
    )

  const open = rows.find((r) => r.report_id === openId) || rows[0]
  return (
    <div className="stack">
      {rows.length > 1 && (
        <div className="row">
          <select value={open.report_id} onChange={(e) => setOpenId(+e.target.value)} style={{ width: 320 }}>
            {rows.map((r) => (
              <option key={r.report_id} value={r.report_id}>
                {r.title || r.subject}
              </option>
            ))}
          </select>
        </div>
      )}
      <div className="card">
        <div className="row between" style={{ marginBottom: '0.4rem' }}>
          <div>
            <h2 style={{ margin: 0 }}>{open.title || open.subject}</h2>
            <div className="muted sm">
              {open.subject} · {open.word_count.toLocaleString()} words · {open.sources?.length || 0} sources ·
              updated {fmtDate(open.last_seen_utc)}
            </div>
          </div>
          <div className="row">
            <a className="btn primary" href={`/api/reports/${open.report_id}/print`} target="_blank" rel="noreferrer">
              Print / Save as PDF
            </a>
            <button className="btn danger sm" onClick={() => remove(open.report_id)}>
              Delete
            </button>
          </div>
        </div>
        {/* Server-rendered from Markdown with raw HTML escaped. */}
        <div className="doc" dangerouslySetInnerHTML={{ __html: html }} />
        {open.sources?.length > 0 && (
          <>
            <h3 style={{ marginTop: '1.6rem' }}>Sources</h3>
            <ol className="doc-sources">
              {open.sources.map((s, i) => (
                <li key={i}>
                  {s.title && <span>{s.title} </span>}
                  {s.url && (
                    <a href={s.url} target="_blank" rel="noreferrer">
                      {s.url}
                    </a>
                  )}
                </li>
              ))}
            </ol>
          </>
        )}
      </div>
    </div>
  )
}

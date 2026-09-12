import React, { useEffect, useState } from 'react'
import { api, Asset, fmtBytes, fmtDate } from '../api'
import { Empty, useConfirm, useToast } from './ui'
import { MapIcon } from './icons'

// A short glyph for the file type. Text, not colour — the icon rules apply.
function typeLabel(a: Asset): string {
  const ext = (a.filename.split('.').pop() || '').toLowerCase()
  if (ext && ext.length <= 4 && /^[a-z0-9]+$/.test(ext)) return ext.toUpperCase()
  const ct = a.content_type
  if (ct.includes('pdf')) return 'PDF'
  if (ct.startsWith('image/')) return 'IMG'
  if (ct.includes('sheet') || ct.includes('excel')) return 'XLS'
  if (ct.includes('word')) return 'DOC'
  return 'FILE'
}

// An assets plan's result: the files it collected. Bytes stream from the
// object store through the account-scoped download endpoint.
export function AssetGrid({ planId }: { planId: number }) {
  const [rows, setRows] = useState<Asset[] | null>(null)
  const toast = useToast()
  const confirm = useConfirm()

  const load = () =>
    api
      .get<{ rows: Asset[] }>(`/api/assets?plan_id=${planId}`)
      .then((r) => setRows(r.rows))
      .catch(() => setRows([]))
  useEffect(() => {
    load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planId])

  const remove = async (a: Asset) => {
    if (!(await confirm({ title: 'Delete this file?', body: `Delete ${a.filename}? The stored file is removed too.`, confirm: 'Delete file' }))) return
    await api.del(`/api/assets/${a.asset_id}`)
    toast('Deleted')
    load()
  }

  if (rows === null) return <p className="muted">Loading…</p>
  if (rows.length === 0)
    return (
      <Empty title="No files yet" icon={<MapIcon size={38} />}>
        Run this search plan and every document it finds is downloaded and kept here.
      </Empty>
    )

  const total = rows.reduce((n, a) => n + a.size_bytes, 0)
  return (
    <div className="stack">
      <div className="row">
        <span className="muted">
          {rows.length} file{rows.length === 1 ? '' : 's'} · {fmtBytes(total)}
        </span>
      </div>
      <div className="grid cols-3">
        {rows.map((a) => (
          <div key={a.asset_id} className="card file-card">
            <div className="row between">
              <span className="ftype">{typeLabel(a)}</span>
              <span className="muted sm">{fmtBytes(a.size_bytes)}</span>
            </div>
            <div className="ftitle" title={a.title}>
              {a.title || a.filename}
            </div>
            <div className="muted sm mono fname" title={a.filename}>
              {a.filename}
            </div>
            <div className="foot row between">
              <a href={`/api/assets/${a.asset_id}`} className="btn sm">
                ⇩ Download
              </a>
              <div className="row" style={{ gap: '0.5rem' }}>
                {a.source_url && (
                  <a href={a.source_url} target="_blank" rel="noreferrer" className="muted sm" title={a.source_url}>
                    source ↗
                  </a>
                )}
                <button className="btn danger sm" onClick={() => remove(a)}>
                  ✕
                </button>
              </div>
            </div>
            <div className="muted sm">{fmtDate(a.last_seen_utc)}</div>
          </div>
        ))}
      </div>
    </div>
  )
}

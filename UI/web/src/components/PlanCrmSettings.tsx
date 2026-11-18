import React, { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { ago, api, CrmConnection, PlanCrmLink } from '../api'
import { useToast } from './ui'

/**
 * A plan's "send results to a CRM": which connected CRM, on or off, and how
 * it is going. The sync runs on the server within a minute of new results
 * (web/crm.rs); turning it on can also send what the plan already found.
 */
export function PlanCrmSettings({ planId, canWrite }: { planId: number; canWrite: boolean }) {
  const toast = useToast()
  const [data, setData] = useState<{ supported: boolean; connections: CrmConnection[]; links: PlanCrmLink[]; results: number } | null>(null)
  const [backfill, setBackfill] = useState(true)
  const [busy, setBusy] = useState(false)
  const load = () => api.get<typeof data>(`/api/plans/${planId}/crm`).then(setData).catch(() => setData(null))
  useEffect(() => {
    load()
  }, [planId])
  // While something is on, the counts move as the sync works.
  useEffect(() => {
    if (!data?.links.some((l) => l.enabled)) return
    const t = setInterval(load, 8000)
    return () => clearInterval(t)
  }, [data?.links.map((l) => `${l.connection_id}${l.enabled}`).join()])
  if (!data || !data.supported) return null

  const set = async (connection_id: number, enabled: boolean, fill: boolean) => {
    setBusy(true)
    try {
      setData(await api.put(`/api/plans/${planId}/crm`, { connection_id, enabled, backfill: fill }))
      toast(enabled ? 'On — sending results' : 'Stopped sending to the CRM')
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  const syncNow = async () => {
    try {
      await api.post(`/api/plans/${planId}/crm/sync`)
      toast('Sending now')
      setTimeout(load, 2500)
    } catch (e: any) {
      toast(e.message, true)
    }
  }

  return (
    <div className="card plan-extra-card">
      <h3>Send results to a CRM</h3>
      {data.connections.length === 0 ? (
        <p className="muted" style={{ margin: 0 }}>
          Connect your CRM under <Link to="/app/settings">Settings → CRM integrations</Link>, then turn it on here. Pipedrive is supported.
        </p>
      ) : (
        <>
          <p className="muted" style={{ marginTop: 0 }}>
            Each new result becomes a contact — a person with their email and phone, their company, and a note with everything Huntwell found. Someone already in the
            CRM with the same email is matched, not duplicated.
          </p>
          <div className="crm-list">
            {data.connections.map((c) => {
              const l = data.links.find((x) => x.connection_id === c.connection_id)
              const on = !!l?.enabled
              return (
                <div key={c.connection_id} className="crm-item">
                  <div style={{ minWidth: 0 }}>
                    <b>{c.provider_name}</b> <span className="muted">· {c.label}</span>
                    {l && (
                      <div className="muted sm">
                        {l.created} created · {l.matched} already there · {l.failed > 0 ? <span className="danger">{l.failed} failed</span> : '0 failed'}
                        {l.last_sync_at ? ` · checked ${ago(l.last_sync_at)}` : ''}
                      </div>
                    )}
                    {c.paused && <div className="danger sm">Paused: {c.last_error || 'the token was refused'}. Reconnect it in Settings.</div>}
                    {!c.paused && l?.last_error && <div className="muted sm">{l.last_error}</div>}
                    {canWrite && !on && data.results > 0 && (
                      <label className="check sm" style={{ marginTop: '0.35rem' }}>
                        <input type="checkbox" checked={backfill} onChange={(e) => setBackfill(e.target.checked)} /> Also send the {data.results} result
                        {data.results === 1 ? '' : 's'} already found
                      </label>
                    )}
                  </div>
                  {canWrite && (
                    <div className="row" style={{ flex: 'none' }}>
                      {on && (
                        <button className="btn sm" onClick={syncNow} disabled={busy}>
                          Send now
                        </button>
                      )}
                      <button className={'btn sm' + (on ? ' ghost' : ' primary')} onClick={() => set(c.connection_id, !on, !on && backfill)} disabled={busy || c.paused}>
                        {on ? 'Turn off' : 'Turn on'}
                      </button>
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        </>
      )}
    </div>
  )
}

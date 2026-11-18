import React, { useEffect, useState } from 'react'
import { ago, api, can, CrmConnection } from '../api'
import { useAuth } from '../auth'
import { Field, useConfirm, useToast } from './ui'

/**
 * CRMs this workspace pushes results into (web/crm.rs). Pipedrive first; the
 * list of providers comes from the server, so a new CRM needs no change here
 * beyond its token's name. The credential is checked with the CRM before it
 * is kept, and never shown again.
 */
export function CrmIntegrations() {
  const { me } = useAuth()
  const manage = can(me, 'keys')
  const toast = useToast()
  const confirm = useConfirm()
  const [conns, setConns] = useState<CrmConnection[] | null>(null)
  const [adding, setAdding] = useState(false)
  const [token, setToken] = useState('')
  const [lead, setLead] = useState(false)
  const [busy, setBusy] = useState(false)
  const load = () =>
    api
      .get<{ connections: CrmConnection[] }>('/api/integrations')
      .then((r) => setConns(r.connections))
      .catch(() => setConns([]))
  useEffect(() => {
    load()
  }, [])

  const connect = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const r = await api.post<{ connection: CrmConnection }>('/api/integrations', { provider: 'pipedrive', api_token: token.trim(), create_lead: lead })
      toast(`Connected to ${r.connection.label}`)
      setToken('')
      setAdding(false)
      load()
    } catch (err: any) {
      toast(err.message || 'Could not connect', true)
    } finally {
      setBusy(false)
    }
  }
  const setLeadFor = async (c: CrmConnection, on: boolean) => {
    try {
      await api.put(`/api/integrations/${c.connection_id}`, { create_lead: on })
      load()
    } catch (err: any) {
      toast(err.message, true)
    }
  }
  const remove = async (c: CrmConnection) => {
    const ok = await confirm({
      title: `Disconnect ${c.provider_name}?`,
      body: `Plans stop sending results to ${c.label}. What is already in ${c.provider_name} stays there.`,
      confirm: 'Disconnect',
      danger: true,
    })
    if (!ok) return
    try {
      await api.del(`/api/integrations/${c.connection_id}`)
      toast('Disconnected')
      load()
    } catch (err: any) {
      toast(err.message, true)
    }
  }

  return (
    <div className="card">
      <h3>CRM integrations</h3>
      <p className="muted" style={{ marginTop: 0 }}>
        Send a plan's results into your CRM as contacts, automatically after each run. Connect it here, then turn it on in a plan's <b>Edit</b> tab.
      </p>
      {conns === null ? null : conns.length === 0 && !adding ? (
        <p className="muted sm">No CRM connected yet.</p>
      ) : (
        <div className="crm-list">
          {conns.map((c) => (
            <div key={c.connection_id} className="crm-item">
              <div style={{ minWidth: 0 }}>
                <b>{c.provider_name}</b> <span className="muted">· {c.label}</span>
                <div className={c.paused ? 'danger sm' : 'muted sm'}>
                  {c.paused ? `Paused — ${c.last_error || 'the token was refused'}. Reconnect with a current token.` : c.last_ok_at ? `Working · last sent ${ago(c.last_ok_at)}` : 'Connected'}
                </div>
                {manage && (
                  <label className="check sm" style={{ marginTop: '0.35rem' }}>
                    <input type="checkbox" checked={c.create_lead} onChange={(e) => setLeadFor(c, e.target.checked)} /> Also create a lead for each new contact
                  </label>
                )}
              </div>
              {manage && (
                <button className="btn sm ghost danger" onClick={() => remove(c)}>
                  Disconnect
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      {manage &&
        (adding ? (
          <form onSubmit={connect} className="crm-add">
            <Field label="Pipedrive API token" hint="In Pipedrive: Settings → Personal preferences → API. Contacts are created as the person whose token this is.">
              <input value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" spellCheck={false} placeholder="Paste the token" autoFocus />
            </Field>
            <label className="check">
              <input type="checkbox" checked={lead} onChange={(e) => setLead(e.target.checked)} /> Also create a lead for each new contact
            </label>
            <div className="row" style={{ marginTop: '0.8rem' }}>
              <button className="btn primary" disabled={busy || !token.trim()}>
                {busy ? 'Checking…' : 'Connect Pipedrive'}
              </button>
              <button type="button" className="btn ghost" onClick={() => setAdding(false)}>
                Cancel
              </button>
            </div>
          </form>
        ) : (
          <button className="btn" onClick={() => setAdding(true)} style={{ marginTop: '0.6rem' }}>
            {conns && conns.some((c) => c.provider === 'pipedrive') ? 'Reconnect or add a Pipedrive account' : 'Connect Pipedrive'}
          </button>
        ))}
    </div>
  )
}

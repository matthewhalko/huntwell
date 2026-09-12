import { Link } from 'react-router-dom'
import React, { useEffect, useState } from 'react'
import { ago, api, ApiKey, fmtDate, PlanSummary } from '../api'
import { Badge, Copy, Empty, Field, Modal, useConfirm, useToast } from '../components/ui'

export default function ApiAccess() {
  const [keys, setKeys] = useState<ApiKey[]>([])
  const [audit, setAudit] = useState<any[]>([])
  const [plans, setPlans] = useState<PlanSummary[]>([])
  const [creating, setCreating] = useState(false)
  const [fresh, setFresh] = useState<ApiKey | null>(null)
  const toast = useToast()
  const confirm = useConfirm()
  const load = async () => {
    const [k, a] = await Promise.all([api.get<ApiKey[]>('/api/keys'), api.get<any[]>('/api/keys/audit?limit=100')])
    setKeys(k)
    setAudit(a)
  }
  useEffect(() => {
    load()
    api.get<PlanSummary[]>('/api/plans').then(setPlans)
  }, [])
  const origin = window.location.origin

  return (
    <>
      <div className="page-head">
        <div>
          <h1>API</h1>
          <div className="sub">Everything Huntwell does, it does through this. Create a plan, run it, read what it found.</div>
        </div>
        <div className="row">
          <Link to="/app/api-docs" className="btn">
            API reference →
          </Link>
          <button className="btn primary" onClick={() => setCreating(true)}>
            + New key
          </button>
        </div>
      </div>

      <div className="page-head">
        <div>
          <h2 style={{ margin: 0 }}>Keys</h2>
          <div className="sub">A key is shown once, at creation.</div>
        </div>
      </div>

      {keys.length === 0 ? (
        <Empty title="No keys yet">A key is shown once, at creation. Scope it to one plan, give it an expiry, pin it to an address.</Empty>
      ) : (
        <div className="card pad0 table-wrap">
          <table>
            <thead>
              <tr>
                <th>Key</th>
                <th>Label</th>
                <th>Scope</th>
                <th>From</th>
                <th>Expires</th>
                <th>Uses</th>
                <th>Last used</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {keys.map((k) => (
                <tr key={k.key_id} style={{ opacity: k.revoked || k.expired ? 0.55 : 1 }}>
                  <td className="mono">{k.token_hint}…</td>
                  <td>
                    {k.label} {k.revoked && <Badge kind="bad">revoked</Badge>} {k.expired && <Badge kind="warn">expired</Badge>}
                  </td>
                  <td>{k.source || <span className="muted">every plan</span>}</td>
                  <td
                    className="clickable mono"
                    title="Click to change the allowed addresses"
                    onClick={async () => {
                      const v = window.prompt('Allowed addresses (IPs or CIDRs, comma separated; empty = anywhere)', k.allow_cidr)
                      if (v === null) return
                      try {
                        await api.post(`/api/keys/${k.key_id}/allow`, { allow_cidr: v })
                        load()
                      } catch (e: any) {
                        toast(e.message, true)
                      }
                    }}
                  >
                    {k.allow_cidr || <span className="muted">anywhere</span>}
                  </td>
                  <td>{k.expires_at ? fmtDate(k.expires_at) : <span className="muted">never</span>}</td>
                  <td className="num">{k.uses}</td>
                  <td className="muted">
                    {ago(k.last_used_at)} {k.last_used_ip && <span className="mono">{k.last_used_ip}</span>}
                  </td>
                  <td>
                    <div className="row">
                      {!k.revoked && (
                        <button
                          className="btn sm"
                          onClick={async () => {
                            await api.post(`/api/keys/${k.key_id}/revoke`)
                            load()
                          }}
                        >
                          Revoke
                        </button>
                      )}
                      <button
                        className="btn sm danger"
                        onClick={async () => {
                          if (!(await confirm({ title: 'Delete this key?', body: 'Anyone still using it will be locked out.', confirm: 'Delete key' }))) return
                          await api.del(`/api/keys/${k.key_id}`)
                          load()
                        }}
                      >
                        Delete
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <h2 style={{ marginTop: '2rem' }}>Recent access</h2>
      {audit.length === 0 ? (
        <p className="muted">No download attempts recorded.</p>
      ) : (
        <div className="card pad0 table-wrap">
          <table>
            <thead>
              <tr>
                <th>When</th>
                <th>Key</th>
                <th>Address</th>
                <th>Outcome</th>
                <th>Path</th>
              </tr>
            </thead>
            <tbody>
              {audit.map((a) => (
                <tr key={a.audit_id}>
                  <td>{fmtDate(a.at)}</td>
                  <td className="mono">{a.key_id ? `#${a.key_id}` : '—'}</td>
                  <td className="mono">{a.ip}</td>
                  <td>
                    <Badge kind={a.outcome === 'ok' ? 'ok' : 'bad'}>{a.outcome}</Badge>
                  </td>
                  <td className="mono muted">{a.path}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {creating && (
        <CreateKey
          plans={plans}
          onClose={() => setCreating(false)}
          onCreated={(k) => {
            setCreating(false)
            setFresh(k)
            load()
          }}
        />
      )}
      {fresh && (
        <Modal title="Your new key" onClose={() => setFresh(null)}>
          <p className="notice">This is the only time the key is shown. Copy it now.</p>
          <Copy text={fresh.token} />
          <p className="muted" style={{ margin: '1rem 0 0.3rem' }}>
            Download URL
          </p>
          <Copy text={`${origin}/dl/prospects.csv?token=${fresh.token}`} />
        </Modal>
      )}
    </>
  )
}

function CreateKey({ plans, onClose, onCreated }: { plans: PlanSummary[]; onClose: () => void; onCreated: (k: ApiKey) => void }) {
  const [label, setLabel] = useState('')
  const [planId, setPlanId] = useState<number | ''>('')
  const [days, setDays] = useState(90)
  const [cidr, setCidr] = useState('')
  const [err, setErr] = useState('')
  const create = async () => {
    try {
      const k = await api.post<ApiKey>('/api/keys', { label, plan_id: planId || null, expires_in_days: days, allow_cidr: cidr })
      onCreated(k)
    } catch (e: any) {
      setErr(e.message)
    }
  }
  return (
    <Modal title="New API key" onClose={onClose}>
      {err && <div className="error">{err}</div>}
      <Field label="Label">
        <input type="text" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="nightly CRM import" autoFocus />
      </Field>
      <Field label="Scope">
        <select value={planId} onChange={(e) => setPlanId(e.target.value ? Number(e.target.value) : '')}>
          <option value="">Every plan</option>
          {plans.map((p) => (
            <option key={p.PlanId} value={p.PlanId}>
              {p.Source}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Expires in (days)" hint="0 = never">
        <input type="number" min={0} value={days} onChange={(e) => setDays(+e.target.value)} />
      </Field>
      <Field label="Allowed addresses" hint="IPs or CIDRs, comma separated. Empty = anywhere. The strongest control when you know where the puller lives.">
        <input type="text" value={cidr} onChange={(e) => setCidr(e.target.value)} placeholder="203.0.113.7, 10.0.0.0/8" />
      </Field>
      <div className="row" style={{ justifyContent: 'flex-end' }}>
        <button className="btn ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn primary" onClick={create}>
          Create key
        </button>
      </div>
    </Modal>
  )
}

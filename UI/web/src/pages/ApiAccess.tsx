import { Link } from 'react-router-dom'
import React, { useEffect, useState } from 'react'
import { ago, api, ApiKey, can, fmtDate, PlanSummary } from '../api'
import { useAuth } from '../auth'
import { Badge, Copy, Empty, Field, Loading, Modal, useConfirm, useToast } from '../components/ui'
import { Picker } from '../components/Picker'
import { useLoadView } from '../components/Skeleton'

export default function ApiAccess() {
  const { me } = useAuth()
  const makeKeys = can(me, 'keys')
  const [keys, setKeys] = useState<ApiKey[] | null>(null)
  const [audit, setAudit] = useState<any[] | null>(null)
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

  const keysView = useLoadView(keys !== null)
  const auditView = useLoadView(audit !== null)
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
          {makeKeys && (
            <button className="btn primary" onClick={() => setCreating(true)}>
              + New key
            </button>
          )}
        </div>
      </div>

      <div className="page-head">
        <div>
          <h2 style={{ margin: 0 }}>Keys</h2>
          <div className="sub">A key is shown once, at creation.</div>
        </div>
      </div>

      {keys === null || keysView !== 'ready' ? (
        keysView === 'skeleton' ? <Loading /> : null
      ) : keys.length === 0 ? (
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
                    {k.label} {k.kind === 'mcp' && <Badge kind="info">Claude</Badge>} {k.kind === 'ext' && <Badge kind="info">Chrome</Badge>} {k.revoked && <Badge kind="bad">revoked</Badge>}{' '}
                    {k.expired && <Badge kind="warn">expired</Badge>}
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
      {audit === null || auditView !== 'ready' ? (
        auditView === 'skeleton' ? <Loading /> : null
      ) : audit.length === 0 ? (
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
      {fresh && fresh.kind === 'mcp' && <McpKeyDialog fresh={fresh} onClose={() => setFresh(null)} />}
      {fresh && fresh.kind !== 'mcp' && (
        <Modal title="Your new key" onClose={() => setFresh(null)}>
          <p className="notice">
            This is the only time these are shown. Copy them now — neither can be recovered later.
          </p>
          <p className="muted" style={{ margin: '0.6rem 0 0.3rem' }}>
            Key
          </p>
          <Copy text={fresh.token} />
          {fresh.secret && (
            <>
              <p className="muted" style={{ margin: '1rem 0 0.3rem' }}>
                Secret
              </p>
              <Copy text={fresh.secret} />
            </>
          )}
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
  const [kind, setKind] = useState<'signed' | 'mcp'>('signed')
  const [err, setErr] = useState('')
  const create = async () => {
    try {
      const k = await api.post<ApiKey>('/api/keys', {
        label,
        plan_id: planId || null,
        expires_in_days: days,
        // Claude connects from Anthropic's servers, so an address pin would only lock it out.
        allow_cidr: kind === 'mcp' ? '' : cidr,
        kind,
      })
      onCreated(k)
    } catch (e: any) {
      setErr(e.message)
    }
  }
  return (
    <Modal title="New API key" onClose={onClose} className="key-create">
      {err && <div className="error">{err}</div>}
      <Field
        label="For"
        hint={
          kind === 'mcp'
            ? 'A key for Claude: add Huntwell as a connector so Claude can read your results and drafts while it works in your browser. Works only at /mcp.'
            : 'For your own code: every request is signed with the key\'s secret. See API docs.'
        }
      >
        <div className="seg">
          <button type="button" className={kind === 'signed' ? 'active' : ''} onClick={() => setKind('signed')}>
            API (signed)
          </button>
          <button type="button" className={kind === 'mcp' ? 'active' : ''} onClick={() => setKind('mcp')}>
            Claude (MCP)
          </button>
        </div>
      </Field>
      <Field label="Label">
        <input type="text" value={label} onChange={(e) => setLabel(e.target.value)} placeholder={kind === 'mcp' ? 'Claude in my browser' : 'nightly CRM import'} autoFocus />
      </Field>
      <Field label="Scope">
        <Picker
          value={planId === '' ? '' : String(planId)}
          onChange={(v) => setPlanId(v ? Number(v) : '')}
          options={[{ value: '', label: 'Every plan' }, ...plans.map((p) => ({ value: String(p.PlanId), label: p.Source }))]}
        />
      </Field>
      <Field label="Expires in (days)" hint="0 = never">
        <input type="number" min={0} value={days} onChange={(e) => setDays(+e.target.value)} />
      </Field>
      {kind === 'signed' && (
        <Field label="Allowed addresses" hint="IPs or CIDRs, comma separated. Empty = anywhere. The strongest control when you know where the puller lives.">
          <input type="text" value={cidr} onChange={(e) => setCidr(e.target.value)} placeholder="203.0.113.7, 10.0.0.0/8" />
        </Field>
      )}
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

/**
 * A new MCP key, and how to put it into Claude. Shown once: the connector URL
 * carries the key, for a Claude that cannot send headers (the Chrome side
 * panel and claude.ai, mostly); the header form is for Claude Code and any
 * Claude that can.
 */
function McpKeyDialog({ fresh, onClose }: { fresh: ApiKey; onClose: () => void }) {
  const origin = window.location.origin
  const url = `${origin}/mcp/${fresh.token}`
  const local = /^https?:\/\/(localhost|127\.|0\.0\.0\.0)/.test(origin)
  return (
    <Modal title="Your MCP key for Claude" onClose={onClose} className="mcp-key">
      <p className="notice">This is the only time this is shown. Copy it now — it cannot be recovered. Anyone with it can read this workspace's results and drafts.</p>
      <p className="muted" style={{ margin: '0.8rem 0 0.3rem' }}>
        Connector URL — the key is in it
      </p>
      <Copy text={url} />
      <ol className="mcp-steps">
        <li>
          In Claude, open <b>Customize → Connectors</b> and choose <b>Add custom connector</b>.
        </li>
        <li>
          Name it <b>Huntwell</b>, paste the URL above, and choose <b>No sign-in</b>.
        </li>
        <li>
          In a chat or the Chrome side panel, turn Huntwell on under <b>+ → Connectors</b> and ask, for example: “Show my newest Huntwell results for the dental
          plan and draft outreach to the first five — open each in Gmail for me to send.”
        </li>
      </ol>
      <details className="mcp-more">
        <summary>Claude Code, or a header instead of the URL</summary>
        <p className="muted sm">The key on its own, sent as a bearer token:</p>
        <Copy text={fresh.token} />
        <p className="muted sm">Claude Code:</p>
        <Copy text={`claude mcp add --transport http huntwell ${origin}/mcp --header "Authorization: Bearer ${fresh.token}"`} />
      </details>
      {local && (
        <p className="muted sm" style={{ marginTop: '0.8rem' }}>
          This is a local server, which Claude on the web cannot reach — it connects from Anthropic's servers. Use Claude Code here, or a tunnel to this server, or
          make the key on your live Huntwell.
        </p>
      )}
      <p className="muted sm" style={{ marginTop: '0.8rem' }}>
        Huntwell never sends anything through Claude: drafts come back with Gmail, Outlook and LinkedIn links for you to review and send.
      </p>
    </Modal>
  )
}


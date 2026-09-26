import React, { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import type { ColDef } from 'ag-grid-community'
import {
  api,
  can,
  fmtDate,
  Outreach as Draft,
  OutreachProfile,
  OutreachVersion,
  outreachBody,
  outreachEmail,
  outreachTo,
} from '../api'
import { useAuth } from '../auth'
import Grid, { useNarrow } from '../components/Grid'
import { Empty, Field, Loading, Modal, useConfirm, useToast } from '../components/ui'
import { CopyIcon, TickIcon } from '../components/icons'

/// Copy to the clipboard and say so — or say why not. True when it worked.
function useCopy() {
  const toast = useToast()
  return async (text: string, what: string) => {
    try {
      await navigator.clipboard.writeText(text)
      toast(`${what} copied`)
      return true
    } catch {
      toast('Copy failed — your browser blocked the clipboard', true)
      return false
    }
  }
}

/// A copy icon that sits at the edge of the part it copies and shows when that
/// part is hovered (always, on a touch screen). It becomes a tick for a moment
/// once copied, so the click is seen without looking away to a toast.
function CopyHover({ text, what }: { text: string; what: string }) {
  const copy = useCopy()
  const [done, setDone] = useState(false)
  useEffect(() => {
    if (!done) return
    const t = setTimeout(() => setDone(false), 1500)
    return () => clearTimeout(t)
  }, [done])
  if (!text.trim()) return null
  return (
    <button
      type="button"
      className={'copy-hover' + (done ? ' done' : '')}
      aria-label={`Copy ${what.toLowerCase()}`}
      title={`Copy ${what.toLowerCase()}`}
      onClick={async (e) => {
        e.stopPropagation()
        if (await copy(text, what)) setDone(true)
      }}
    >
      {done ? <TickIcon size={15} /> : <CopyIcon size={15} />}
    </button>
  )
}

/// Start a draft to a prospect and open it. Shared with the person dialog on
/// the Results pages, so both go through the same checks and the same page.
export function useDraftOutreach() {
  const nav = useNavigate()
  const toast = useToast()
  const [busy, setBusy] = useState(false)
  const start = async (prospectId: number) => {
    setBusy(true)
    try {
      const r = await api.post<{ outreach: Draft }>('/api/outreach', { prospect_id: prospectId })
      nav(`/app/outreach?open=${r.outreach.outreach_id}`)
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  return { start, busy }
}

export default function Outreach() {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const [items, setItems] = useState<Draft[] | null>(null)
  const [profile, setProfile] = useState<OutreachProfile | null>(null)
  const [showSettings, setShowSettings] = useState(false)
  const [showNew, setShowNew] = useState(false)
  const [params, setParams] = useSearchParams()
  const openId = Number(params.get('open')) || null
  const load = () => api.get<{ items: Draft[] }>('/api/outreach').then((r) => setItems(r.items))
  useEffect(() => {
    load()
    api.get<OutreachProfile>('/api/outreach/profile').then(setProfile)
  }, [])
  // Nothing to write from yet: the settings are where to start.
  const unset = !!profile && !profile.product.trim() && !profile.rules.trim()

  const narrow = useNarrow()
  // At phone width: who it is to and the subject; the rest is in the draft.
  const columns = useMemo<ColDef<Draft>[]>(
    () => [
      {
        headerName: 'To',
        flex: 1.4,
        valueGetter: (p) => p.data?.recipient_name || p.data?.recipient_email || '—',
        cellRenderer: (p: { value: string }) => <b>{p.value}</b>,
      },
      { field: 'recipient_company', headerName: 'Company', flex: 1.2, hide: narrow },
      { field: 'subject', headerName: 'Subject', flex: 2 },
      { field: 'created_by_name', headerName: 'By', flex: 0.8, hide: narrow },
      {
        field: 'updated_at',
        headerName: 'Updated',
        flex: 0.9,
        sort: 'desc',
        hide: narrow,
        valueFormatter: (p) => fmtDate(p.value),
      },
    ],
    [narrow],
  )

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Outreach</h1>
          <div className="sub">Cold emails drafted for the people your searches find. Copy one into your own mail to send it.</div>
        </div>
        <div className="row">
          <button className="btn" onClick={() => setShowSettings(true)}>
            Settings
          </button>
          {write && (
            <button className="btn primary" onClick={() => setShowNew(true)} disabled={profile?.ready === false}>
              + New draft
            </button>
          )}
        </div>
      </div>

      {profile && !profile.ready && (
        <div className="notice" style={{ marginBottom: '1rem' }}>
          Drafting isn't switched on for this server yet.
        </div>
      )}
      {unset && profile?.ready && (
        <div className="card outreach-start">
          <div>
            <h3 style={{ margin: 0 }}>Tell Huntwell what you sell</h3>
            <p className="muted" style={{ margin: '0.3rem 0 0' }}>
              Describe your product and how your emails should read. Every draft is written from it.
            </p>
          </div>
          <button className="btn primary" onClick={() => setShowSettings(true)}>
            Set up outreach
          </button>
        </div>
      )}

      {items === null ? (
        <Loading />
      ) : items.length === 0 ? (
        <Empty title="No drafts yet">
          <p className="muted">
            Open a person under Results and choose <b>Draft outreach</b>, or start a draft to anyone with <b>New draft</b>.
          </p>
        </Empty>
      ) : (
        <Grid
          rows={items}
          columns={columns}
          getRowId={(r) => String(r.outreach_id)}
          onRowClick={(r) => setParams({ open: String(r.outreach_id) })}
        />
      )}

      {showSettings && profile && (
        <SettingsDialog
          profile={profile}
          write={write}
          onClose={() => setShowSettings(false)}
          onSaved={(p) => {
            setProfile(p)
            setShowSettings(false)
          }}
        />
      )}
      {showNew && (
        <NewDraftDialog
          onClose={() => setShowNew(false)}
          onCreated={(o) => {
            setShowNew(false)
            load()
            setParams({ open: String(o.outreach_id) })
          }}
        />
      )}
      {openId && (
        <DraftDialog
          id={openId}
          write={write}
          onChanged={load}
          onClose={() => {
            params.delete('open')
            setParams(params)
          }}
        />
      )}
    </>
  )
}

function SettingsDialog({
  profile,
  write,
  onClose,
  onSaved,
}: {
  profile: OutreachProfile
  write: boolean
  onClose: () => void
  onSaved: (p: OutreachProfile) => void
}) {
  const toast = useToast()
  const [product, setProduct] = useState(profile.product)
  const [rules, setRules] = useState(profile.rules)
  const [footer, setFooter] = useState(profile.footer)
  const [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try {
      await api.put('/api/outreach/profile', { product, rules, footer })
      toast('Saved')
      onSaved({ ...profile, product, rules, footer })
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="Outreach settings" onClose={onClose} className="outreach-dialog">
      <Field label="What you sell" hint="Your product, what it does and who it's for. Shared with your team.">
        <textarea
          rows={6}
          value={product}
          maxLength={4000}
          disabled={!write}
          onChange={(e) => setProduct(e.target.value)}
          placeholder="We make booking software for independent hotels. It replaces the front desk's spreadsheets and cuts no-shows with automatic reminders…"
        />
      </Field>
      <Field label="Rules for drafts" hint="How every email should read. Shared with your team.">
        <textarea
          rows={5}
          value={rules}
          maxLength={4000}
          disabled={!write}
          onChange={(e) => setRules(e.target.value)}
          placeholder={'Under 120 words. Friendly, not salesy. Mention something specific about them.\nEnd with one simple question. Never mention pricing.'}
        />
      </Field>
      <Field label="Your footer" hint="Added to the end of your drafts exactly as written. Yours only — teammates set their own.">
        <textarea
          rows={4}
          value={footer}
          maxLength={1000}
          onChange={(e) => setFooter(e.target.value)}
          placeholder={'Sam Lee\nPartnerships, Acme\n(503) 555-0100 · acme.com'}
        />
      </Field>
      <div className="row between" style={{ marginTop: '1rem' }}>
        <span />
        <div className="row">
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" onClick={save} disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </Modal>
  )
}

function NewDraftDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (o: Draft) => void }) {
  const toast = useToast()
  const [r, setR] = useState({ name: '', email: '', title: '', company: '', notes: '' })
  const [busy, setBusy] = useState(false)
  const set = (k: keyof typeof r) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setR({ ...r, [k]: e.target.value })
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const res = await api.post<{ outreach: Draft }>('/api/outreach', { recipient: r })
      onCreated(res.outreach)
    } catch (err: any) {
      toast(err.message, true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="New draft" onClose={onClose} className="outreach-dialog">
      <form onSubmit={submit}>
        <div className="two">
          <Field label="Name">
            <input value={r.name} onChange={set('name')} maxLength={200} autoFocus />
          </Field>
          <Field label="Email">
            <input type="email" value={r.email} onChange={set('email')} maxLength={320} />
          </Field>
          <Field label="Title">
            <input value={r.title} onChange={set('title')} maxLength={200} />
          </Field>
          <Field label="Company">
            <input value={r.company} onChange={set('company')} maxLength={200} />
          </Field>
        </div>
        <Field label="What you know about them" hint="Optional. Anything that would make the email specific to them.">
          <textarea rows={3} value={r.notes} onChange={set('notes')} maxLength={2000} />
        </Field>
        <div className="row between" style={{ marginTop: '1rem' }}>
          <span className="muted sm">A name or a company is enough to start.</span>
          <button className="btn primary" disabled={busy || (!r.name.trim() && !r.company.trim())}>
            {busy ? 'Writing…' : 'Draft email'}
          </button>
        </div>
      </form>
    </Modal>
  )
}

const SOURCE_LABEL: Record<string, string> = {
  draft: 'First draft',
  revise: 'Revised',
  edit: 'Edited by hand',
  restore: 'Restored',
}

function DraftDialog({ id, write, onClose, onChanged }: { id: number; write: boolean; onClose: () => void; onChanged: () => void }) {
  const toast = useToast()
  const confirm = useConfirm()
  const copy = useCopy()
  const [o, setO] = useState<Draft | null>(null)
  const [versions, setVersions] = useState<OutreachVersion[]>([])
  const [feedback, setFeedback] = useState('')
  const [busy, setBusy] = useState<'' | 'revise' | 'save' | 'restore'>('')
  const [editing, setEditing] = useState(false)
  const [subject, setSubject] = useState('')
  const [body, setBody] = useState('')
  const [err, setErr] = useState('')

  const load = () =>
    api
      .get<{ outreach: Draft; versions: OutreachVersion[] }>(`/api/outreach/${id}`)
      .then((r) => {
        setO(r.outreach)
        setVersions(r.versions)
      })
      .catch((e) => setErr(e.message))
  useEffect(() => {
    load()
  }, [id])

  const changed = (next: Draft) => {
    setO(next)
    onChanged()
    api.get<{ versions: OutreachVersion[] }>(`/api/outreach/${id}`).then((r) => setVersions(r.versions))
  }
  const revise = async () => {
    setBusy('revise')
    try {
      const r = await api.post<{ outreach: Draft }>(`/api/outreach/${id}/revise`, { feedback })
      setFeedback('')
      changed(r.outreach)
      toast('Revised')
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy('')
    }
  }
  const saveEdit = async () => {
    setBusy('save')
    try {
      const r = await api.put<{ outreach: Draft }>(`/api/outreach/${id}`, { subject, body })
      setEditing(false)
      changed(r.outreach)
      toast('Saved')
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy('')
    }
  }
  const restore = async (version: number) => {
    setBusy('restore')
    try {
      const r = await api.post<{ outreach: Draft }>(`/api/outreach/${id}/restore`, { version })
      changed(r.outreach)
      toast(`Back to version ${version}`)
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy('')
    }
  }
  const remove = async () => {
    if (!(await confirm({ title: 'Delete this draft?', body: 'The draft and all its versions are removed.', confirm: 'Delete', danger: true }))) return
    try {
      await api.del(`/api/outreach/${id}`)
      onChanged()
      onClose()
    } catch (e: any) {
      toast(e.message, true)
    }
  }

  const title = o ? `To ${o.recipient_name || o.recipient_email || o.recipient_company || 'someone'}` : 'Draft'
  // A mail app opened with the draft in it. Long bodies can exceed what a
  // mailto link carries, so copying stays the main way out.
  const mailto = o
    ? `mailto:${encodeURIComponent(o.recipient_email)}?subject=${encodeURIComponent(o.subject)}&body=${encodeURIComponent(outreachBody(o))}`
    : ''

  return (
    <Modal title={title} onClose={onClose} className="outreach-dialog">
      {err ? (
        <p className="muted">{err}</p>
      ) : !o ? (
        <Loading />
      ) : (
        <>
          <div className="outreach-copybar">
            {/* The whole email at once; each part copies on its own from the
                icon beside it. */}
            <button className="btn primary sm" onClick={() => copy(outreachEmail(o), 'Email')}>
              <CopyIcon size={14} /> Copy email
            </button>
            {o.recipient_email && mailto.length < 1900 && (
              <a className="btn sm" href={mailto}>
                Open in mail app
              </a>
            )}
          </div>

          <dl className="outreach-meta">
            <dt>To</dt>
            <dd className="copyable">
              {outreachTo(o) || <span className="muted">no address — add it when you send</span>}
              {(o.recipient_title || o.recipient_company) && (
                <span className="muted"> · {[o.recipient_title, o.recipient_company].filter(Boolean).join(', ')}</span>
              )}
              <CopyHover text={outreachTo(o)} what="Recipient" />
            </dd>
            <dt>Subject</dt>
            <dd className={editing ? undefined : 'copyable'}>
              {editing ? (
                <input value={subject} onChange={(e) => setSubject(e.target.value)} maxLength={300} style={{ width: '100%' }} />
              ) : (
                <>
                  <b>{o.subject}</b>
                  <CopyHover text={o.subject} what="Subject" />
                </>
              )}
            </dd>
          </dl>

          {editing ? (
            <>
              <textarea className="outreach-body-edit" value={body} onChange={(e) => setBody(e.target.value)} rows={12} />
              {o.footer && <pre className="outreach-footer">{o.footer}</pre>}
              <div className="row" style={{ justifyContent: 'flex-end', marginTop: '0.6rem' }}>
                <button className="btn sm" onClick={() => setEditing(false)}>
                  Cancel
                </button>
                <button className="btn primary sm" onClick={saveEdit} disabled={busy !== '' || !body.trim()}>
                  {busy === 'save' ? 'Saving…' : 'Save edit'}
                </button>
              </div>
            </>
          ) : (
            <div className="outreach-body copyable">
              <pre>{o.body}</pre>
              {o.footer && <pre className="outreach-footer">{o.footer}</pre>}
              {/* The body as it is sent — with the footer. */}
              <CopyHover text={outreachBody(o)} what="Body" />
            </div>
          )}

          {write && !editing && (
            <div className="outreach-revise">
              <Field label="Change it" hint="Say what to change and Huntwell rewrites it. Earlier versions are kept below.">
                <textarea
                  rows={2}
                  value={feedback}
                  maxLength={2000}
                  onChange={(e) => setFeedback(e.target.value)}
                  placeholder="Shorter, and lead with their new Portland location"
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && feedback.trim()) revise()
                  }}
                />
              </Field>
              <div className="row between">
                <button
                  className="btn sm"
                  onClick={() => {
                    setSubject(o.subject)
                    setBody(o.body)
                    setEditing(true)
                  }}
                >
                  Edit by hand
                </button>
                <button className="btn primary sm" onClick={revise} disabled={busy !== '' || !feedback.trim()}>
                  {busy === 'revise' ? 'Rewriting…' : 'Revise'}
                </button>
              </div>
            </div>
          )}

          {versions.length > 1 && (
            <details className="outreach-versions">
              <summary>
                {versions.length} versions · this is version {o.version}
              </summary>
              <ol>
                {versions.map((v) => (
                  <li key={v.version}>
                    <div className="row between">
                      <span>
                        <b>v{v.version}</b> · {SOURCE_LABEL[v.source] || v.source} · <span className="muted">{fmtDate(v.created_at)}</span>
                      </span>
                      {write && v.version !== o.version && (
                        <button className="btn sm" onClick={() => restore(v.version)} disabled={busy !== ''}>
                          Use this
                        </button>
                      )}
                    </div>
                    {v.feedback && v.source === 'revise' && <div className="muted sm">“{v.feedback}”</div>}
                    <div className="sm outreach-version-subject">{v.subject}</div>
                  </li>
                ))}
              </ol>
            </details>
          )}

          <div className="row between" style={{ marginTop: '1rem' }}>
            {write ? (
              <button className="btn danger sm" onClick={remove}>
                Delete
              </button>
            ) : (
              <span />
            )}
            <button className="btn sm" onClick={onClose}>
              Close
            </button>
          </div>
        </>
      )}
    </Modal>
  )
}

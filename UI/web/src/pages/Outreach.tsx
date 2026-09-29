import React, { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import type { ColDef } from 'ag-grid-community'
import {
  api,
  can,
  fmtDate,
  Outreach as Draft,
  OutreachDesign,
  OutreachProfile,
  OutreachVersion,
  outreachBody,
  outreachEmail,
  outreachTo,
} from '../api'
import { useAuth } from '../auth'
import Grid, { useNarrow } from '../components/Grid'
import { CopyHover, Empty, Field, Loading, Modal, useConfirm, useToast } from '../components/ui'
import { CopyIcon } from '../components/icons'

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

/// Start a draft to a prospect and open it. Shared with the person dialog on
/// the Results pages, so both go through the same checks and the same page.
///
/// `picker` is a menu of the workspace's saved profiles (nothing when it has
/// none), to render beside the button: the default writes with the prospect's
/// plan's outreach; a profile applies to anyone.
export function useDraftOutreach() {
  const nav = useNavigate()
  const toast = useToast()
  const [busy, setBusy] = useState(false)
  const [designs, setDesigns] = useState<{ design_id: number; name: string }[]>([])
  const [designId, setDesignId] = useState<number | ''>('')
  useEffect(() => {
    api
      .get<{ designs: OutreachDesign[] }>('/api/outreach/designs')
      .then((r) => setDesigns(r.designs))
      .catch(() => setDesigns([]))
  }, [])
  const picker =
    designs.length > 0 ? (
      <select
        className="sm outreach-pick"
        value={designId}
        onChange={(e) => setDesignId(e.target.value ? +e.target.value : '')}
        aria-label="Write it with"
        title="Write it with"
      >
        <option value="">Its plan's outreach</option>
        {designs.map((d) => (
          <option key={d.design_id} value={d.design_id}>
            {d.name}
          </option>
        ))}
      </select>
    ) : null
  const start = async (prospectId: number) => {
    setBusy(true)
    try {
      const r = await api.post<{ outreach: Draft }>('/api/outreach', { prospect_id: prospectId, ...(designId ? { design_id: designId } : {}) })
      nav(`/app/outreach?open=${r.outreach.outreach_id}`)
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  return { start, busy, picker }
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
  const tab = params.get('tab') === 'profiles' ? 'profiles' : 'drafts'
  const [newFor, setNewFor] = useState<number | ''>('')
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
      { field: 'campaign', headerName: 'Written for', flex: 1, hide: narrow, valueFormatter: (p) => p.value || '—' },
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
            <button
              className="btn primary"
              onClick={() => {
                setNewFor('')
                setShowNew(true)
              }}
              disabled={profile?.ready === false}
            >
              + New draft
            </button>
          )}
        </div>
      </div>

      <div className="tabs">
        <button className={tab === 'drafts' ? 'active' : ''} onClick={() => setParams({})}>
          Drafts
        </button>
        <button className={tab === 'profiles' ? 'active' : ''} onClick={() => setParams({ tab: 'profiles' })}>
          Profiles
        </button>
      </div>

      {profile && !profile.ready && (
        <div className="notice" style={{ marginBottom: '1rem' }}>
          Drafting isn't switched on for this server yet.
        </div>
      )}
      {tab === 'profiles' ? (
        <ProfilesTab
          write={write}
          ready={profile?.ready !== false}
          onChanged={() => api.get<OutreachProfile>('/api/outreach/profile').then(setProfile)}
          onDraft={(id) => {
            setNewFor(id)
            setShowNew(true)
          }}
        />
      ) : (
        <>
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
        </>
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
          campaigns={profile?.campaigns || []}
          designs={profile?.designs || []}
          initialDesign={newFor}
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
      <p className="muted" style={{ marginTop: 0 }}>
        The workspace's default for every draft. A plan can tailor its own — who the campaign is for, the angle, its own rules — under the
        plan's <b>Edit</b> tab.
      </p>
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

function NewDraftDialog({
  campaigns,
  designs,
  initialDesign,
  onClose,
  onCreated,
}: {
  campaigns: { plan_id: number; name: string }[]
  designs: { design_id: number; name: string }[]
  initialDesign: number | ''
  onClose: () => void
  onCreated: (o: Draft) => void
}) {
  const toast = useToast()
  const [r, setR] = useState({ name: '', email: '', title: '', company: '', notes: '' })
  // "d:12" a saved profile, "p:7" a plan's own outreach, "" the workspace's.
  const [forWhat, setForWhat] = useState(initialDesign ? `d:${initialDesign}` : '')
  const [busy, setBusy] = useState(false)
  const set = (k: keyof typeof r) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setR({ ...r, [k]: e.target.value })
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const [kind, id] = forWhat.split(':')
      const res = await api.post<{ outreach: Draft }>('/api/outreach', {
        recipient: r,
        ...(kind === 'd' ? { design_id: +id } : kind === 'p' ? { plan_id: +id } : {}),
      })
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
        {(designs.length > 0 || campaigns.length > 0) && (
          <Field label="Write it with" hint="A saved profile, or a plan's own outreach: who it's for, the angle and its rules.">
            <select value={forWhat} onChange={(e) => setForWhat(e.target.value)}>
              <option value="">Workspace outreach settings</option>
              {designs.length > 0 && (
                <optgroup label="Profiles">
                  {designs.map((d) => (
                    <option key={d.design_id} value={`d:${d.design_id}`}>
                      {d.name}
                    </option>
                  ))}
                </optgroup>
              )}
              {campaigns.length > 0 && (
                <optgroup label="Plans">
                  {campaigns.map((c) => (
                    <option key={c.plan_id} value={`p:${c.plan_id}`}>
                      {c.name}
                    </option>
                  ))}
                </optgroup>
              )}
            </select>
          </Field>
        )}
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
            {o.campaign && (
              <>
                <dt>Written for</dt>
                <dd>
                  <Link to={o.design_id ? '/app/outreach?tab=profiles' : `/app/plans/${o.plan_id}`}>{o.campaign}</Link>
                </dd>
              </>
            )}
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

/**
 * Saved outreach profiles: a named design — who the emails are for and the
 * angle, what to offer, extra rules — that belongs to no plan. Any draft can
 * be written with one, to a prospect or anyone typed in, and a plan can use
 * one as its outreach.
 */
function ProfilesTab({
  write,
  ready,
  onChanged,
  onDraft,
}: {
  write: boolean
  ready: boolean
  onChanged: () => void
  onDraft: (designId: number) => void
}) {
  const toast = useToast()
  const confirm = useConfirm()
  const [designs, setDesigns] = useState<OutreachDesign[] | null>(null)
  const [editing, setEditing] = useState<OutreachDesign | 'new' | null>(null)
  const load = () => api.get<{ designs: OutreachDesign[] }>('/api/outreach/designs').then((r) => setDesigns(r.designs))
  useEffect(() => {
    load()
  }, [])
  const remove = async (d: OutreachDesign) => {
    const ok = await confirm({
      title: `Delete “${d.name}”?`,
      body:
        d.plans > 0
          ? `${d.plans} plan${d.plans === 1 ? '' : 's'} use${d.plans === 1 ? 's' : ''} it; they go back to the workspace's settings. Drafts already written keep their text.`
          : 'Drafts already written with it keep their text.',
      confirm: 'Delete',
      danger: true,
    })
    if (!ok) return
    try {
      await api.del(`/api/outreach/designs/${d.design_id}`)
      toast('Profile deleted')
      load()
      onChanged()
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  if (designs === null) return <Loading />
  return (
    <>
      <div className="row between" style={{ marginBottom: '1rem' }}>
        <p className="muted" style={{ margin: 0 }}>
          Write any email — to someone a plan found or anyone you type in — for a particular group, without tying it to a plan.
        </p>
        {write && (
          <button className="btn" onClick={() => setEditing('new')}>
            + New profile
          </button>
        )}
      </div>
      {designs.length === 0 ? (
        <Empty title="No profiles yet">
          <p className="muted">
            A profile says who a set of emails is for, the angle to take, what to offer them and any extra rules. Pick it when you draft.
          </p>
        </Empty>
      ) : (
        <div className="design-list">
          {designs.map((d) => (
            <div key={d.design_id} className="card design-card">
              <div className="row between" style={{ alignItems: 'flex-start' }}>
                <div style={{ minWidth: 0 }}>
                  <h3 style={{ margin: 0 }}>{d.name}</h3>
                  <p className="muted sm" style={{ margin: '0.2rem 0 0' }}>
                    {d.plans > 0 ? `Used by ${d.plans} plan${d.plans === 1 ? '' : 's'} · ` : ''}updated {fmtDate(d.updated_at)}
                  </p>
                </div>
                {write && (
                  <div className="row" style={{ flex: 'none' }}>
                    <button className="btn sm primary" onClick={() => onDraft(d.design_id)} disabled={!ready}>
                      Draft with this
                    </button>
                    <button className="btn sm" onClick={() => setEditing(d)}>
                      Edit
                    </button>
                    <button className="btn sm ghost danger" onClick={() => remove(d)}>
                      Delete
                    </button>
                  </div>
                )}
              </div>
              {d.brief && <p className="design-brief">{d.brief}</p>}
            </div>
          ))}
        </div>
      )}
      {editing && (
        <ProfileDialog
          design={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null)
            load()
            onChanged()
          }}
        />
      )}
    </>
  )
}

function ProfileDialog({ design, onClose, onSaved }: { design: OutreachDesign | null; onClose: () => void; onSaved: () => void }) {
  const toast = useToast()
  const [name, setName] = useState(design?.name || '')
  const [brief, setBrief] = useState(design?.brief || '')
  const [product, setProduct] = useState(design?.product || '')
  const [rules, setRules] = useState(design?.rules || '')
  const [busy, setBusy] = useState(false)
  const empty = !brief.trim() && !product.trim() && !rules.trim()
  const save = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      const body = { name, brief, product, rules }
      if (design) await api.put(`/api/outreach/designs/${design.design_id}`, body)
      else await api.post('/api/outreach/designs', body)
      toast(design ? 'Profile saved' : 'Profile created')
      onSaved()
    } catch (err: any) {
      toast(err.message, true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title={design ? 'Edit profile' : 'New outreach profile'} onClose={onClose} className="outreach-dialog">
      <form onSubmit={save}>
        <Field label="Name">
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={120} placeholder="Newly renovated hotels" autoFocus />
        </Field>
        <Field label="Who it's for, and the angle" hint="Who these people are, why you're writing now, and what you want from the email.">
          <textarea
            rows={4}
            value={brief}
            maxLength={4000}
            onChange={(e) => setBrief(e.target.value)}
            placeholder="Owners of boutique hotels who renovated in the last year. Lead with filling a newly reopened hotel's first season. Ask for a 15-minute call."
          />
        </Field>
        <Field label="What you're offering them" hint="Leave blank to use your workspace's description.">
          <textarea rows={3} value={product} maxLength={4000} onChange={(e) => setProduct(e.target.value)} />
        </Field>
        <Field label="Rules" hint="Added to your workspace's rules. Where they disagree, these win.">
          <textarea rows={3} value={rules} maxLength={4000} onChange={(e) => setRules(e.target.value)} placeholder="Mention their renovation. Under 90 words." />
        </Field>
        <div className="row between" style={{ marginTop: '1rem' }}>
          <span className="muted sm">{empty ? 'Fill in at least one of the three.' : ''}</span>
          <div className="row">
            <button className="btn" type="button" onClick={onClose}>
              Cancel
            </button>
            <button className="btn primary" disabled={busy || !name.trim() || empty}>
              {busy ? 'Saving…' : 'Save profile'}
            </button>
          </div>
        </div>
      </form>
    </Modal>
  )
}

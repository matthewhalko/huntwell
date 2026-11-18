import React, { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import type { ColDef } from 'ag-grid-community'
import { ago, api, can, Contact, ContactActivity, ContactImportResult, CrmOption, hasModule, PlanSummary } from '../api'
import { useAuth } from '../auth'
import Grid, { useNarrow } from '../components/Grid'
import { Badge, Empty, Field, Modal, useConfirm, useToast } from '../components/ui'
import { Picker } from '../components/Picker'
import { PageSkeleton, useLoadView } from '../components/Skeleton'

/// The built-in CRM (the `crm` module): everyone a workspace is working with,
/// what stage each is at, and a log of what happened with them. Contacts come
/// from search results (Import, or "Add to CRM" on a result) or by hand.

const STAGE_BADGE: Record<string, string> = { lead: '', contacted: 'info', qualified: 'warn', customer: 'ok', lost: 'bad' }

export function StageBadge({ stage, stages }: { stage: string; stages: CrmOption[] }) {
  return <Badge kind={STAGE_BADGE[stage] ?? ''}>{stages.find((s) => s.id === stage)?.label ?? stage}</Badge>
}

/// Today as YYYY-MM-DD in this browser's zone, to compare follow-up dates with.
function today(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function fmtDay(d: string | null): string {
  if (!d) return ''
  const [y, m, day] = d.split('-').map(Number)
  return new Date(y, m - 1, day).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: y === new Date().getFullYear() ? undefined : 'numeric' })
}

function FollowUp({ on }: { on: string | null }) {
  if (!on) return <span className="dim">—</span>
  const t = today()
  if (on < t) return <Badge kind="bad">{fmtDay(on)} · overdue</Badge>
  if (on === t) return <Badge kind="warn">today</Badge>
  return <span>{fmtDay(on)}</span>
}

function NotOn() {
  return (
    <Empty title="The CRM isn't switched on">
      The CRM is a module your Huntwell administrator turns on for a workspace. Ask them to switch it on for this one.
    </Empty>
  )
}

export default function Crm() {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const on = hasModule(me, 'crm')
  const nav = useNavigate()
  const [contacts, setContacts] = useState<Contact[] | null>(null)
  const [stages, setStages] = useState<CrmOption[]>([])
  const [counts, setCounts] = useState<Record<string, number>>({})
  const [stage, setStage] = useState('')
  const [q, setQ] = useState('')
  const [importing, setImporting] = useState(false)
  const [adding, setAdding] = useState(false)
  const load = () =>
    api.get<{ contacts: Contact[]; stages: CrmOption[]; counts: Record<string, number> }>('/api/contacts').then((r) => {
      setContacts(r.contacts)
      setStages(r.stages)
      setCounts(r.counts)
    })
  useEffect(() => {
    if (on) load().catch(() => setContacts([]))
  }, [on])

  // Filtered here, not on the server: a workspace's contacts fit in one load,
  // and typing should not wait on a round trip.
  const shown = useMemo(() => {
    const needle = q.trim().toLowerCase()
    return (contacts ?? []).filter(
      (c) =>
        (!stage || c.stage === stage) &&
        (!needle || [c.name, c.company, c.email, c.title, c.source_plan].some((v) => v.toLowerCase().includes(needle))),
    )
  }, [contacts, stage, q])

  const narrow = useNarrow()
  const columns = useMemo<ColDef<Contact>[]>(
    () => [
      {
        headerName: 'Name',
        flex: 1.4,
        minWidth: 170,
        valueGetter: (p) => p.data?.name || p.data?.company || p.data?.email || '(unnamed)',
        cellRenderer: (p: { value: string }) => <b>{p.value}</b>,
      },
      { field: 'company', headerName: 'Company', flex: 1.2, valueFormatter: (p) => p.value || '—' },
      {
        field: 'stage',
        headerName: 'Stage',
        width: 130,
        cellRenderer: (p: { value: string }) => <StageBadge stage={p.value} stages={stages} />,
      },
      { field: 'email', headerName: 'Email', flex: 1.3, hide: narrow, valueFormatter: (p) => p.value || '—' },
      {
        field: 'last_touch_at',
        headerName: 'Last touch',
        width: 130,
        valueFormatter: (p) => (p.value ? ago(p.value) : '—'),
      },
      {
        field: 'follow_up_on',
        headerName: 'Follow up',
        width: 160,
        // Overdue first, then soonest, then none.
        comparator: (a: string | null, b: string | null) => (a ?? '9999') .localeCompare(b ?? '9999'),
        cellRenderer: (p: { value: string | null }) => <FollowUp on={p.value} />,
      },
      { field: 'source_plan', headerName: 'From', flex: 1, hide: narrow, valueFormatter: (p) => p.value || 'Added by hand' },
    ],
    [stages, narrow],
  )

  const view = useLoadView(contacts !== null)
  if (!on) return <NotOn />
  const total = Object.values(counts).reduce((a, b) => a + b, 0)
  return (
    <>
      <div className="page-head">
        <div>
          <h1>CRM</h1>
          <div className="sub">The people you're working with, where each one stands, and what happened with them.</div>
        </div>
        {write && (
          <div className="row">
            <button className="btn" onClick={() => setImporting(true)}>
              Import from a plan
            </button>
            <button className="btn primary" onClick={() => setAdding(true)}>
              + New contact
            </button>
          </div>
        )}
      </div>

      {contacts === null || view !== 'ready' ? (
        view === 'skeleton' ? <PageSkeleton /> : null
      ) : contacts.length === 0 ? (
        <Empty title="No contacts yet">
          Bring in the people a search plan found with <b>Import from a plan</b>, choose <b>Add to CRM</b> on any result, or add someone by hand.
        </Empty>
      ) : (
        <>
          <div className="row contact-filters">
            <div className="seg small" role="tablist" aria-label="Stage">
              <button className={stage === '' ? 'active' : ''} onClick={() => setStage('')}>
                All {total}
              </button>
              {stages.map((s) => (
                <button key={s.id} className={stage === s.id ? 'active' : ''} onClick={() => setStage(s.id)}>
                  {s.label} {counts[s.id] ?? 0}
                </button>
              ))}
            </div>
            <input type="search" className="head-filter" placeholder="Search name, company, email…" value={q} onChange={(e) => setQ(e.target.value)} />
          </div>
          <Grid rows={shown} columns={columns} getRowId={(c) => String(c.contact_id)} onRowClick={(c) => nav(`/app/crm/${c.contact_id}`)} />
        </>
      )}

      {importing && (
        <ImportDialog
          onClose={() => setImporting(false)}
          onDone={() => {
            setImporting(false)
            load()
          }}
        />
      )}
      {adding && <NewContactDialog onClose={() => setAdding(false)} onCreated={(id) => nav(`/app/crm/${id}`)} />}
    </>
  )
}

/// "3 added, 1 matched someone already here" — what an import came to.
export function importSummary(r: ContactImportResult): string {
  const parts = []
  if (r.created) parts.push(`${r.created} added`)
  if (r.linked) parts.push(`${r.linked} matched someone already here by email`)
  if (r.already) parts.push(`${r.already} already in the CRM`)
  if (r.skipped) parts.push(`${r.skipped} skipped (no name, company or email)`)
  return parts.join(', ') || 'Nothing to import'
}

/// "Add to CRM" for one search result, in its dialog: a link to the contact
/// when it is in already. Nothing at all when the workspace has no CRM.
export function AddToCrm({ prospectId, artifactId }: { prospectId?: number; artifactId?: number }) {
  const { me } = useAuth()
  const on = hasModule(me, 'crm') && can(me, 'plans')
  const toast = useToast()
  const [contactId, setContactId] = useState<number | null | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const body = prospectId ? { prospect_ids: [prospectId] } : { artifact_ids: [artifactId] }
  useEffect(() => {
    if (!on) return
    api
      .post<{ prospects: Record<string, number>; artifacts: Record<string, number> }>('/api/contacts/lookup', body)
      .then((r) => setContactId((prospectId ? r.prospects[prospectId] : r.artifacts[artifactId!]) ?? null))
      .catch(() => setContactId(null))
  }, [on, prospectId, artifactId])
  if (!on || contactId === undefined) return null
  if (contactId)
    return (
      <Link className="btn sm" to={`/app/crm/${contactId}`}>
        In CRM →
      </Link>
    )
  const add = async () => {
    setBusy(true)
    try {
      const r = await api.post<ContactImportResult>('/api/contacts/import', body)
      if (r.contact_id) setContactId(r.contact_id)
      toast(r.linked ? 'Matched someone already in your CRM by email' : r.skipped ? importSummary(r) : 'Added to your CRM')
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <button className="btn sm" onClick={add} disabled={busy}>
      {busy ? 'Adding…' : 'Add to CRM'}
    </button>
  )
}

function ImportDialog({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const toast = useToast()
  const [plans, setPlans] = useState<PlanSummary[] | null>(null)
  const [planId, setPlanId] = useState('')
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    api.get<PlanSummary[]>('/api/plans').then((ps) => setPlans(ps.filter((p) => p.Kind === 'prospects' || p.Kind === 'artifacts' || !p.Kind)))
  }, [])
  const go = async () => {
    setBusy(true)
    try {
      const r = await api.post<ContactImportResult>('/api/contacts/import', { plan_id: Number(planId) })
      toast(importSummary(r))
      onDone()
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="Import from a plan" onClose={onClose}>
      <p className="muted" style={{ marginTop: 0 }}>
        Every result the plan has found comes in as a lead. Importing again later adds only what is new; someone whose email is already here is matched, not doubled.
      </p>
      <Field label="Plan">
        <Picker
          value={planId}
          onChange={setPlanId}
          placeholder={plans === null ? 'Loading…' : plans.length ? 'Pick a plan' : 'No plans that find people yet'}
          options={(plans ?? []).map((p) => ({ value: String(p.PlanId), label: p.Source, detail: `${p.prospects} result${p.prospects === 1 ? '' : 's'}` }))}
        />
      </Field>
      <div className="row" style={{ justifyContent: 'flex-end' }}>
        <button className="btn ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn primary" disabled={!planId || busy} onClick={go}>
          {busy ? 'Importing…' : 'Import'}
        </button>
      </div>
    </Modal>
  )
}

const EMPTY_FIELDS = { name: '', email: '', phone: '', title: '', company: '', website: '', linkedin: '', location: '', notes: '', stage: 'lead', follow_up_on: null as string | null }
type Fields = typeof EMPTY_FIELDS

function fieldsOf(c: Contact): Fields {
  return {
    name: c.name,
    email: c.email,
    phone: c.phone,
    title: c.title,
    company: c.company,
    website: c.website,
    linkedin: c.linkedin,
    location: c.location,
    notes: c.notes,
    stage: c.stage,
    follow_up_on: c.follow_up_on,
  }
}

function NewContactDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (id: number) => void }) {
  const [f, setF] = useState<Fields>(EMPTY_FIELDS)
  const [err, setErr] = useState('')
  const set = (k: keyof Fields) => (e: React.ChangeEvent<HTMLInputElement>) => setF({ ...f, [k]: e.target.value })
  const create = async () => {
    try {
      const r = await api.post<{ contact_id: number }>('/api/contacts', f)
      onCreated(r.contact_id)
    } catch (e: any) {
      setErr(e.message)
    }
  }
  return (
    <Modal title="New contact" onClose={onClose}>
      {err && <div className="error">{err}</div>}
      <div className="form-grid">
        <Field label="Name">
          <input type="text" value={f.name} onChange={set('name')} autoFocus />
        </Field>
        <Field label="Company">
          <input type="text" value={f.company} onChange={set('company')} />
        </Field>
        <Field label="Email">
          <input type="email" value={f.email} onChange={set('email')} />
        </Field>
        <Field label="Phone">
          <input type="tel" value={f.phone} onChange={set('phone')} />
        </Field>
        <Field label="Title">
          <input type="text" value={f.title} onChange={set('title')} />
        </Field>
      </div>
      <div className="row" style={{ justifyContent: 'flex-end' }}>
        <button className="btn ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn primary" onClick={create}>
          Add contact
        </button>
      </div>
    </Modal>
  )
}

// ---- one contact ----------------------------------------------------------------

type Detail = { contact: Contact; activity: ContactActivity[]; stages: CrmOption[]; kinds: CrmOption[] }

/// A datetime-local value for now, in this browser's zone.
function nowLocal(): string {
  const d = new Date()
  d.setMinutes(d.getMinutes() - d.getTimezoneOffset())
  return d.toISOString().slice(0, 16)
}

const KIND_GLYPH: Record<string, string> = { note: '✎', email: '✉', call: '☎', meeting: '◷', linkedin: 'in' }

export function ContactPage() {
  const { id } = useParams()
  const { me } = useAuth()
  const write = can(me, 'plans')
  const on = hasModule(me, 'crm')
  const nav = useNavigate()
  const toast = useToast()
  const confirm = useConfirm()
  const [d, setD] = useState<Detail | null>(null)
  const [missing, setMissing] = useState(false)
  const [f, setF] = useState<Fields>(EMPTY_FIELDS)
  const [saving, setSaving] = useState(false)
  const [kind, setKind] = useState('email')
  const [body, setBody] = useState('')
  const [when, setWhen] = useState(nowLocal)
  const [logging, setLogging] = useState(false)

  const take = (r: Detail) => {
    setD(r)
    setF(fieldsOf(r.contact))
  }
  useEffect(() => {
    if (!on) return
    api
      .get<Detail>(`/api/contacts/${id}`)
      .then(take)
      .catch(() => setMissing(true))
  }, [id, on])

  const view = useLoadView(d !== null || missing)
  if (!on) return <NotOn />
  if (missing)
    return (
      <Empty title="Contact not found">
        It may have been deleted. <Link to="/app/crm">Back to the CRM</Link>
      </Empty>
    )
  if (!d || view !== 'ready') return view === 'skeleton' ? <PageSkeleton kind="detail" /> : null

  const c = d.contact
  const dirty = JSON.stringify(f) !== JSON.stringify(fieldsOf(c))
  const set = (k: keyof Fields) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>) =>
    setF({ ...f, [k]: k === 'follow_up_on' ? e.target.value || null : e.target.value })

  const save = async (next: Fields = f) => {
    setSaving(true)
    try {
      take(await api.put<Detail>(`/api/contacts/${c.contact_id}`, next))
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setSaving(false)
    }
  }
  // The stage is one click: saved at once, along with anything else edited.
  const setStage = (stage: string) => {
    const next = { ...f, stage }
    setF(next)
    save(next)
  }
  const log = async () => {
    setLogging(true)
    try {
      const at = when ? new Date(when).toISOString() : undefined
      take(await api.post<Detail>(`/api/contacts/${c.contact_id}/activity`, { kind, body, occurred_at: at }))
      setBody('')
      setWhen(nowLocal())
    } catch (e: any) {
      toast(e.message, true)
    } finally {
      setLogging(false)
    }
  }
  const remove = async () => {
    if (!(await confirm({ title: 'Delete this contact?', body: 'Their details and everything logged with them go too. This cannot be undone.', confirm: 'Delete contact' })))
      return
    await api.del(`/api/contacts/${c.contact_id}`)
    nav('/app/crm')
  }
  const removeActivity = async (a: ContactActivity) => {
    if (!(await confirm({ title: 'Delete this entry?', body: 'It is removed from their history.', confirm: 'Delete' }))) return
    take(await api.del<Detail>(`/api/contacts/${c.contact_id}/activity/${a.activity_id}`))
  }
  const kindLabel = (k: string) => d.kinds.find((x) => x.id === k)?.label ?? k
  const linkedin = /^https?:\/\//i.test(c.linkedin) ? c.linkedin : ''
  const website = c.website && !/^https?:\/\//i.test(c.website) ? `https://${c.website}` : c.website

  return (
    <>
      <div className="page-head">
        <div>
          <div className="muted sm">
            <Link to="/app/crm">← CRM</Link>
          </div>
          <h1>
            {c.name || c.company || c.email} <StageBadge stage={c.stage} stages={d.stages} />
          </h1>
          <div className="sub">
            {[c.title, c.name ? c.company : ''].filter(Boolean).join(' · ') || ' '}
            {c.source_plan && <span className="dim"> — found by {c.source_plan}</span>}
          </div>
        </div>
        <div className="row">
          {c.email && (
            <a className="btn" href={`mailto:${encodeURIComponent(c.email)}`}>
              Email
            </a>
          )}
          {c.phone && (
            <a className="btn" href={`tel:${c.phone.replace(/[^\d+]/g, '')}`}>
              Call
            </a>
          )}
          {linkedin && (
            <a className="btn" href={linkedin} target="_blank" rel="noopener noreferrer">
              LinkedIn
            </a>
          )}
          {write && (
            <button className="btn danger" onClick={remove}>
              Delete
            </button>
          )}
        </div>
      </div>

      <div className="seg contact-stages" role="radiogroup" aria-label="Stage">
        {d.stages.map((s) => (
          <button key={s.id} className={f.stage === s.id ? 'active' : ''} disabled={!write || saving} onClick={() => setStage(s.id)} aria-checked={f.stage === s.id} role="radio">
            {s.label}
          </button>
        ))}
      </div>

      <div className="two contact-page">
        <div className="card">
          <h3 style={{ marginTop: 0 }}>Activity</h3>
          {write && (
            <div className="contact-log">
              <div className="seg small" role="radiogroup" aria-label="What happened">
                {d.kinds.map((k) => (
                  <button key={k.id} className={kind === k.id ? 'active' : ''} onClick={() => setKind(k.id)} role="radio" aria-checked={kind === k.id}>
                    {k.label}
                  </button>
                ))}
              </div>
              <textarea
                rows={3}
                value={body}
                onChange={(e) => setBody(e.target.value)}
                placeholder={kind === 'note' ? 'Write a note…' : `What was the ${kindLabel(kind).toLowerCase()} about? (optional)`}
              />
              <div className="row between">
                <label className="row muted sm" style={{ gap: '0.4rem' }}>
                  When
                  <input type="datetime-local" value={when} max={nowLocal()} onChange={(e) => setWhen(e.target.value)} />
                </label>
                <button className="btn primary sm" disabled={logging || (kind === 'note' && !body.trim())} onClick={log}>
                  {logging ? 'Saving…' : `Log ${kindLabel(kind).toLowerCase()}`}
                </button>
              </div>
            </div>
          )}
          {d.activity.length === 0 ? (
            <p className="muted">Nothing logged yet. Each email, call or meeting you log here shows up in their history, newest first.</p>
          ) : (
            <ul className="contact-timeline">
              {d.activity.map((a) => (
                <li key={a.activity_id}>
                  <span className="contact-timeline-ico" aria-hidden>
                    {KIND_GLYPH[a.kind] ?? '•'}
                  </span>
                  <div className="contact-timeline-main">
                    <div className="contact-timeline-head">
                      <b>{kindLabel(a.kind)}</b>
                      <span className="muted sm" title={new Date(a.occurred_at).toLocaleString()}>
                        {new Date(a.occurred_at).toLocaleString(undefined, { month: 'short', day: 'numeric', year: 'numeric', hour: 'numeric', minute: '2-digit' })}
                        {a.created_by_name && ` · ${a.created_by_name}`}
                      </span>
                      {write && (
                        <button className="linkish sm contact-timeline-del" onClick={() => removeActivity(a)} aria-label="Delete this entry">
                          Delete
                        </button>
                      )}
                    </div>
                    {a.body && <div className="contact-timeline-body">{a.body}</div>}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="card">
          <h3 style={{ marginTop: 0 }}>Details</h3>
          <fieldset disabled={!write} className="contact-fields">
            <Field label="Follow up on">
              <input type="date" value={f.follow_up_on ?? ''} onChange={set('follow_up_on')} />
            </Field>
            <Field label="Name">
              <input type="text" value={f.name} onChange={set('name')} />
            </Field>
            <Field label="Company">
              <input type="text" value={f.company} onChange={set('company')} />
            </Field>
            <Field label="Title">
              <input type="text" value={f.title} onChange={set('title')} />
            </Field>
            <Field label="Email">
              <input type="email" value={f.email} onChange={set('email')} />
            </Field>
            <Field label="Phone">
              <input type="tel" value={f.phone} onChange={set('phone')} />
            </Field>
            <Field label="Website">
              <input type="text" value={f.website} onChange={set('website')} />
            </Field>
            <Field label="LinkedIn">
              <input type="text" value={f.linkedin} onChange={set('linkedin')} />
            </Field>
            <Field label="Location">
              <input type="text" value={f.location} onChange={set('location')} />
            </Field>
            <Field label="Notes">
              <textarea rows={5} value={f.notes} onChange={set('notes')} />
            </Field>
          </fieldset>
          {website && /^https?:\/\//i.test(website) && (
            <p className="sm" style={{ marginTop: 0 }}>
              <a href={website} target="_blank" rel="noopener noreferrer">
                Open website ↗
              </a>
            </p>
          )}
          {write && (
            <div className="row" style={{ justifyContent: 'flex-end' }}>
              {dirty && (
                <button className="btn ghost sm" onClick={() => setF(fieldsOf(c))}>
                  Discard
                </button>
              )}
              <button className="btn primary sm" disabled={!dirty || saving} onClick={() => save()}>
                {saving ? 'Saving…' : 'Save'}
              </button>
            </div>
          )}
        </div>
      </div>
    </>
  )
}

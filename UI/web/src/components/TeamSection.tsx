import React, { useEffect, useState } from 'react'
import { ago, api, Team as TeamT } from '../api'
import { useAuth } from '../auth'
import { Field, useToast } from './ui'

/**
 * Who you work with — a section of Settings, since it is about the account
 * rather than about the searching.
 *
 * A workspace is simply the account that owns the data; a team is other people
 * given access to it. That is why everything on this page is about people
 * rather than about the plans — the plans are already shared the moment
 * somebody joins.
 */
/// Initials for the avatar tile: two letters from a name, one from an email.
function initials(name: string, email: string): string {
  const n = name.trim()
  if (n) {
    const parts = n.split(/\s+/)
    return ((parts[0][0] || '') + (parts.length > 1 ? parts[parts.length - 1][0] : '')).toUpperCase()
  }
  return (email.trim()[0] || '?').toUpperCase()
}

/// Which of the four brand colours a person gets. Stable per account, so a
/// teammate is the same colour every time you look at the list.
function tint(seed: number): string {
  return ['var(--brand-sky)', 'var(--brand-green)', 'var(--brand-yellow)', 'var(--brand-pink)'][Math.abs(seed) % 4]
}

export function TeamSection() {
  const { me } = useAuth()
  const toast = useToast()
  const [team, setTeam] = useState<TeamT | null>(null)
  const [email, setEmail] = useState('')
  const [role, setRole] = useState('member')
  const [busy, setBusy] = useState(false)
  const [emailed, setEmailed] = useState(false)
  const [link, setLink] = useState('')
  // The workspace's own name, like a Slack workspace: what everyone in it sees
  // in the switcher and what its invitations are sent in the name of.
  const [wsName, setWsName] = useState('')
  const [renaming, setRenaming] = useState(false)

  const load = () =>
    api
      .get<TeamT>('/api/team')
      .then((t) => {
        setTeam(t)
        setWsName(t.workspace)
      })
      .catch(() => {})

  const rename = async () => {
    setRenaming(true)
    try {
      const r = await api.put<{ workspace: string }>('/api/team', { name: wsName.trim() })
      setWsName(r.workspace)
      toast('Workspace renamed')
      load()
    } catch (e: any) {
      toast(e.message || 'Could not rename the workspace', true)
    } finally {
      setRenaming(false)
    }
  }
  useEffect(() => {
    load()
  }, [me?.workspace_id])

  const inviteLink = (token: string) => `${window.location.origin}/join/${token}`

  const invite = async (e?: React.FormEvent) => {
    e?.preventDefault()
    if (!email.trim() || busy) return
    setBusy(true)
    try {
      const r = await api.post<{ invite: { token: string }; emailed: boolean }>('/api/team/invite', { email: email.trim(), role })
      setLink(inviteLink(r.invite.token))
      setEmailed(!!r.emailed)
      setEmail('')
      load()
    } catch (e: any) {
      toast(e.message || 'Could not create that invitation', true)
    } finally {
      setBusy(false)
    }
  }

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text)
      toast('Invite link copied')
    } catch {
      toast('Copy failed — select the link and copy it', true)
    }
  }

  const revoke = async (id: number) => {
    await api.del(`/api/team/invites/${id}`)
    load()
  }
  const remove = async (id: number, who: string) => {
    if (!confirm(`Remove ${who} from this workspace? They keep their own account and their own plans.`)) return
    await api.del(`/api/team/members/${id}`)
    load()
  }

  return (
    <>
      {team?.can_invite && (
        <div className="card" style={{ marginBottom: '1.4rem' }}>
          <h3 style={{ marginTop: 0 }}>Workspace</h3>
          <p className="muted" style={{ margin: '0 0 0.9rem' }}>
            A workspace holds the search plans and everything they find. Everyone you invite works in it with you.
            {!me?.own_workspace && ' You were invited to this one.'}
          </p>
          <Field label="Workspace name" hint="What everyone here sees, and what invitations are sent in the name of.">
            <div className="row" style={{ gap: '0.5rem', flexWrap: 'nowrap' }}>
              <input value={wsName} onChange={(e) => setWsName(e.target.value)} placeholder={team?.workspace} style={{ flex: 1, minWidth: 0 }} />
              <button className="btn" onClick={rename} disabled={renaming || wsName.trim() === team?.workspace}>
                Rename
              </button>
            </div>
          </Field>
          <hr />
          <h3>Invite someone</h3>
          <form onSubmit={invite} className="row field-row" style={{ gap: '0.6rem', flexWrap: 'wrap' }}>
            <Field label="Their email">
              <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} placeholder="teammate@company.com" style={{ width: 260 }} />
            </Field>
            <Field label="Can they invite others?">
              <select value={role} onChange={(e) => setRole(e.target.value)} style={{ width: 170 }}>
                <option value="member">No — member</option>
                <option value="admin">Yes — admin</option>
              </select>
            </Field>
            <button className="btn primary" type="submit" disabled={busy || !email.trim()}>
              Create invite link
            </button>
          </form>
          {link && (
            <div className="notice" style={{ marginTop: '0.8rem' }}>
              {emailed
                ? 'Sent. They can also use this link — it works once, expires in 14 days, and only for the address it was made for.'
                : 'Send them this link — it works once, expires in 14 days, and only for the address it was made for.'}
              <div className="row" style={{ gap: '0.5rem', marginTop: '0.5rem', flexWrap: 'nowrap' }}>
                <input readOnly value={link} onFocus={(e) => e.currentTarget.select()} style={{ flex: 1, minWidth: 0 }} />
                <button className="btn" onClick={() => copy(link)}>
                  Copy
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      <div className="card">
        <h3 style={{ marginTop: 0 }}>{team?.can_invite ? 'Members' : 'Team'}</h3>
        <div className="members">
          {team?.members.map((m) => (
            <div className="member" key={m.account_id}>
              <span className="avatar" style={{ background: tint(m.account_id) }} aria-hidden>
                {initials(m.display_name, m.email)}
              </span>
              <div className="member-main">
                <div className="member-name">
                  {m.display_name || m.email}
                  {m.account_id === me?.account_id && <span className="you">you</span>}
                </div>
                {!!m.display_name && <div className="member-sub">{m.email}</div>}
              </div>
              <span className="badge">{m.owner ? 'owner' : m.role}</span>
              <span className="member-when">joined {ago(m.joined_at)}</span>
              <span className="member-act">
                {team?.can_invite && !m.owner && m.account_id !== me?.account_id && (
                  <button className="btn sm danger" onClick={() => remove(m.account_id, m.display_name || m.email)}>
                    Remove
                  </button>
                )}
              </span>
            </div>
          ))}
        </div>
      </div>

      {!!team?.invites.length && (
        <div className="card" style={{ marginTop: '1.4rem' }}>
          <h3 style={{ marginTop: 0 }}>Invited, not joined yet</h3>
          <div className="members">
            {team.invites.map((i) => (
              <div className="member" key={i.invite_id}>
                <span className="avatar pending" aria-hidden>
                  {initials('', i.email)}
                </span>
                <div className="member-main">
                  <div className="member-name">{i.email}</div>
                  <div className="member-sub">
                    as {i.role} · expires {ago(i.expires_at)}
                  </div>
                </div>
                <span className="member-act">
                  <button className="btn sm" onClick={() => copy(inviteLink(i.token))}>
                    Copy link
                  </button>
                  <button className="btn sm danger" onClick={() => revoke(i.invite_id)}>
                    Revoke
                  </button>
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  )
}

import React, { useEffect, useState } from 'react'
import { Navigate, useNavigate, useParams } from 'react-router-dom'
import { api } from '../api'
import { useAuth } from '../auth'
import { Spinner } from '../components/ui'

/**
 * The other end of an invite link.
 *
 * Signed out, it sends them to sign in and comes back here — the invitation is
 * bound to an email address, so there is no way to accept it as anyone else.
 */
export default function Join() {
  const { token } = useParams()
  const { me, loading, refresh } = useAuth()
  const nav = useNavigate()
  const [invite, setInvite] = useState<{ email: string; role: string; workspace: string } | null>(null)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api
      .get<{ email: string; role: string; workspace: string }>(`/api/team/join/${token}`)
      .then(setInvite)
      .catch((e) => setErr(e.message || 'This invitation is no longer valid'))
  }, [token])

  if (loading) return <div className="auth">Loading…</div>
  if (!me) return <Navigate to="/login" state={{ from: `/join/${token}` }} replace />

  const accept = async () => {
    setBusy(true)
    try {
      await api.post(`/api/team/join/${token}`, {})
      await refresh()
      nav('/app')
    } catch (e: any) {
      setErr(e.message || 'Could not accept that invitation')
      setBusy(false)
    }
  }

  return (
    <div className="auth">
      <div className="card raised" style={{ maxWidth: 460 }}>
        <div className="brand" style={{ color: 'var(--text)', padding: '0 0 1rem' }}>
          <span className="grad-text" aria-hidden>
            ✦
          </span>
          <span className="word">huntwell</span>
        </div>
        {err ? (
          <>
            <h1>That link didn't work</h1>
            <p className="muted">{err}</p>
            <button className="btn primary" onClick={() => nav('/app')}>
              Go to Huntwell
            </button>
          </>
        ) : !invite ? (
          <Spinner />
        ) : (
          <>
            <h1>Join {invite.workspace}</h1>
            <p className="muted">
              You've been invited to work in <b>{invite.workspace}</b> as {invite.role === 'admin' ? 'an admin' : 'a member'}. You'll
              share their search plans and everything those plans find.
            </p>
            {me.email.toLowerCase() !== invite.email.toLowerCase() && (
              <div className="notice" style={{ margin: '0.8rem 0' }}>
                This invitation was sent to <b>{invite.email}</b>, but you're signed in as {me.email}. Sign in as {invite.email} to
                accept it.
              </div>
            )}
            <button className="btn primary lg" onClick={accept} disabled={busy || me.email.toLowerCase() !== invite.email.toLowerCase()}>
              {busy ? 'Joining…' : `Join ${invite.workspace}`}
            </button>
          </>
        )}
      </div>
    </div>
  )
}

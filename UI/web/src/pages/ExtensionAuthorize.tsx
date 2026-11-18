import React, { useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { api, Workspace } from '../api'
import { useAuth } from '../auth'
import { Shell } from './Auth'

/// Huntwell for Chrome asking to sign in (web/extension.rs): what it will be
/// able to do, as whom, and Allow / Cancel. Opened by the extension in
/// Chrome's sign-in window; Allow sends a one-time code back to it.
export default function ExtensionAuthorize() {
  const { me } = useAuth()
  const [params] = useSearchParams()
  const redirect = params.get('redirect_uri') || ''
  const [problem, setProblem] = useState('')
  const [checked, setChecked] = useState(false)
  const [busy, setBusy] = useState(false)
  const [workspace, setWorkspace] = useState('')

  useEffect(() => {
    api
      .get(`/api/extension/check?redirect_uri=${encodeURIComponent(redirect)}`)
      .then(() => setChecked(true))
      .catch((e) => setProblem(e.message))
    api
      .get<{ workspaces: Workspace[] }>('/api/team/workspaces')
      .then((r) => setWorkspace(r.workspaces.find((w) => w.workspace_id === me?.workspace_id)?.name || ''))
      .catch(() => {})
  }, [redirect])

  const allow = async () => {
    setBusy(true)
    try {
      const r = await api.post<{ redirect: string }>('/api/extension/authorize', {
        redirect_uri: redirect,
        code_challenge: params.get('code_challenge') || '',
        code_challenge_method: params.get('code_challenge_method') || '',
        state: params.get('state') || '',
      })
      window.location.replace(r.redirect)
    } catch (e: any) {
      setProblem(e.message)
      setBusy(false)
    }
  }
  // Only ever back to an address the server accepted.
  const cancel = () => {
    if (!checked) return window.close()
    const u = new URL(redirect)
    u.searchParams.set('error', 'You did not approve the sign-in')
    u.searchParams.set('state', params.get('state') || '')
    window.location.replace(u.toString())
  }

  return (
    <Shell title="Connect Huntwell for Chrome">
      {problem ? (
        <div className="error">{problem}</div>
      ) : (
        <>
          <p className="muted" style={{ marginTop: 0 }}>
            Signed in as <b>{me?.email}</b>
            {workspace && (
              <>
                {' '}
                in <b>{workspace}</b>
              </>
            )}
            .
          </p>
          <p style={{ marginBottom: '0.4rem' }}>The extension will be able to:</p>
          <ul className="ext-scope">
            <li>see the outreach you queue for LinkedIn and Reddit</li>
            <li>open each person's page and fill your message in</li>
            <li>mark a draft sent once you've sent it</li>
          </ul>
          <p className="muted sm">
            It never presses Send — you do. It can't see your plans' settings, billing or keys. Revoke it any time under API access.
          </p>
          <div className="row" style={{ justifyContent: 'flex-end', marginTop: '1.2rem' }}>
            <button className="btn ghost" onClick={cancel} disabled={busy}>
              Cancel
            </button>
            <button className="btn primary" onClick={allow} disabled={busy || !checked} autoFocus>
              {busy ? 'Connecting…' : 'Allow'}
            </button>
          </div>
        </>
      )}
    </Shell>
  )
}

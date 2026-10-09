import React, { useEffect, useRef, useState } from 'react'
import { SkeletonFields } from '../components/Skeleton'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { api, AuthConfig, InvitationInfo } from '../api'
import { useAuth } from '../auth'
import { Field } from '../components/ui'
import { ArtBackdrop } from '../components/ArtBackdrop'

declare global {
  interface Window {
    turnstile?: {
      render: (el: HTMLElement, opts: Record<string, unknown>) => string
      reset: (id: string) => void
      remove: (id: string) => void
    }
  }
}

const TURNSTILE_SRC = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit'

/// Loads Cloudflare's script once, however many widgets ask for it.
let turnstileLoading: Promise<void> | null = null
function loadTurnstile(): Promise<void> {
  if (window.turnstile) return Promise.resolve()
  if (!turnstileLoading) {
    turnstileLoading = new Promise((resolve, reject) => {
      const s = document.createElement('script')
      s.src = TURNSTILE_SRC
      s.async = true
      s.onload = () => resolve()
      s.onerror = () => reject(new Error('the verification widget could not load'))
      document.head.appendChild(s)
    })
  }
  return turnstileLoading
}

/// What the sign-in and sign-up forms need before anyone is signed in. `null`
/// until it arrives, so the forms neither render a widget the server will not
/// check nor submit before knowing whether one is required.
export function useAuthConfig(): AuthConfig | null {
  const [cfg, setCfg] = useState<AuthConfig | null>(null)
  useEffect(() => {
    let live = true
    api
      .get<AuthConfig>('/api/auth/config')
      .then((c) => live && setCfg(c))
      // No config means an old server: behave as before, with no widget.
      .catch(() => live && setCfg({ open_signup: true, turnstile_site_key: null }))
    return () => {
      live = false
    }
  }, [])
  return cfg
}

/// The Turnstile widget. Reports its token up, and an empty string when the
/// token expires or the widget resets — tokens are single-use, so after a
/// failed submit `resetKey` bumps and a fresh one is issued.
function Turnstile({ siteKey, onToken, resetKey }: { siteKey: string; onToken: (t: string) => void; resetKey: number }) {
  const box = useRef<HTMLDivElement>(null)
  const widget = useRef<string | null>(null)
  const [err, setErr] = useState('')
  useEffect(() => {
    let live = true
    loadTurnstile()
      .then(() => {
        if (!live || !box.current || !window.turnstile) return
        widget.current = window.turnstile.render(box.current, {
          sitekey: siteKey,
          theme: document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light',
          callback: (t: string) => onToken(t),
          'expired-callback': () => onToken(''),
          'error-callback': () => onToken(''),
        })
      })
      .catch((e) => live && setErr(e.message))
    return () => {
      live = false
      if (widget.current && window.turnstile) window.turnstile.remove(widget.current)
      widget.current = null
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [siteKey])
  useEffect(() => {
    if (resetKey && widget.current && window.turnstile) {
      window.turnstile.reset(widget.current)
      onToken('')
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetKey])
  return (
    <div style={{ margin: '0.4rem 0 0.9rem' }}>
      <div ref={box} />
      {err && <div className="error">{err}</div>}
    </div>
  )
}

function Shell({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="auth">
      <ArtBackdrop />
      <div className="card raised">
        <Link to="/" className="brand" aria-label="Huntwell home" style={{ color: 'var(--text)', padding: '0 0 1rem', textDecoration: 'none', width: 'fit-content' }}>
          <span className="grad-text" aria-hidden>
            ✦
          </span>
          <span className="word">huntwell</span>
        </Link>
        <h1>{title}</h1>
        {children}
      </div>
    </div>
  )
}

export function Login() {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const { refresh } = useAuth()
  const nav = useNavigate()
  const loc = useLocation() as any
  const cfg = useAuthConfig()
  const [token, setToken] = useState('')
  const [resetKey, setResetKey] = useState(0)
  const needsChallenge = !!cfg?.turnstile_site_key
  // Sent here from a teammate's invitation while signed out: someone new
  // creates their account from that invitation instead.
  const joinToken = typeof loc.state?.from === 'string' ? (loc.state.from.match(/^\/join\/([^/?#]+)$/)?.[1] ?? '') : ''
  // Set when the password was right and the account wants its second factor.
  const [mfa, setMfa] = useState<{ challenge: string; email: string } | null>(null)
  const [code, setCode] = useState('')
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ mfa_required?: boolean; challenge?: string; email?: string }>('/api/auth/login', { email, password, turnstile_token: token })
      if (r.mfa_required && r.challenge) {
        setMfa({ challenge: r.challenge, email: r.email || email })
        return
      }
      await refresh()
      nav(loc.state?.from || '/app', { replace: true })
    } catch (e: any) {
      setErr(e.message)
      // A token is spent on the attempt, whatever the outcome.
      if (needsChallenge) setResetKey((k) => k + 1)
    } finally {
      setBusy(false)
    }
  }
  const submitCode = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!mfa) return
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/mfa', { email: mfa.email, challenge: mfa.challenge, code })
      await refresh()
      nav(loc.state?.from || '/app', { replace: true })
    } catch (e: any) {
      setErr(e.message)
      // A challenge is single-use: a wrong code means the password again.
      setMfa(null)
      setCode('')
      setPassword('')
    } finally {
      setBusy(false)
    }
  }
  if (mfa) {
    return (
      <Shell title="Two-factor code">
        <p>
          Enter the six-digit code from your authenticator app for <b>{mfa.email}</b>.
        </p>
        <form onSubmit={submitCode}>
          {err && <div className="error">{err}</div>}
          <Field label="Code">
            <input
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9 ]*"
              maxLength={7}
              value={code}
              onChange={(e) => setCode(e.target.value)}
              autoFocus
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace', fontSize: '1.4rem', letterSpacing: '0.25em' }}
            />
          </Field>
          <button className="btn primary" disabled={busy || code.replace(/\D/g, '').length !== 6} style={{ width: '100%' }}>
            {busy ? 'Checking…' : 'Sign in'}
          </button>
        </form>
        <p className="muted" style={{ marginTop: '1rem' }}>
          Lost your device? Ask an operator to reset two-factor for your account.
        </p>
      </Shell>
    )
  }
  return (
    <Shell title="Welcome back">
      <form onSubmit={submit}>
        {err && <div className="error">{err}</div>}
        <Field label="Email">
          <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoFocus autoComplete="email" required />
        </Field>
        <Field label="Password">
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
        </Field>
        {cfg?.turnstile_site_key && <Turnstile siteKey={cfg.turnstile_site_key} onToken={setToken} resetKey={resetKey} />}
        <button className="btn primary" disabled={busy || !cfg || (needsChallenge && !token)} style={{ width: '100%' }}>
          {busy ? 'Signing in…' : 'Sign in'}
        </button>
      </form>
      <p className="muted" style={{ marginTop: '1rem' }}>
        New here?{' '}
        {joinToken ? (
          <Link to={`/signup?invite=${encodeURIComponent(joinToken)}`}>Create an account to accept your invitation</Link>
        ) : (
          <Link to="/signup">{cfg?.invite_only ? 'Request access' : 'Create an account'}</Link>
        )}
        <br />
        <Link to="/forgot">Forgot your password?</Link>
      </p>
    </Shell>
  )
}

/// Sign-up. Huntwell is invite-only unless the server says otherwise: with an
/// invitation link (`?invite=…`) this is the account form, its address fixed
/// to the one invited; without one it asks to join the waitlist instead.
export function Signup() {
  const cfg = useAuthConfig()
  const loc = useLocation()
  const invite = new URLSearchParams(loc.search).get('invite')?.trim() || ''
  const [info, setInfo] = useState<InvitationInfo | null>(null)
  const [inviteErr, setInviteErr] = useState('')
  useEffect(() => {
    if (!invite) return
    let live = true
    api
      .get<InvitationInfo>(`/api/auth/invite/${encodeURIComponent(invite)}`)
      .then((i) => live && setInfo(i))
      .catch((e) => live && setInviteErr(e.message || 'This invitation has expired or has already been used'))
    return () => {
      live = false
    }
  }, [invite])

  if (!cfg || (invite && !info && !inviteErr)) {
    return (
      <Shell title="Create your account">
        <SkeletonFields fields={3} />
      </Shell>
    )
  }
  if (info) return <SignupForm cfg={cfg} invite={invite} info={info} />
  if (!cfg.invite_only) return <SignupForm cfg={cfg} />
  return <WaitlistForm cfg={cfg} inviteErr={inviteErr} />
}

function SignupForm({ cfg, invite, info }: { cfg: AuthConfig; invite?: string; info?: InvitationInfo }) {
  const [email, setEmail] = useState(info?.email || '')
  const [name, setName] = useState(info?.kind === 'platform' ? info.name : '')
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const { refresh } = useAuth()
  const nav = useNavigate()
  const [token, setToken] = useState('')
  const [resetKey, setResetKey] = useState(0)
  const needsChallenge = !!cfg.turnstile_site_key
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/signup', { email, password, display_name: name, turnstile_token: token, invite: invite || '' })
      await refresh()
      // A teammate's invitation is accepted on its own page, which says which
      // workspace they are joining.
      nav(info?.kind === 'team' && invite ? `/join/${encodeURIComponent(invite)}` : '/app', { replace: true })
    } catch (e: any) {
      setErr(e.message)
      if (needsChallenge) setResetKey((k) => k + 1)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Shell title={info?.kind === 'team' ? `Join ${info.workspace}` : 'Create your account'}>
      {info && (
        <p className="muted" style={{ marginTop: '-0.4rem' }}>
          {info.kind === 'team' ? (
            <>
              You were invited to <b>{info.workspace}</b>. Create your account to accept.
            </>
          ) : (
            <>You're invited. Choose a password to finish setting up your account.</>
          )}
        </p>
      )}
      <form onSubmit={submit}>
        {err && <div className="error">{err}</div>}
        <Field label="Your name">
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} autoFocus autoComplete="name" maxLength={80} />
        </Field>
        <Field label="Email" hint={info ? 'The invitation is for this address.' : undefined}>
          <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="email" required readOnly={!!info} />
        </Field>
        <Field label="Password" hint="At least 12 characters, with upper and lower case and a digit.">
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required minLength={12} />
        </Field>
        {cfg.turnstile_site_key && <Turnstile siteKey={cfg.turnstile_site_key} onToken={setToken} resetKey={resetKey} />}
        <button className="btn primary" disabled={busy || (needsChallenge && !token)} style={{ width: '100%' }}>
          {busy ? 'Creating…' : 'Create account'}
        </button>
      </form>
      {/* Assent has to be recorded somewhere a screenshot can show it. */}
      <p className="muted sm" style={{ marginTop: '0.8rem' }}>
        By creating an account you agree to the <Link to="/terms">Terms</Link> and the{' '}
        <Link to="/privacy">Privacy Policy</Link>.
      </p>
      <p className="muted" style={{ marginTop: '0.6rem' }}>
        Already have one? <Link to="/login">Sign in</Link>
      </p>
    </Shell>
  )
}

/// Invite-only: ask to be let in. The answer is the same whether or not the
/// address is already on the list (or already has an account), so this form
/// tells nobody anything about who uses Huntwell.
function WaitlistForm({ cfg, inviteErr }: { cfg: AuthConfig; inviteErr: string }) {
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const [note, setNote] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState(false)
  const [token, setToken] = useState('')
  const [resetKey, setResetKey] = useState(0)
  const needsChallenge = !!cfg.turnstile_site_key
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/waitlist', { email, name, note, turnstile_token: token })
      setDone(true)
    } catch (e: any) {
      setErr(e.message)
      if (needsChallenge) setResetKey((k) => k + 1)
    } finally {
      setBusy(false)
    }
  }
  if (done) {
    return (
      <Shell title="You're on the list">
        <p className="muted">
          Thanks{name.trim() ? `, ${name.trim()}` : ''}. We will email <b>{email.trim()}</b> for access.
        </p>
        <p className="muted" style={{ marginTop: '1rem' }}>
          <Link to="/">Back to Huntwell</Link>
        </p>
      </Shell>
    )
  }
  return (
    <Shell title="Request access">
      {inviteErr ? (
        <div className="notice" style={{ margin: '0 0 0.9rem' }}>
          That invitation link didn't work — {inviteErr.replace(/^./, (c) => c.toLowerCase())}. Ask whoever sent it for a new one, or
          request access below.
        </div>
      ) : (
        <p className="muted" style={{ marginTop: '-0.4rem' }}>
          Huntwell is invite-only. Join the waitlist, and we will email you for access.
        </p>
      )}
      <form onSubmit={submit}>
        {err && <div className="error">{err}</div>}
        <Field label="Your name">
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} autoFocus autoComplete="name" maxLength={80} />
        </Field>
        <Field label="Email">
          <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="email" required maxLength={320} />
        </Field>
        <Field label="What would you use Huntwell for?" hint="Optional, but it helps us decide.">
          <textarea value={note} onChange={(e) => setNote(e.target.value)} rows={3} maxLength={600} />
        </Field>
        {cfg.turnstile_site_key && <Turnstile siteKey={cfg.turnstile_site_key} onToken={setToken} resetKey={resetKey} />}
        <button className="btn primary" disabled={busy || (needsChallenge && !token)} style={{ width: '100%' }}>
          {busy ? 'Sending…' : 'Request access'}
        </button>
      </form>
      <p className="muted" style={{ marginTop: '0.8rem' }}>
        Have an account? <Link to="/login">Sign in</Link>
      </p>
    </Shell>
  )
}

/// Shown after sign-up (and after sign-in to an account that never verified):
/// the address has a six-digit code waiting in it. Nothing else works until
/// it is entered here.
export function CheckEmail() {
  const { me, loading, refresh } = useAuth()
  const nav = useNavigate()
  const [code, setCode] = useState('')
  const [err, setErr] = useState('')
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  // Once the address is confirmed, one more optional step: an authenticator.
  const [offerMfa, setOfferMfa] = useState(false)
  useEffect(() => {
    if (!loading && !me) nav('/login', { replace: true })
    if (me?.email_verified && !offerMfa) nav('/app', { replace: true })
  }, [me, loading, nav, offerMfa])
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    setNote('')
    try {
      await api.post('/api/auth/verify', { code })
      setOfferMfa(true)
      await refresh()
    } catch (e: any) {
      setErr(e.message)
      setCode('')
    } finally {
      setBusy(false)
    }
  }
  const resend = async () => {
    setBusy(true)
    setErr('')
    setNote('')
    try {
      const r = await api.post<{ ok: boolean; verified: boolean }>('/api/auth/resend')
      setNote(r.verified ? 'Already verified — taking you in.' : 'A new code is on its way. Check your inbox (and spam).')
      if (r.verified) await refresh()
    } catch (e: any) {
      setErr(e.message)
    } finally {
      setBusy(false)
    }
  }
  const signOut = async () => {
    await api.post('/api/auth/logout')
    await refresh()
    nav('/login', { replace: true })
  }
  const digits = code.replace(/\D/g, '')
  if (offerMfa) {
    return (
      <Shell title="Email confirmed">
        <MfaSetup
          intro="Optional, but a good idea: an authenticator app makes your account safe even if your password leaks."
          onDone={() => nav('/app', { replace: true })}
          skipLabel="Skip for now"
        />
      </Shell>
    )
  }
  return (
    <Shell title="Check your email">
      <p>
        We sent a six-digit code to <b>{me?.email}</b>. Enter it here to confirm the address is yours.
      </p>
      <form onSubmit={submit}>
        {err && <div className="error">{err}</div>}
        {note && <p className="muted">{note}</p>}
        <Field label="Code" hint="It expires in 15 minutes.">
          <input
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9 ]*"
            maxLength={7}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoFocus
            style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace', fontSize: '1.4rem', letterSpacing: '0.25em' }}
          />
        </Field>
        <button className="btn primary" disabled={busy || digits.length !== 6} style={{ width: '100%' }}>
          {busy ? 'Checking…' : 'Confirm'}
        </button>
      </form>
      <p className="muted" style={{ marginTop: '1rem' }}>
        Didn't get it?{' '}
        <a href="#" onClick={(e) => { e.preventDefault(); resend() }}>
          Send a new code
        </a>
        . Wrong address?{' '}
        <a href="#" onClick={(e) => { e.preventDefault(); signOut() }}>
          Sign out
        </a>{' '}
        and sign up again.
      </p>
    </Shell>
  )
}

/// Forgot password, in two steps on one page: the address, then the code
/// from the email plus a new password. The first step answers the same
/// whether the address has an account, so the page cannot be used to find out.
export function Forgot() {
  const nav = useNavigate()
  const cfg = useAuthConfig()
  const [step, setStep] = useState<'email' | 'code' | 'done'>('email')
  const [email, setEmail] = useState('')
  const [code, setCode] = useState('')
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const [token, setToken] = useState('')
  const [resetKey, setResetKey] = useState(0)
  const needsChallenge = !!cfg?.turnstile_site_key
  const send = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/forgot', { email, turnstile_token: token })
      setStep('code')
    } catch (e: any) {
      setErr(e.message)
      if (needsChallenge) setResetKey((k) => k + 1)
    } finally {
      setBusy(false)
    }
  }
  const finish = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/reset', { email, code, password })
      setStep('done')
    } catch (e: any) {
      setErr(e.message)
      if (e.status === 410) setStep('email')
    } finally {
      setBusy(false)
    }
  }
  const digits = code.replace(/\D/g, '')
  if (step === 'done') {
    return (
      <Shell title="Password changed">
        <p>Your password is updated and every other session was signed out.</p>
        <button className="btn primary" onClick={() => nav('/login', { replace: true })} style={{ width: '100%' }}>
          Sign in
        </button>
      </Shell>
    )
  }
  if (step === 'code') {
    return (
      <Shell title="Check your email">
        <p>
          If <b>{email}</b> has an account, a six-digit code is on its way. Enter it with your new password.
        </p>
        <form onSubmit={finish}>
          {err && <div className="error">{err}</div>}
          <Field label="Code" hint="It expires in 15 minutes.">
            <input
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9 ]*"
              maxLength={7}
              value={code}
              onChange={(e) => setCode(e.target.value)}
              autoFocus
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace', fontSize: '1.4rem', letterSpacing: '0.25em' }}
            />
          </Field>
          <Field label="New password" hint="At least 12 characters, with upper and lower case and a digit.">
            <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required minLength={12} />
          </Field>
          <button className="btn primary" disabled={busy || digits.length !== 6 || password.length < 12} style={{ width: '100%' }}>
            {busy ? 'Saving…' : 'Set new password'}
          </button>
        </form>
        <p className="muted" style={{ marginTop: '1rem' }}>
          Didn't get it?{' '}
          <a href="#" onClick={(e) => { e.preventDefault(); setStep('email'); setCode('') }}>
            Send another code
          </a>
        </p>
      </Shell>
    )
  }
  return (
    <Shell title="Reset your password">
      <p>Enter the address you signed up with and we'll email you a code.</p>
      <form onSubmit={send}>
        {err && <div className="error">{err}</div>}
        <Field label="Email">
          <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoFocus autoComplete="email" required />
        </Field>
        {cfg?.turnstile_site_key && <Turnstile siteKey={cfg.turnstile_site_key} onToken={setToken} resetKey={resetKey} />}
        <button className="btn primary" disabled={busy || !cfg || (needsChallenge && !token)} style={{ width: '100%' }}>
          {busy ? 'Sending…' : 'Send code'}
        </button>
      </form>
      <p className="muted" style={{ marginTop: '1rem' }}>
        Remembered it? <Link to="/login">Sign in</Link>
      </p>
    </Shell>
  )
}

/// Two-factor setup: the QR (and the secret as text), then a code from the
/// app to prove it took. Used on sign-up and from Settings; `askPassword`
/// is for Settings, where the sign-in may be an hour old.
export function MfaSetup({ intro, onDone, skipLabel, askPassword }: { intro: string; onDone: () => void; skipLabel?: string; askPassword?: boolean }) {
  const { refresh } = useAuth()
  const [enrol, setEnrol] = useState<{ secret: string; uri: string; qr_svg: string } | null>(null)
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  // Setup acts on a recent sign-in. When the server has none at hand it says
  // so (428) and this becomes a password prompt — never a dead end.
  const [needPw, setNeedPw] = useState(!!askPassword)
  const start = async () => {
    setBusy(true)
    setErr('')
    try {
      setEnrol(await api.post('/api/auth/mfa/setup', { password }))
    } catch (e: any) {
      if (e.status === 428) {
        setNeedPw(true)
        setErr('')
      } else {
        setErr(e.message)
      }
    } finally {
      setBusy(false)
    }
  }
  const confirm = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/mfa/confirm', { code })
      await refresh()
      onDone()
    } catch (e: any) {
      setErr(e.message)
      setCode('')
    } finally {
      setBusy(false)
    }
  }
  if (!enrol) {
    return (
      <div>
        <p>{intro}</p>
        {err && <div className="error">{err}</div>}
        {needPw && (
          <Field label="Your password" hint="To confirm it's you before adding a second factor.">
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && password) start() }}
              autoComplete="current-password"
              autoFocus
            />
          </Field>
        )}
        <div className="row" style={{ gap: '0.6rem' }}>
          <button className="btn primary" onClick={start} disabled={busy || (needPw && !password)}>
            {busy ? 'Starting…' : 'Set up an authenticator'}
          </button>
          {skipLabel && (
            <button className="btn" onClick={onDone} disabled={busy}>
              {skipLabel}
            </button>
          )}
        </div>
      </div>
    )
  }
  return (
    <form onSubmit={confirm}>
      <p>Scan this with Google Authenticator, 1Password, Authy or any TOTP app, then enter the code it shows.</p>
      <div style={{ width: 200, margin: '0.6rem 0' }} dangerouslySetInnerHTML={{ __html: enrol.qr_svg }} />
      <p className="muted sm" style={{ wordBreak: 'break-all' }}>
        Can't scan? Enter this key by hand: <code>{enrol.secret}</code>
      </p>
      {err && <div className="error">{err}</div>}
      <Field label="Code from the app">
        <input
          type="text"
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern="[0-9 ]*"
          maxLength={7}
          value={code}
          onChange={(e) => setCode(e.target.value)}
          autoFocus
          style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace', fontSize: '1.4rem', letterSpacing: '0.25em' }}
        />
      </Field>
      <div className="row" style={{ gap: '0.6rem' }}>
        <button className="btn primary" disabled={busy || code.replace(/\D/g, '').length !== 6}>
          {busy ? 'Checking…' : 'Turn on two-factor'}
        </button>
        {skipLabel && (
          <button type="button" className="btn" onClick={onDone} disabled={busy}>
            {skipLabel}
          </button>
        )}
      </div>
    </form>
  )
}

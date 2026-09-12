import React, { useState } from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { api } from '../api'
import { useAuth } from '../auth'
import { Field } from '../components/ui'

function Shell({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="auth">
      <div className="card raised">
        <div className="brand" style={{ color: 'var(--text)', padding: '0 0 1rem' }}>
          <span className="grad-text" aria-hidden>
            ✦
          </span>
          <span className="word">huntwell</span>
        </div>
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
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/login', { email, password })
      await refresh()
      nav(loc.state?.from || '/app', { replace: true })
    } catch (e: any) {
      setErr(e.message)
    } finally {
      setBusy(false)
    }
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
        <button className="btn primary" disabled={busy} style={{ width: '100%' }}>
          {busy ? 'Signing in…' : 'Sign in'}
        </button>
      </form>
      <p className="muted" style={{ marginTop: '1rem' }}>
        New here? <Link to="/signup">Create an account</Link>
      </p>
    </Shell>
  )
}

export function Signup() {
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const { refresh } = useAuth()
  const nav = useNavigate()
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.post('/api/auth/signup', { email, password, display_name: name })
      await refresh()
      nav('/app', { replace: true })
    } catch (e: any) {
      setErr(e.message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Shell title="Create your account">
      <form onSubmit={submit}>
        {err && <div className="error">{err}</div>}
        <Field label="Your name">
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} autoFocus autoComplete="name" />
        </Field>
        <Field label="Email">
          <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="email" required />
        </Field>
        <Field label="Password" hint="At least 10 characters.">
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required minLength={10} />
        </Field>
        <button className="btn primary" disabled={busy} style={{ width: '100%' }}>
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

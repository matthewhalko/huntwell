import React, { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { Modal, Spinner } from './ui'

declare global {
  interface Window {
    Stripe?: any
  }
}

/// Loads Stripe's own script once, on demand. Nothing about payments ships in
/// the bundle — the field the card is typed into belongs to Stripe, in an
/// iframe, so the number never reaches this app.
function loadStripeJs(): Promise<any> {
  if (window.Stripe) return Promise.resolve(window.Stripe)
  return new Promise((resolve, reject) => {
    const existing = document.querySelector<HTMLScriptElement>('script[data-stripe]')
    const el = existing || document.createElement('script')
    el.onload = () => (window.Stripe ? resolve(window.Stripe) : reject(new Error('Stripe did not load')))
    el.onerror = () => reject(new Error('Stripe did not load'))
    if (!existing) {
      el.src = 'https://js.stripe.com/v3'
      el.dataset.stripe = '1'
      document.head.appendChild(el)
    }
  })
}

/**
 * Add a card without leaving what you were doing.
 *
 * Opened by the thing that was blocked — pressing Go now with no card on file —
 * so the answer to "you need a card" is the card field itself, and `onDone`
 * resumes whatever was interrupted.
 */
export function CardDialog({ reason, onDone, onClose }: { reason?: string; onDone: () => void; onClose: () => void }) {
  const [err, setErr] = useState('')
  const [ready, setReady] = useState(false)
  const [busy, setBusy] = useState(false)
  const [mocked, setMocked] = useState(false)
  const mount = useRef<HTMLDivElement>(null)
  const stripeRef = useRef<any>(null)
  const elementsRef = useRef<any>(null)

  useEffect(() => {
    let dead = false
    ;(async () => {
      try {
        const r = await api.post<{ publishable_key?: string; client_secret?: string; mocked?: boolean }>('/api/billing/intent', {})
        if (dead) return
        // No Stripe keys on this server: the card is already attached.
        if (r.mocked) {
          setMocked(true)
          setReady(true)
          return
        }
        const Stripe = await loadStripeJs()
        if (dead) return
        const stripe = Stripe(r.publishable_key)
        const elements = stripe.elements({
          clientSecret: r.client_secret,
          appearance: { theme: document.documentElement.dataset.theme === 'dark' ? 'night' : 'stripe' },
        })
        elements.create('payment', { layout: 'tabs' }).mount(mount.current!)
        stripeRef.current = stripe
        elementsRef.current = elements
        setReady(true)
      } catch (e: any) {
        if (!dead) setErr(e.message || 'Could not start card setup')
      }
    })()
    return () => {
      dead = true
    }
  }, [])

  const submit = async (e?: React.FormEvent) => {
    e?.preventDefault()
    if (busy) return
    setBusy(true)
    setErr('')
    try {
      if (mocked) {
        onDone()
        return
      }
      const { error, setupIntent } = await stripeRef.current.confirmSetup({
        elements: elementsRef.current,
        redirect: 'if_required',
      })
      if (error) throw new Error(error.message || 'That card was declined')
      await api.post('/api/billing/attach', { setup_intent_id: setupIntent.id })
      onDone()
    } catch (e: any) {
      setErr(e.message || 'Could not save that card')
      setBusy(false)
    }
  }

  return (
    <Modal title="Add a payment method" onClose={onClose}>
      <p className="muted" style={{ marginTop: 0 }}>
        {reason || 'Runs cost money to execute, so we need a card on file before the first one.'}
      </p>
      <form onSubmit={submit}>
        {mocked ? (
          <p className="notice">
            Card payments aren't set up on this server, so a test card (Visa •••• 4242) has been attached instead.
          </p>
        ) : (
          <div ref={mount} style={{ minHeight: 60, margin: '1rem 0' }} />
        )}
        {!ready && !err && (
          <div className="row" style={{ gap: '0.6rem' }}>
            <Spinner /> <span className="muted">Loading secure card field…</span>
          </div>
        )}
        {err && <div className="error">{err}</div>}
        <p className="muted" style={{ fontSize: '0.82rem' }}>
          Card details go straight to Stripe. We store only the brand and last four digits.
        </p>
        <div className="row" style={{ justifyContent: 'flex-end' }}>
          <button className="btn ghost" type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" type="submit" disabled={!ready || busy}>
            {busy ? 'Saving…' : mocked ? 'Continue' : 'Save card'}
          </button>
        </div>
      </form>
    </Modal>
  )
}

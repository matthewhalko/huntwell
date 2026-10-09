import React, { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api, Billing } from '../api'
import { useAuth } from '../auth'
import { CardDialog } from './CardDialog'
import { CardBrandIcon, cardBrandLabel } from './icons'
import { ModalBackdrop } from './ui'

/// The zone the browser is already in. Nobody is asked for it — a person whose
/// laptop is set to Denver does not want to answer a question about it.
export function browserZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
  } catch {
    return 'UTC'
  }
}

/**
 * What happens next, not a form.
 *
 * It waits until someone actually asks for something — raised by Go now or
 * Build plan, never on arrival. A welcome dialog in front of an empty app is
 * an interruption; the same dialog in front of a search someone just typed is
 * the answer to "why hasn't it started?".
 *
 * `resume` is what they were doing when it opened, run once setup is out of
 * the way. Dismissing it marks setup done: it is a welcome, not a gate.
 */
export default function Setup({ resume, onClose }: { resume?: () => void; onClose?: () => void }) {
  const { me, refresh } = useAuth()
  const nav = useNavigate()
  const [billing, setBilling] = useState<Billing | null>(null)
  const [addingCard, setAddingCard] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api
      .get<Billing>('/api/billing')
      .then(setBilling)
      .catch(() => setBilling(null))
  }, [])

  /// Closing it out. The timezone rides along so schedules are right from the
  /// first plan, without anyone being asked for it.
  const done = async (go?: string, then?: boolean) => {
    if (busy) return
    setBusy(true)
    try {
      await api.put('/api/auth/me', { browser_timezone: browserZone(), onboarded: true })
      await refresh()
      if (then !== false) {
        if (resume) resume()
        else if (go) nav(go)
      }
      onClose?.()
    } catch {
      setBusy(false)
    }
  }
  /// Waved away. Setup is still marked done — it is a welcome, not a gate, and
  /// nagging on every Go now would make it one. Anyone without a card still
  /// meets the card field, which is the check that actually matters.

  const hasCard = !!billing?.has_card
  // Free credit an operator added: no card is needed until it is spent.
  const free = !hasCard && !!billing?.free_credit
  const ready = hasCard || free
  const firstName = (me?.display_name || '').split(' ')[0]

  if (addingCard) {
    return (
      <CardDialog
        reason="Runs cost money to execute, so a card comes first. Nothing is charged until a search actually runs."
        onClose={() => setAddingCard(false)}
        onDone={() => {
          setAddingCard(false)
          api.get<Billing>('/api/billing').then(setBilling).catch(() => {})
        }}
      />
    )
  }

  return (
    <ModalBackdrop className="setup-bg">
      <div className="modal setup">
        <div className="setup-mark">
          <span className="grad-text" aria-hidden>
            ✦
          </span>
          <span className="word">huntwell</span>
        </div>
        <h2>{firstName ? `Welcome, ${firstName}` : 'Welcome'}</h2>
        <p className="muted" style={{ marginTop: '0.2rem' }}>
          Two steps and your first list is on its way.
        </p>

        <ol className="steps">
          <li className={'step' + (ready ? ' on' : '')}>
            <span className="step-mark">{ready ? '✓' : '1'}</span>
            <div className="step-body">
              <div className="step-title">{free ? 'Free credit to start with' : 'Add a payment method'}</div>
              <p className="step-hint">
                {hasCard ? (
                  <span className="pay-method" style={{ marginTop: 0 }}>
                    <CardBrandIcon brand={billing?.card?.brand} size={32} />
                    <span>
                      {cardBrandLabel(billing?.card?.brand)} •••• {billing?.card?.last4} on file. Credits are purchased next — a run spends only what you have preallocated.
                    </span>
                  </span>
                ) : free ? (
                  `You have ${'$' + (billing?.credits_usd ?? 0).toFixed(2)} of credit on us — no card needed until it's used up. You'll be asked for one then.`
                ) : (
                  'Executions cost money, so a card comes first. You then buy credits and spend only those.'
                )}
              </p>
              {!ready && (
                <button className="btn primary sm" onClick={() => setAddingCard(true)}>
                  Add a card
                </button>
              )}
            </div>
          </li>
          <li className={'step' + (ready ? '' : ' waiting')}>
            <span className="step-mark">2</span>
            <div className="step-body">
              <div className="step-title">{resume ? 'Start the search you typed' : 'Create your first search'}</div>
              <p className="step-hint">
                {resume
                  ? 'It is ready to go — this picks up right where you left off.'
                  : "Say what you're looking for in plain words — Huntwell works out how to find it."}
              </p>
              <button className="btn primary sm" disabled={!ready || busy} onClick={() => done('/app')}>
                {resume ? 'Start it' : 'Create a search plan'}
              </button>
            </div>
          </li>
        </ol>

        <div className="row between" style={{ marginTop: '1.1rem' }}>
          <button className="btn ghost sm" disabled={busy} onClick={() => done(undefined, false)}>
            Not right now
          </button>
          <span className="muted" style={{ fontSize: '0.8rem' }}>
            Scheduled searches use {browserZone().replace(/_/g, ' ')} time.
          </span>
        </div>
      </div>
    </ModalBackdrop>
  )
}

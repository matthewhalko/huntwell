import React, { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api, Billing } from '../api'
import { useAuth } from '../auth'
import { CardDialog } from './CardDialog'

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
    <div className="modal-bg setup-bg">
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
          <li className={'step' + (hasCard ? ' on' : '')}>
            <span className="step-mark">{hasCard ? '✓' : '1'}</span>
            <div className="step-body">
              <div className="step-title">Add a payment method</div>
              <p className="step-hint">
                {hasCard
                  ? `${billing?.card?.brand} •••• ${billing?.card?.last4} on file. Nothing is charged until a search runs.`
                  : 'Executions cost money, so a card comes first. Nothing is charged until one runs.'}
              </p>
              {!hasCard && (
                <button className="btn primary sm" onClick={() => setAddingCard(true)}>
                  Add a card
                </button>
              )}
            </div>
          </li>
          <li className={'step' + (hasCard ? '' : ' waiting')}>
            <span className="step-mark">2</span>
            <div className="step-body">
              <div className="step-title">{resume ? 'Start the search you typed' : 'Create your first search'}</div>
              <p className="step-hint">
                {resume
                  ? 'It is ready to go — this picks up right where you left off.'
                  : "Say what you're looking for in plain words — Huntwell works out how to find it."}
              </p>
              <button className="btn primary sm" disabled={!hasCard || busy} onClick={() => done('/app')}>
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
    </div>
  )
}

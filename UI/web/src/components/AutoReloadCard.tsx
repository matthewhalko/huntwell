import React, { useState } from 'react'
import { api, AutoReload, Billing, fmtDate } from '../api'
import { useToast } from './ui'

const usd = (n: number) => '$' + n.toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 })

/**
 * Auto-reload: "when my balance falls below $10, add $50". The server charges
 * the saved card off-session when the balance drops (billing.rs, auto-reload).
 *
 * The wording of the agreement below is version `auto-reload-v1`
 * (billing::AUTO_RELOAD_TERMS). Change one, change both and bump the version.
 */
export function AutoReloadCard({ billing, canPay, onSaved }: { billing: Billing; canPay: boolean; onSaved: () => void }) {
  const toast = useToast()
  const saved: AutoReload = billing.auto_reload || {
    enabled: false,
    below_usd: 0,
    amount_usd: 0,
    agreed_at: null,
    terms: '',
    last_at: null,
    last_error: '',
  }
  const maxPerDay = billing.auto_reload_max_per_day ?? 3
  const [below, setBelow] = useState(saved.below_usd || 10)
  const [amount, setAmount] = useState(saved.amount_usd || 50)
  const [agree, setAgree] = useState(false)
  const [editing, setEditing] = useState(false)
  const [busy, setBusy] = useState(false)

  const card = billing.card
  const changed = !saved.enabled || below !== saved.below_usd || amount !== saved.amount_usd || saved.terms !== billing.auto_reload_terms
  const valid = Number.isInteger(below) && below >= 5 && below <= 500 && Number.isInteger(amount) && amount >= 10 && amount <= 500
  const showForm = canPay && !!card && (!saved.enabled || editing)

  const save = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.put('/api/billing/auto-reload', { enabled, below_usd: below, amount_usd: amount, agree: enabled && agree })
      toast(enabled ? `Auto-reload on — we'll add ${usd(amount)} when your balance falls below ${usd(below)}` : 'Auto-reload off')
      setAgree(false)
      setEditing(false)
      onSaved()
    } catch (e: any) {
      toast(e.message || 'Could not save auto-reload', true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card auto-reload" style={{ marginBottom: '1.4rem' }}>
      <div className="row between" style={{ marginBottom: '0.4rem' }}>
        <h3 style={{ margin: 0 }}>Auto-reload</h3>
        <span className={'badge ' + (saved.enabled ? 'ok' : '')}>{saved.enabled ? 'on' : 'off'}</span>
      </div>

      {saved.last_error && <div className="notice">{saved.last_error}</div>}

      {!card ? (
        <p className="muted" style={{ margin: 0 }}>
          Add a card above to have credit added automatically when your balance runs low.
        </p>
      ) : saved.enabled && !editing ? (
        <>
          <p style={{ margin: '0 0 0.3rem' }}>
            When your balance falls below <b>{usd(saved.below_usd)}</b>, we charge your card ending {card.last4} <b>{usd(saved.amount_usd)}</b>.
          </p>
          <p className="muted sm" style={{ margin: 0 }}>
            At most {maxPerDay} times in 24 hours.
            {saved.agreed_at && ` Agreed ${fmtDate(saved.agreed_at)}.`}
            {saved.last_at && ` Last reloaded ${fmtDate(saved.last_at)}.`}
          </p>
          {canPay && (
            <div className="row" style={{ marginTop: '0.8rem' }}>
              <button className="btn" onClick={() => setEditing(true)} disabled={busy}>
                Change
              </button>
              <button className="btn ghost" onClick={() => save(false)} disabled={busy}>
                Turn off
              </button>
            </div>
          )}
        </>
      ) : !canPay ? (
        <p className="muted" style={{ margin: 0 }}>Only someone allowed to buy credits can set this up.</p>
      ) : (
        <p className="muted" style={{ margin: 0 }}>
          Never run out mid-search: when your balance runs low, we add credit from your card automatically.
        </p>
      )}

      {showForm && (
        <form
          className="auto-reload-form"
          onSubmit={(e) => {
            e.preventDefault()
            save(true)
          }}
        >
          <div className="auto-reload-rule">
            <span>When my balance falls below</span>
            <span className="money-input">
              $<input type="number" min={5} max={500} step={1} value={below} onChange={(e) => setBelow(Math.round(+e.target.value))} aria-label="Balance to reload at, in dollars" />
            </span>
            <span>add</span>
            <span className="money-input">
              $<input type="number" min={10} max={500} step={1} value={amount} onChange={(e) => setAmount(Math.round(+e.target.value))} aria-label="Credit to add, in dollars" />
            </span>
          </div>
          {!valid && <p className="danger sm" style={{ margin: '0.4rem 0 0' }}>Reload at $5–$500, and add $10–$500, in whole dollars.</p>}

          {changed && valid && (
            <label className="check auto-reload-terms">
              <input type="checkbox" checked={agree} onChange={(e) => setAgree(e.target.checked)} />
              <span>
                I authorize Huntwell to charge my {card!.brand ? card!.brand.charAt(0).toUpperCase() + card!.brand.slice(1) : 'card'} ending {card!.last4}{' '}
                <b>{usd(amount)}</b> each time my prepaid balance falls below <b>{usd(below)}</b>, up to {maxPerDay} times in 24 hours, until I turn
                auto-reload off. Each charge buys {usd(amount)} of prepaid credit for searches and drafts.
              </span>
            </label>
          )}

          <div className="row" style={{ marginTop: '0.8rem' }}>
            <button className="btn primary" disabled={busy || !valid || (changed && !agree)}>
              {busy ? 'Saving…' : saved.enabled ? 'Save changes' : 'Turn on auto-reload'}
            </button>
            {editing && (
              <button
                type="button"
                className="btn ghost"
                onClick={() => {
                  setEditing(false)
                  setBelow(saved.below_usd)
                  setAmount(saved.amount_usd)
                  setAgree(false)
                }}
              >
                Cancel
              </button>
            )}
          </div>
        </form>
      )}
    </div>
  )
}

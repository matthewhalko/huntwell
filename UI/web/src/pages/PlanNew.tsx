import React, { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { api, Effort, EFFORTS, Plan } from '../api'
import { BuildingPhrase, useBuildingPhrase, useToast } from '../components/ui'

/**
 * One question, centred on the screen: what are you looking for?
 *
 * The same composition as Home's ask box, because they are the same act — the
 * plan is created the moment it is asked for and drafts itself in the
 * background, so this page hands over to the plan's own page rather than
 * holding the user on a spinner.
 */
export default function PlanNew() {
  const nav = useNavigate()
  const toast = useToast()
  const loc = useLocation() as { state?: { icp?: string } }
  const [brief, setBrief] = useState(loc.state?.icp || '')
  const [name, setName] = useState('')
  const [target, setTarget] = useState(10)
  const [effort, setEffort] = useState<Effort>('normal')
  const [busy, setBusy] = useState(false)
  const note = useBuildingPhrase(busy)
  const [status, setStatus] = useState<any>(null)

  useEffect(() => {
    api.get('/api/status').then(setStatus).catch(() => {})
  }, [])

  /// Creating the plan takes milliseconds; *writing* it takes the agent about
  /// a minute. The box keeps its rainbow border for that whole minute — the
  /// wait belongs to the thing that started it — and only then hands over to
  /// the plan's own page.
  const create = async (e?: React.FormEvent) => {
    e?.preventDefault()
    if (!brief.trim() || busy) return
    setBusy(true)
    try {
      const plan = await api.post<Plan>('/api/plans', { brief: brief.trim(), name: name.trim(), target, effort })
      // Wait for the agent to finish writing the search. Three minutes is far
      // past the agent's own timeout; it only fires if the server went away.
      for (let n = 0; n < 90; n++) {
        const p = await api.get<Plan>(`/api/plans/${plan.PlanId}`)
        if (p.Status === 'failed') throw new Error("That search couldn't be built. Try describing it a different way.")
        if (p.Status !== 'drafting') break
        await new Promise((r) => setTimeout(r, 2000))
      }
      nav(`/app/plans/${plan.PlanId}`, { state: { welcome: true } })
    } catch (e: any) {
      toast(e.message || 'Could not create that plan', true)
    } finally {
      setBusy(false)
    }
  }
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      create()
    }
  }

  const agentReady = !status || (status.agent && status.cursor_key)

  return (
    <div className="ask-hero">
      <h1>
        What are you <span className="grad-text">looking for</span>?
      </h1>
      <p className="sub">Say it in plain words. Huntwell works out how to find it.</p>

      {!agentReady && <div className="error">The AI agent isn't configured on this server, so plans can't be built.</div>}

      <form className={'ask' + (busy ? ' busy' : '')} onSubmit={create}>
        <textarea
          className="ask-input"
          rows={3}
          autoFocus
          maxLength={600}
          placeholder="Hotel GMs on the Oregon coast. Or: a report on Kapital's funding history."
          value={brief}
          onChange={(e) => setBrief(e.target.value)}
          onKeyDown={onKey}
        />
        <div className="ask-actions">
          <span className="muted sm">{note ? <BuildingPhrase text={note} /> : 'Enter to create · Shift+Enter for a new line'}</span>
          <button className="btn primary lg" type="submit" disabled={!brief.trim() || busy || !agentReady}>
            {busy ? note : 'Create search plan'}
          </button>
        </div>
      </form>

      <div className="new-opts">
        <label>
          <span className="ask-label">Name it</span>
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Optional" />
        </label>
        <label>
          <span className="ask-label">Target results</span>
          <input type="number" min={0} max={500} value={target} onChange={(e) => setTarget(+e.target.value)} />
        </label>
        <div>
          <span className="ask-label">How hard to try</span>
          <div className="seg">
            {EFFORTS.map((e) => (
              <button key={e.value} type="button" className={effort === e.value ? 'active' : ''} onClick={() => setEffort(e.value)}>
                {e.label}
              </button>
            ))}
          </div>
        </div>
      </div>
      <p className="muted sm" style={{ marginTop: '0.7rem' }}>{EFFORTS.find((e) => e.value === effort)?.hint}</p>
    </div>
  )
}

import React, { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { api, OutreachDesign, PlanOutreach } from '../api'
import { Field, useToast } from './ui'

/**
 * How outreach drafts to this plan's people are written: the workspace's
 * settings, a saved profile (Outreach → Profiles), or a campaign tailored to
 * this plan — who these people are, the angle, what to offer them, and rules
 * on top of the workspace's.
 *
 * Saved on its own endpoint, apart from the plan's settings form. The text is
 * kept when tailoring is switched off, so switching back loses nothing.
 */
export function PlanOutreachSettings({ planId, canWrite }: { planId: number; canWrite: boolean }) {
  const toast = useToast()
  const [saved, setSaved] = useState<PlanOutreach | null>(null)
  const [custom, setCustom] = useState(false)
  const [designId, setDesignId] = useState<number | ''>('')
  const [designs, setDesigns] = useState<OutreachDesign[]>([])
  const [useProfile, setUseProfile] = useState(false)
  const [brief, setBrief] = useState('')
  const [product, setProduct] = useState('')
  const [rules, setRules] = useState('')
  const [busy, setBusy] = useState(false)

  const load = (v: PlanOutreach) => {
    setSaved(v)
    setCustom(v.custom)
    setDesignId(v.design_id ?? '')
    setUseProfile(!v.custom && v.design_id != null)
    setBrief(v.brief)
    setProduct(v.product)
    setRules(v.rules)
  }

  useEffect(() => {
    api.get<PlanOutreach>(`/api/plans/${planId}/outreach`).then(load).catch(() => setSaved(null))
    api
      .get<{ designs: OutreachDesign[] }>('/api/outreach/designs')
      .then((r) => setDesigns(r.designs))
      .catch(() => setDesigns([]))
  }, [planId])

  if (!saved) return null

  const mode: 'workspace' | 'profile' | 'custom' = custom ? 'custom' : useProfile ? 'profile' : 'workspace'
  const pickMode = (m: typeof mode) => {
    setCustom(m === 'custom')
    setUseProfile(m === 'profile')
    if (m === 'profile' && designId === '' && designs.length) setDesignId(designs[0].design_id)
  }
  // Only the profile choice keeps a profile; the other two clear it.
  const sentDesign = mode === 'profile' && designId !== '' ? designId : null
  const dirty =
    custom !== saved.custom || sentDesign !== (saved.custom ? null : saved.design_id) || brief !== saved.brief || product !== saved.product || rules !== saved.rules
  const empty = !brief.trim() && !product.trim() && !rules.trim()
  const ws = saved.workspace

  const save = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    try {
      load(await api.put<PlanOutreach>(`/api/plans/${planId}/outreach`, { custom, design_id: sentDesign, brief, product, rules }))
      toast('Outreach saved')
    } catch (err: any) {
      toast(err.message || 'Could not save', true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <form onSubmit={save} className="card plan-extra-card">
      <h3>Outreach for this plan</h3>
      <p className="muted" style={{ marginTop: 0 }}>
        How cold emails to the people this plan finds are written. Drafts to its results use this automatically.
      </p>

      <div className="seg" style={{ marginBottom: '0.9rem' }}>
        <button type="button" className={mode === 'workspace' ? 'active' : ''} onClick={() => pickMode('workspace')} disabled={!canWrite}>
          Workspace's outreach
        </button>
        <button type="button" className={mode === 'profile' ? 'active' : ''} onClick={() => pickMode('profile')} disabled={!canWrite}>
          A saved profile
        </button>
        <button type="button" className={mode === 'custom' ? 'active' : ''} onClick={() => pickMode('custom')} disabled={!canWrite}>
          Tailor it for this plan
        </button>
      </div>

      {mode === 'profile' ? (
        designs.length === 0 ? (
          <div className="outreach-fallback">
            <p className="muted sm" style={{ margin: 0 }}>
              No saved profiles yet — make one under <Link to="/app/outreach?tab=profiles">Outreach → Profiles</Link>.
            </p>
          </div>
        ) : (
          <Field label="Profile" hint="Drafts to this plan's people are written with it. Edit it under Outreach → Profiles.">
            <select value={designId} onChange={(e) => setDesignId(e.target.value ? +e.target.value : '')} disabled={!canWrite}>
              {designs.map((d) => (
                <option key={d.design_id} value={d.design_id}>
                  {d.name}
                </option>
              ))}
            </select>
          </Field>
        )
      ) : !custom ? (
        <div className="outreach-fallback">
          <p className="muted sm" style={{ margin: 0 }}>
            {ws.product.trim() || ws.rules.trim() ? (
              <>
                Drafts use your workspace's description and rules, set under{' '}
                <Link to="/app/outreach">Outreach → Settings</Link>.
              </>
            ) : (
              <>
                Your workspace has no outreach settings yet — add them under <Link to="/app/outreach">Outreach → Settings</Link>, or
                tailor this plan's own.
              </>
            )}
          </p>
        </div>
      ) : (
        <>
          <Field
            label="Who this campaign is for, and the angle"
            hint="Who these people are, why you're writing to them now, and what you want from the email."
          >
            <textarea
              rows={4}
              value={brief}
              maxLength={4000}
              disabled={!canWrite}
              onChange={(e) => setBrief(e.target.value)}
              placeholder="Owners of boutique hotels on the Oregon coast who renovated in the last year. Lead with how we help a newly reopened hotel fill its first season. Ask for a 15-minute call."
            />
          </Field>
          <Field
            label="What you're offering them"
            hint={
              ws.product.trim()
                ? "Leave blank to use your workspace's description. Fill it in to pitch something different to this group."
                : 'Your product, as it matters to these people.'
            }
          >
            <textarea
              rows={3}
              value={product}
              maxLength={4000}
              disabled={!canWrite}
              onChange={(e) => setProduct(e.target.value)}
              placeholder={ws.product.trim() ? ws.product.trim().slice(0, 300) : 'We make booking software for independent hotels…'}
            />
          </Field>
          <Field
            label="Rules for this campaign"
            hint={
              ws.rules.trim()
                ? "Added to your workspace's rules. Where they disagree, these win."
                : 'How these emails should read.'
            }
          >
            <textarea
              rows={3}
              value={rules}
              maxLength={4000}
              disabled={!canWrite}
              onChange={(e) => setRules(e.target.value)}
              placeholder={'Mention their renovation. Under 90 words.\nNo discount talk.'}
            />
          </Field>
          {ws.rules.trim() && (
            <details className="outreach-fallback">
              <summary className="muted sm">Your workspace's rules, which still apply</summary>
              <pre className="outreach-rules">{ws.rules}</pre>
            </details>
          )}
        </>
      )}

      {canWrite && (
        <div className="row" style={{ marginTop: '1rem' }}>
          <button className="btn primary" type="submit" disabled={busy || !dirty || (custom && empty) || (mode === 'profile' && sentDesign === null)}>
            {busy ? 'Saving…' : 'Save outreach'}
          </button>
          {custom && empty && <span className="muted sm">Fill in at least one field to tailor it.</span>}
        </div>
      )}
    </form>
  )
}

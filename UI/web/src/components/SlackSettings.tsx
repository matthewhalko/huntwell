import React, { useEffect, useState } from 'react'
import { api, PlanSlack } from '../api'
import { Field, useToast } from './ui'

/**
 * Post a plan's new results into a Slack channel through an incoming webhook.
 *
 * Saved on its own, apart from the plan's settings form: it has its own
 * endpoint, and a webhook is a credential — the server keeps it sealed and only
 * ever sends back a hint of it, so the field starts empty with that hint shown.
 */
export function SlackSettings({ planId, canWrite }: { planId: number; canWrite: boolean }) {
  const toast = useToast()
  const [s, setS] = useState<PlanSlack | null>(null)
  const [webhook, setWebhook] = useState('')
  const [enabled, setEnabled] = useState(false)
  const [layout, setLayout] = useState<'auto' | 'all'>('auto')
  const [limit, setLimit] = useState(10)
  const [busy, setBusy] = useState<'' | 'save' | 'test' | 'remove'>('')

  const load = (v: PlanSlack) => {
    setS(v)
    setEnabled(v.enabled)
    setLayout(v.layout === 'all' ? 'all' : 'auto')
    setLimit(v.limit || 10)
    setWebhook('')
  }

  useEffect(() => {
    api.get<PlanSlack>(`/api/plans/${planId}/slack`).then(load).catch(() => setS(null))
  }, [planId])

  if (!s) return null

  const typed = webhook.trim()
  const dirty = !!typed || enabled !== s.enabled || layout !== s.layout || limit !== s.limit
  const max = s.max_limit || 40

  const save = async (e?: React.FormEvent) => {
    e?.preventDefault()
    setBusy('save')
    try {
      const saved = await api.put<PlanSlack>(`/api/plans/${planId}/slack`, {
        ...(typed ? { webhook: typed } : {}),
        // Pasting a webhook is asking for it to be used.
        enabled: enabled || (!!typed && !s.configured),
        layout,
        limit,
      })
      load(saved)
      toast('Slack settings saved')
    } catch (err: any) {
      toast(err.message || 'Could not save', true)
    } finally {
      setBusy('')
    }
  }

  const test = async () => {
    setBusy('test')
    try {
      await api.post(`/api/plans/${planId}/slack/test`, typed ? { webhook: typed } : {})
      toast('Test message sent — check the channel')
    } catch (err: any) {
      toast(err.message || 'Slack did not accept the test message', true)
    } finally {
      setBusy('')
    }
  }

  const remove = async () => {
    setBusy('remove')
    try {
      load(await api.put<PlanSlack>(`/api/plans/${planId}/slack`, { webhook: '', enabled: false }))
      toast('Slack disconnected')
    } catch (err: any) {
      toast(err.message || 'Could not remove it', true)
    } finally {
      setBusy('')
    }
  }

  return (
    <form onSubmit={save} className="card slack-card">
      <h3>Post results to Slack</h3>
      <p className="muted" style={{ marginTop: 0 }}>
        After each run that finds something new, Huntwell posts the new results in a Slack channel. Nothing is posted for a run that finds nothing.
      </p>

      {s.last_error && (
        <div className="error">
          The last post didn't go through: {s.last_error}
        </div>
      )}

      <Field
        label="Webhook URL"
        hint={
          s.configured
            ? `Saved: ${s.hint}. Paste a new one to replace it.`
            : 'In Slack: Apps → Incoming Webhooks → Add to Slack, pick the channel, and copy the URL it gives you.'
        }
      >
        <input
          type="url"
          value={webhook}
          onChange={(e) => setWebhook(e.target.value)}
          placeholder={s.configured ? '••••••••  (saved)' : 'https://hooks.slack.com/services/…'}
          autoComplete="off"
          spellCheck={false}
          disabled={!canWrite}
        />
      </Field>

      <Field label="Post">
        <label className="check">
          <input type="checkbox" checked={enabled} disabled={!canWrite || (!s.configured && !typed)} onChange={(e) => setEnabled(e.target.checked)} /> Post new
          results to this channel
        </label>
      </Field>

      <Field
        label="What each post contains"
        hint={
          layout === 'auto'
            ? `Up to ${limit} new results go in one post. More than that, and the post shows the first few with a link to see them all in Huntwell.`
            : 'Every new result, split over several posts when there are many.'
        }
      >
        <div className="seg">
          <button type="button" className={layout === 'auto' ? 'active' : ''} onClick={() => setLayout('auto')} disabled={!canWrite}>
            Results, or a link when there are many
          </button>
          <button type="button" className={layout === 'all' ? 'active' : ''} onClick={() => setLayout('all')} disabled={!canWrite}>
            Always every result
          </button>
        </div>
      </Field>

      {layout === 'auto' && (
        <Field label="Most results in one post" hint={`1 to ${max}.`}>
          <input
            type="number"
            min={1}
            max={max}
            value={limit}
            onChange={(e) => setLimit(Math.max(1, Math.min(max, Math.round(+e.target.value) || 1)))}
            style={{ width: 140 }}
            disabled={!canWrite}
          />
        </Field>
      )}

      {canWrite && (
        <div className="row">
          <button className="btn primary" type="submit" disabled={!!busy || !dirty}>
            {busy === 'save' ? 'Saving…' : 'Save Slack settings'}
          </button>
          <button className="btn" type="button" onClick={test} disabled={!!busy || (!s.configured && !typed)}>
            {busy === 'test' ? 'Sending…' : 'Send a test message'}
          </button>
          {s.configured && (
            <button className="btn ghost" type="button" onClick={remove} disabled={!!busy}>
              {busy === 'remove' ? 'Removing…' : 'Disconnect'}
            </button>
          )}
        </div>
      )}
    </form>
  )
}

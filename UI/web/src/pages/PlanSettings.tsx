import React, { useState } from 'react'
import { api, Effort, EFFORTS, Plan } from '../api'
import { Field, useToast } from '../components/ui'

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']

/**
 * Everything about a plan a person owns: what to call it, what they asked for,
 * how many results to aim for, and when it should run itself.
 *
 * What is deliberately absent: the scrape, enrich and planner prompts, the
 * field mapping and the dedupe key. Those are drafted by the agent, live only
 * on the server, and are never sent to a browser — so there is nothing here to
 * get wrong, and nothing to leak.
 */
export function PlanSettings({ plan, onSaved }: { plan: Plan; onSaved: (p: Plan) => void }) {
  const toast = useToast()
  const [name, setName] = useState(plan.Source)
  const [description, setDescription] = useState(plan.Description)
  const [target, setTarget] = useState(plan.TargetProspects)
  const [effort, setEffort] = useState<Effort>(plan.Effort || 'normal')
  const [enabled, setEnabled] = useState(plan.ScheduleEnabled)
  const [time, setTime] = useState(plan.ScheduleTime || '09:00')
  const [days, setDays] = useState(plan.ScheduleDays)
  const [alert, setAlert] = useState(!!plan.AlertEmail)
  const [busy, setBusy] = useState(false)

  const daySet = new Set(
    days
      .split(',')
      .map((d) => parseInt(d, 10))
      .filter((d) => !isNaN(d)),
  )
  const cadence: 'off' | 'daily' | 'weekly' | 'custom' = !enabled
    ? 'off'
    : daySet.size === 0
      ? 'daily'
      : daySet.size === 1
        ? 'weekly'
        : 'custom'

  const pick = (c: 'off' | 'daily' | 'weekly' | 'custom') => {
    if (c === 'off') {
      setEnabled(false)
      return
    }
    setEnabled(true)
    if (c === 'daily') setDays('')
    if (c === 'weekly') setDays(String(daySet.size ? Math.min(...Array.from(daySet)) : 1))
    if (c === 'custom' && daySet.size < 2) setDays('1,4')
  }
  const toggleDay = (i: number) => {
    const next = new Set(daySet)
    next.has(i) ? next.delete(i) : next.add(i)
    setDays(Array.from(next).sort().join(','))
  }

  const save = async (e?: React.FormEvent) => {
    e?.preventDefault()
    setBusy(true)
    try {
      const saved = await api.put<Plan>(`/api/plans/${plan.PlanId}`, {
        name: name.trim(),
        description: description.trim(),
        target,
        effort,
        schedule_enabled: enabled,
        schedule_time: time,
        schedule_days: days,
        alert_email: alert,
      })
      onSaved(saved)
      toast('Saved')
    } catch (err: any) {
      toast(err.message || 'Could not save', true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <form onSubmit={save} className="card">
      <Field label="Name">
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </Field>
      <Field label="What this plan looks for" hint="In your own words. Huntwell works out how to find it.">
        <textarea rows={3} value={description} onChange={(e) => setDescription(e.target.value)} maxLength={600} />
      </Field>
      <Field label="Target results each run" hint="0 means keep going until the search runs dry.">
        <input type="number" min={0} max={500} value={target} onChange={(e) => setTarget(+e.target.value)} style={{ width: 140 }} />
      </Field>

      <Field label="How hard should it try?" hint={EFFORTS.find((e) => e.value === effort)?.hint}>
        <div className="seg">
          {EFFORTS.map((e) => (
            <button key={e.value} type="button" className={effort === e.value ? 'active' : ''} onClick={() => setEffort(e.value)}>
              {e.label}
            </button>
          ))}
        </div>
      </Field>

      <Field label="Email me when it finds something new" hint="Only when a run adds rows this plan had not seen before. Nothing sent for a run that finds nothing.">
        <label className="check">
          <input type="checkbox" checked={alert} onChange={(e) => setAlert(e.target.checked)} /> Send an alert
        </label>
      </Field>

      <Field label="Run this automatically">
        <div className="seg">
          {(
            [
              ['off', 'Off'],
              ['daily', 'Once a day'],
              ['weekly', 'Once a week'],
              ['custom', 'Custom days'],
            ] as const
          ).map(([v, l]) => (
            <button key={v} type="button" className={cadence === v ? 'active' : ''} onClick={() => pick(v)}>
              {l}
            </button>
          ))}
        </div>
      </Field>
      {cadence !== 'off' && (
        <div className="form-grid">
          <Field label="Time of day" hint="Local time in your timezone (Settings).">
            <input type="time" value={time} onChange={(e) => setTime(e.target.value)} />
          </Field>
          {(cadence === 'weekly' || cadence === 'custom') && (
            <Field label={cadence === 'weekly' ? 'Day of the week' : 'Days it runs'}>
              <div className="row">
                {DAYS.map((d, i) => (
                  <button
                    key={d}
                    type="button"
                    className={'btn sm' + (daySet.has(i) ? ' dark' : '')}
                    onClick={() => (cadence === 'weekly' ? setDays(String(i)) : toggleDay(i))}
                  >
                    {d}
                  </button>
                ))}
              </div>
            </Field>
          )}
        </div>
      )}
      <p className="muted">A slot is skipped, never queued, if the previous run is still going.</p>

      <hr />
      <button className="btn primary" type="submit" disabled={busy || !name.trim()}>
        {busy ? 'Saving…' : 'Save changes'}
      </button>
    </form>
  )
}

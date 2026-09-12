import React, { useEffect, useMemo, useState } from 'react'
import { api, CatalogModel, ModelCatalog, Plan, PlanKind, PlanStageModels } from '../api'
import { Picker, PickOption } from './Picker'
import { Field, useToast } from './ui'

type Stage = keyof PlanStageModels

function scrapeLabel(kind: PlanKind): { title: string; hint: string } {
  if (kind === 'report') return { title: 'Research', hint: 'The model that reads sources and writes the report.' }
  if (kind === 'assets') return { title: 'Find files', hint: 'The model that looks for files about the subject.' }
  return { title: 'Search', hint: 'Reads pages and pulls results out. This is usually where the bill is.' }
}

function stagesFor(plan: Plan): { key: Stage; title: string; hint: string }[] {
  const out: { key: Stage; title: string; hint: string }[] = [{ key: 'scrape', ...scrapeLabel(plan.Kind) }]
  if (plan.Kind !== 'report' && plan.Kind !== 'assets') {
    out.push({
      key: 'enrich',
      title: 'Enrichment',
      hint: 'Fills in a result from its own page. One short page per row.',
    })
  }
  if (plan.Effort !== 'quick' && plan.Kind !== 'report' && plan.Kind !== 'assets') {
    out.push({
      key: 'planner',
      title: 'Next search',
      hint: 'Proposes the next angle when a run is learning as it goes.',
    })
  }
  return out
}

function money(n: number): string {
  if (n >= 10) return `$${n.toFixed(n % 1 === 0 ? 0 : 2)}`
  return `$${n.toFixed(n >= 1 && n % 1 === 0 ? 0 : 2)}`
}

export function priceLine(m?: CatalogModel | null): string {
  if (!m || m.sell_input_per_m == null || m.sell_output_per_m == null) return ''
  return `${money(m.sell_input_per_m)} in · ${money(m.sell_output_per_m)} out / M`
}

function emptyModels(): PlanStageModels {
  return { scrape: '', enrich: '', planner: '' }
}

/**
 * Per-stage model pickers. Saves on change so Advanced is not a second Edit tab.
 */
export function PlanModels({ plan, onSaved }: { plan: Plan; onSaved: (p: Plan) => void }) {
  const toast = useToast()
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null)
  const [busy, setBusy] = useState<Stage | null>(null)
  const chosen = plan.Models || emptyModels()

  useEffect(() => {
    api
      .get<ModelCatalog>('/api/models')
      .then(setCatalog)
      .catch(() => setCatalog({ markup: 1.3, models: [] }))
  }, [])

  const byId = useMemo(() => {
    const map = new Map<string, CatalogModel>()
    for (const m of catalog?.models || []) map.set(m.id, m)
    return map
  }, [catalog])

  const optionsFor = (stage: Stage): PickOption[] => {
    const current = chosen[stage] || ''
    const listed = catalog?.models || []
    const known = listed.some((m) => m.id === current)
    const extras: PickOption[] =
      current && !known ? [{ value: current, label: current, detail: priceLine(byId.get(current)) }] : []
    return [
      { value: '', label: 'Default (Cursor chooses)' },
      ...extras,
      ...listed.map((m) => ({ value: m.id, label: m.label, detail: priceLine(m) })),
    ]
  }

  const pick = async (stage: Stage, id: string) => {
    setBusy(stage)
    try {
      const saved = await api.put<Plan>(`/api/plans/${plan.PlanId}`, {
        models: { ...chosen, [stage]: id },
      })
      onSaved(saved)
    } catch (e: any) {
      toast(e.message || 'Could not save that model', true)
    } finally {
      setBusy(null)
    }
  }

  const pct = catalog ? Math.round((catalog.markup - 1) * 100) : 30
  const rows = stagesFor(plan)

  return (
    <div className="card plan-adv">
      <h3>Models</h3>
      <p className="plan-adv-note">
        Prices are Cursor’s published rates plus a {pct}% Huntwell fee, per million tokens. A new run
        picks up a change; one already going does not.
      </p>
      <div className="plan-adv-grid">
        {rows.map((row) => {
          const current = byId.get(chosen[row.key] || '')
          return (
            <Field
              key={row.key}
              label={row.title}
              hint={
                busy === row.key
                  ? 'Saving…'
                  : chosen[row.key] && current
                    ? `${priceLine(current)}. ${row.hint}`
                    : row.hint
              }
            >
              <Picker
                value={chosen[row.key] || ''}
                options={optionsFor(row.key)}
                onChange={(v) => pick(row.key, v)}
                placeholder="Default (Cursor chooses)"
                searchFrom={6}
              />
            </Field>
          )
        })}
      </div>
    </div>
  )
}

export function planHasCustomModels(plan: Plan): boolean {
  const m = plan.Models
  if (!m) return false
  return !!(m.scrape || m.enrich || m.planner)
}

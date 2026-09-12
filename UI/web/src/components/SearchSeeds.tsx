import React from 'react'
import { ago, api } from '../api'
import { Badge, useConfirm, useToast } from './ui'

/**
 * The plan's starting angles, as cards a person can read.
 *
 * The database stores each seed as a JSON bag of vars. That is how the runner
 * consumes it — not how anyone should have to look at it. A card is the seed
 * as a starting place: what to try, whether it is waiting or already used,
 * and what that try found.
 */
export interface SeedRow {
  seed_json: string
  status: string
  iteration: number | null
  new_prospects: number | null
  queued_at: string
  explored_at: string
}

const HIDDEN = new Set([
  '_prompt_hash',
  '_scrape_prompt',
  'known_companies',
  'known_companies_csv',
  'known_people',
  'known_people_csv',
  'explored_seeds',
  'explored_seeds_csv',
])

type Field = { key: string; label: string; value: string }

function fieldsFromSeed(raw: string): Field[] {
  let obj: unknown
  try {
    obj = JSON.parse(raw || '{}')
  } catch {
    return []
  }
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return []
  const out: Field[] = []
  for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
    if (HIDDEN.has(k) || k.startsWith('_')) continue
    const value = displayVal(v)
    if (!value) continue
    out.push({ key: k, label: prettyKey(k), value })
  }
  return out
}

function displayVal(v: unknown): string {
  if (v == null) return ''
  if (typeof v === 'string') return v.trim()
  if (typeof v === 'number' || typeof v === 'boolean') return String(v)
  if (Array.isArray(v)) return v.filter((x) => x != null && String(x).trim()).slice(0, 3).join(', ')
  return ''
}

function prettyKey(k: string) {
  const spaced = k.replace(/_/g, ' ').replace(/([a-z])([A-Z])/g, '$1 $2')
  return spaced.replace(/^./, (c) => c.toUpperCase())
}

function seedTitle(fields: Field[], raw: string) {
  const vals = fields.map((f) => f.value).filter((v) => v.length > 0 && v.length < 48)
  if (vals.length) return vals.slice(0, 3).join(' · ')
  const snippet = raw.trim()
  if (!snippet || snippet === '{}') return 'Starting search'
  return snippet.length > 72 ? snippet.slice(0, 72) + '…' : snippet
}

export function seedLabel(raw: string) {
  return seedTitle(fieldsFromSeed(raw), raw)
}

export function SearchSeeds({ planId, rows, onChange }: { planId: number; rows: SeedRow[]; onChange: () => void }) {
  const toast = useToast()
  const confirm = useConfirm()
  const waiting = rows.filter((r) => r.status === 'pending')
  const used = rows.filter((r) => r.status !== 'pending')
  const ordered = [...waiting, ...used]

  const clearWaiting = async () => {
    if (
      !(await confirm({
        title: 'Clear waiting seeds?',
        body: 'Angles that have not run yet will be dropped. Seeds already used stay on the plan.',
        confirm: 'Clear waiting',
      }))
    ) {
      return
    }
    await api.del(`/api/plans/${planId}/queue`)
    toast('Waiting seeds cleared')
    onChange()
  }

  return (
    <section className="seeds">
      <div className="seeds-head">
        <div>
          <h3>Search seeds</h3>
          <p className="seeds-lede">
            A seed is a starting angle for a run — a city, segment, or keyword to try. Waiting seeds are used next.
            Used seeds show what that angle found.
          </p>
        </div>
        {waiting.length > 0 && (
          <button className="btn sm" type="button" onClick={clearWaiting}>
            Clear waiting
          </button>
        )}
      </div>
      {ordered.length === 0 ? (
        <div className="card seeds-empty">
          <p>
            No search seeds yet. The first run starts from the plan itself; later runs add new angles here for the next
            try.
          </p>
        </div>
      ) : (
        <div className="seed-list">
          {ordered.map((r, i) => (
            <SeedCard key={`${r.status}-${r.queued_at}-${r.explored_at}-${i}`} row={r} />
          ))}
        </div>
      )}
    </section>
  )
}

function SeedCard({ row }: { row: SeedRow }) {
  const waiting = row.status === 'pending'
  const fields = fieldsFromSeed(row.seed_json)
  const found = row.new_prospects
  return (
    <article className={'seed-card' + (waiting ? ' waiting' : ' used')}>
      <div className="seed-card-top">
        <Badge kind={waiting ? 'warn' : 'ok'}>{waiting ? 'Waiting' : 'Used'}</Badge>
        {!waiting && found != null && (
          <span className={'seed-found' + (found > 0 ? '' : ' none')}>{found > 0 ? `Found ${found}` : 'Nothing new'}</span>
        )}
      </div>
      <h4 className="seed-title">{seedTitle(fields, row.seed_json)}</h4>
      {fields.length > 0 && (
        <ul className="seed-vars">
          {fields.map((f) => (
            <li key={f.key} className="seed-var">
              <span className="seed-var-k">{f.label}</span> {f.value}
            </li>
          ))}
        </ul>
      )}
      <p className="seed-foot">
        {waiting
          ? row.queued_at
            ? `Waiting since ${ago(row.queued_at)} — used on the next run`
            : 'Used on the next run'
          : [
              row.explored_at ? `Tried ${ago(row.explored_at)}` : '',
              row.iteration != null && row.iteration > 0 ? `round ${row.iteration}` : '',
            ]
              .filter(Boolean)
              .join(' · ') || 'Already tried'}
      </p>
    </article>
  )
}

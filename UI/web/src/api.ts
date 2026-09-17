// Thin fetch wrapper. Same-origin, cookie sessions; a 401 anywhere sends the
// app back to sign-in.

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

let onUnauthorized: (() => void) | null = null
export function setUnauthorizedHandler(fn: () => void) {
  onUnauthorized = fn
}

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  })
  if (res.status === 401 && !url.startsWith('/api/auth/')) onUnauthorized?.()
  const text = await res.text()
  let data: any = null
  try {
    data = text ? JSON.parse(text) : null
  } catch {
    data = { error: text }
  }
  if (!res.ok) throw new ApiError(res.status, data?.error || `${res.status} ${res.statusText}`)
  return data as T
}

export const api = {
  get: <T>(url: string) => request<T>('GET', url),
  post: <T>(url: string, body?: unknown) => request<T>('POST', url, body ?? {}),
  put: <T>(url: string, body?: unknown) => request<T>('PUT', url, body ?? {}),
  del: <T>(url: string) => request<T>('DELETE', url),
}

// ---- types (mirroring the Rust side) ----

/// `GET /api/auth/config`: what the sign-in and sign-up pages need before
/// anyone is signed in. A site key means the server will demand a Turnstile
/// token with those forms.
export interface AuthConfig {
  open_signup: boolean
  turnstile_site_key: string | null
}

export interface Me {
  account_id: number
  email: string
  display_name: string
  timezone: string
  /// While true the zone follows the browser; false once picked by hand.
  timezone_auto: boolean
  /// Plan kinds this workspace may create, from the server.
  kinds?: string[]
  /// Whether they claimed the right to collect from the restricted platforms.
  platform_ack?: boolean
  platform_ack_at?: string | null
  theme: 'light' | 'dark' | 'system'
  created_at: string
  /// False until the link in the verification email is followed; the app
  /// shows the check-your-email screen instead of anything else until then.
  email_verified: boolean
  open_signup: boolean
  /// False until first-run setup is done; the app shows the setup dialog on it.
  onboarded: boolean
  /// The workspace this session is working in — theirs, or a shared one.
  workspace_id: number
  own_workspace: boolean
}

/// A person with access to a workspace.
export interface Member {
  account_id: number
  email: string
  display_name: string
  role: string
  joined_at: string
  owner: boolean
}

export interface InviteRow {
  invite_id: number
  email: string
  role: string
  token: string
  created_at: string
  expires_at: string
}

export interface Team {
  workspace_id: number
  /// What this workspace is called — its own name, or whose it is.
  workspace: string
  your_role: string
  can_invite: boolean
  members: Member[]
  invites: InviteRow[]
}

/// A workspace this person can work in: their own, or one they were invited to.
export interface Workspace {
  workspace_id: number
  name: string
  role: string
  own: boolean
}

/// The card on file. `has_card` is what gates a run — no card, no work.
export interface Billing {
  stripe: boolean
  has_card: boolean
  card: { brand: string; last4: string; added_at: string | null } | null
}

// A saved authenticated login (a Browserbase Context connection).
export interface BrowserConnection {
  site: string
  url: string
  connected_at: string
}

// A column of a custom-artifact plan's schema.
export interface FieldSpec {
  key: string
  label: string
  type: 'text' | 'longtext' | 'number' | 'money' | 'url' | 'date'
  role?: '' | 'key' | 'title' | 'url'
}

// One collected artifact (custom-schema row). `fields` is keyed by schema key.
export interface Artifact {
  artifact_id: number
  plan_id: number
  source_key: string
  title: string
  url: string
  fields: Record<string, string | number | boolean | null>
  source: string
  first_seen_utc: string
  last_seen_utc: string
}

// One synthesized report document (Kind='report').
export interface Report {
  report_id: number
  plan_id: number
  execution_id: number | null
  source_key: string
  subject: string
  title: string
  markdown: string
  sources: { title?: string; url?: string }[]
  word_count: number
  source: string
  first_seen_utc: string
  last_seen_utc: string
}

// One collected file (Kind='assets'). Bytes live in the object store; download
// them from /api/assets/{asset_id}.
export interface Asset {
  asset_id: number
  plan_id: number
  execution_id: number | null
  source_key: string
  title: string
  source_url: string
  filename: string
  content_type: string
  size_bytes: number
  source: string
  first_seen_utc: string
  last_seen_utc: string
}

export type PlanKind = 'prospects' | 'artifacts' | 'report' | 'assets' | ''

/// How hard a run tries. One choice standing in for the iteration count and
/// the give-up threshold behind it.
export type Effort = 'quick' | 'normal' | 'thorough' | 'exhaustive'

export const EFFORTS: { value: Effort; label: string; hint: string }[] = [
  { value: 'quick', label: 'Quick look', hint: 'One pass. Fastest and cheapest.' },
  { value: 'normal', label: 'Normal', hint: 'A few angles, stopping when two come up empty.' },
  { value: 'thorough', label: 'Thorough', hint: 'Many angles and deeper result pages.' },
  { value: 'exhaustive', label: 'Exhaustive', hint: 'Keeps hunting until it repeatedly finds nothing new.' },
]

/// A plan as its owner sees it: what they asked for, what it collects, when it
/// runs. The prompts, field mapping and dedupe key that make it work are
/// drafted server-side and never sent to the browser.
export interface Plan {
  PlanId: number
  Source: string
  /// The brief, in the user's own words.
  Description: string
  Kind: PlanKind
  /// For 'report' and 'assets': the one subject the run is about.
  Subject: string
  TargetProspects: number
  /// How hard it tries — the iteration count and give-up threshold behind it.
  Effort: Effort
  /// 'drafting' while the search is being written, then 'ready' — or 'failed'
  /// if the agent could not write it.
  Status: 'drafting' | 'ready' | 'failed'
  Favorite: boolean
  AlertEmail: boolean
  ScheduleEnabled: boolean
  ScheduleTime: string
  ScheduleDays: string
  NextRunAt: string | null
  UpdatedAt: string
  /// Whether it can run at all.
  Ready: boolean
  /// Per-stage Cursor model ids. Empty means the install default / Cursor.
  Models?: PlanStageModels
}

export interface PlanStageModels {
  scrape: string
  enrich: string
  planner: string
}

export interface CatalogModel {
  id: string
  label: string
  cursor_input_per_m: number | null
  cursor_output_per_m: number | null
  sell_input_per_m: number | null
  sell_output_per_m: number | null
}

export interface ModelCatalog {
  markup: number
  models: CatalogModel[]
}

export interface PlanSummary extends Plan {
  prospects: number
  executions: number
  /// What this plan has cost across every run.
  tokens: number
  spend_usd: number
  last_execution_at: string
  last_execution_status: string
  active_execution_id: number | null
}

export interface Run {
  execution_id: number
  plan_id: number
  source: string
  status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
  trigger: string
  started_at: string
  finished_at: string | null
  exit_code: number | null
  args_json: any
  new_prospects: number
  /// Billable tokens this run spent, and what that comes to at your rate.
  tokens: number
  cost_usd: number
  /// Null when the run found nothing — kept distinct from "cheap per result".
  cost_per_result: number | null
  /// True when this run has a remote browser that Watch can open.
  watchable?: boolean
}

/// Dollars at the precision small amounts need: $0.04 rather than $0.00.
export function usdFine(n: number): string {
  if (!n) return '$0'
  return n < 1 ? `$${n.toFixed(2)}` : `$${n.toFixed(2)}`
}

export interface LogLine {
  seq: number
  ts: string
  stream: 'stdout' | 'stderr'
  line: string
}

export interface Prospect {
  prospect_id: number
  plan_id: number
  name: string
  title: string
  company: string
  industry: string
  email: string
  email_status: string
  phone: string
  website: string
  linkedin: string
  location: string
  notes: string
  estimated_value: number | null
  source: string
  source_key: string
  first_seen_utc: string
  last_seen_utc: string
}

export interface ApiKey {
  key_id: number
  token: string
  token_hint: string
  label: string
  plan_id: number | null
  source: string
  allow_cidr: string
  expires_at: string
  created_at: string
  last_used_at: string
  last_used_ip: string
  uses: number
  revoked: boolean
  expired: boolean
}

export interface Overview {
  overview: { plans: number; prospects: number; prospects_7d: number; executions: number; active_executions: number; last_execution_at: string }
  recent_executions: Run[]
  latest_prospects: Prospect[]
}

// The dollar budget for the current billing period (the usage meter + cap).
export interface Usage {
  budget_usd: number
  topups_usd: number
  available_usd: number
  used_usd: number
  remaining_usd: number
  tokens_used: number
  cogs_usd: number
  period_start: string
}

// Dollars, to cents: 12.5 → "$12.50".
export function usd(n: number): string {
  return `$${(n ?? 0).toFixed(2)}`
}

// Compact token counts: 1_240_000 → "1.2M".
export function fmtTokens(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`
  if (n >= 1e3) return `${Math.round(n / 1e3)}K`
  return `${n}`
}

export function fmtDate(s?: string | null): string {
  if (!s) return '—'
  const d = new Date(s)
  if (isNaN(d.getTime())) return s
  return d.toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

export function ago(s?: string | null): string {
  if (!s) return 'never'
  const t = new Date(s).getTime()
  if (isNaN(t)) return s
  const diff = Math.max(0, Date.now() - t) / 1000
  if (diff < 60) return 'just now'
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

// Human label for a plan's schedule: "daily 09:00", "Mondays 09:00", "Mon Thu 09:00".
const DOW = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
export function scheduleLabel(p: { ScheduleEnabled: boolean; ScheduleTime: string; ScheduleDays: string }): string {
  if (!p.ScheduleEnabled) return ''
  const days = p.ScheduleDays.split(',')
    .map((s) => s.trim())
    .filter(Boolean)
    .map(Number)
  const when = days.length === 0 ? 'daily' : days.length === 1 ? `${DOW[days[0]] || '?'}s` : days.map((d) => DOW[d] || '?').join(' ')
  return `${when} ${p.ScheduleTime}`
}

export function fmtBytes(n: number): string {
  if (!n) return '—'
  if (n >= 1024 * 1024 * 1024) return `${(n / 1024 ** 3).toFixed(1)} GB`
  if (n >= 1024 * 1024) return `${(n / 1024 ** 2).toFixed(1)} MB`
  if (n >= 1024) return `${Math.round(n / 1024)} KB`
  return `${n} B`
}

export function money(v: number | null): string {
  if (v === null || v === undefined) return ''
  if (v >= 1e9) return `$${(v / 1e9).toFixed(1)}B`
  if (v >= 1e6) return `$${(v / 1e6).toFixed(1)}M`
  if (v >= 1e3) return `$${Math.round(v / 1e3)}K`
  return `$${v}`
}

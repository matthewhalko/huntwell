import React, { useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { Picker } from '../components/Picker'
import { useToast } from '../components/ui'

/**
 * The API reference, in the app.
 *
 * Not a link to a doc site: the reader is signed in, so the examples carry
 * their own host, and the thing they are reading about is one tab away.
 * Guides first (how a call is signed, how a hunt is driven), then one
 * endpoint at a time — scanned, not read as a scroll.
 */

/**
 * Syntax highlighting, hand-rolled.
 *
 * Two grammars, both tiny: the JSON we emit and the curl we generate. A
 * highlighting library would be 40kB and a CDN dependency for something that
 * is this well defined, and the colours have to come from the theme tokens
 * anyway. Text goes through React as text nodes, so nothing here can inject
 * markup.
 */
const JSON_RE = /("(?:\\.|[^"\\])*")(\s*:)?|(-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b)|\b(true|false|null)\b|([{}[\],])/g
const SH_RE = /('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*")|(^|\s)(-{1,2}[A-Za-z][\w-]*)|\b(curl)\b|(https?:\/\/[^\s\\'"]+)/g

function paint(src: string, lang: 'json' | 'sh'): React.ReactNode[] {
  const re = lang === 'json' ? JSON_RE : SH_RE
  const out: React.ReactNode[] = []
  const put = (text: string, cls?: string) => {
    if (!text) return
    out.push(cls ? <span key={out.length} className={cls}>{text}</span> : text)
  }
  let last = 0
  let m: RegExpExecArray | null
  re.lastIndex = 0
  while ((m = re.exec(src))) {
    put(src.slice(last, m.index))
    if (lang === 'json') {
      if (m[1]) put(m[1], m[2] ? 'tok-key' : 'tok-str')
      if (m[2]) put(m[2], 'tok-punct')
      if (m[3]) put(m[3], 'tok-num')
      if (m[4]) put(m[4], 'tok-lit')
      if (m[5]) put(m[5], 'tok-punct')
    } else {
      if (m[1]) put(m[1], 'tok-str')
      if (m[3]) {
        put(m[2])
        put(m[3], 'tok-flag')
      }
      if (m[4]) put(m[4], 'tok-cmd')
      if (m[5]) put(m[5], 'tok-url')
    }
    last = re.lastIndex
    if (m[0] === '') re.lastIndex++
  }
  put(src.slice(last))
  return out
}

function Code({ children, lang }: { children: string; lang: 'json' | 'sh' | 'plain' }) {
  return (
    <pre className={'docs-code lang-' + lang}>
      {lang === 'plain' ? children : paint(children, lang)}
    </pre>
  )
}

function Fields({ rows }: { rows: [string, string, string][] }) {
  return (
    <table className="docs-params">
      <thead>
        <tr>
          <th>Field</th>
          <th>Type</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {rows.map(([n, t, d]) => (
          <tr key={n}>
            <td>
              <code>{n}</code>
            </td>
            <td className="muted">{t}</td>
            <td className="muted">{d}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

export interface Endpoint {
  group: string
  title: string
  blurb: string
  guide?: 'overview' | 'auth' | 'errors'
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'
  path?: string
  params?: [string, string][]
  /// Query string used in the signed example. Must match what the curl sends.
  exampleQuery?: string
  body?: string
  fields?: [string, string, string][]
  response?: string
  returns?: [string, string, string][]
  status?: string
}

const PLAN = `{
  "id": 13,
  "name": "PDX roasters",
  "description": "independent coffee roasters in Portland",
  "kind": "prospects",
  "subject": "independent coffee roasters in Portland, Oregon",
  "status": "ready",
  "effort": "thorough",
  "target": 25,
  "schedule": { "enabled": false, "time": "09:00", "days": "1", "next_run_at": null },
  "ready": true,
  "updated_at": "2026-09-03T13:01:04Z",
  "models": { "scrape": "", "enrich": "", "planner": "" }
}`

const OUTREACH = `{
  "id": 57,
  "prospect_id": 9120,
  "plan_id": 13,
  "campaign": "Boutique hotels on the Oregon coast",
  "recipient": { "name": "Ana Ruiz", "email": "ana@coasthotels.com", "title": "General Manager",
                 "company": "Coast Hotels", "notes": "" },
  "to": "Ana Ruiz <ana@coasthotels.com>",
  "subject": "Fewer no-shows at Coast Hotels",
  "body": "Hi Ana,\n\nI saw Coast Hotels just opened in Lincoln City…",
  "footer": "Sam Lee\nPartnerships, Acme",
  "body_with_footer": "Hi Ana,\n\n…\n\nSam Lee\nPartnerships, Acme",
  "email": "To: Ana Ruiz <ana@coasthotels.com>\nSubject: Fewer no-shows at Coast Hotels\n\n…",
  "version": 1,
  "created_by": "Sam Lee",
  "created_at": "2026-09-25T17:02:11Z",
  "updated_at": "2026-09-25T17:02:11Z"
}`

const RUN = `{
  "id": 418,
  "plan_id": 13,
  "plan": "PDX roasters",
  "status": "running",
  "trigger": "api",
  "started_at": "2026-09-03T13:02:11Z",
  "finished_at": null,
  "new_artifacts": 6,
  "tokens": 184200,
  "cost_usd": 0.42,
  "cost_per_artifact": 0.07
}`

export const ENDPOINTS: Endpoint[] = [
  {
    group: 'Start here',
    title: 'Overview',
    blurb: 'A key can do everything the app can: create a plan from a sentence, run it, watch it, and read what it found.',
    guide: 'overview',
  },
  {
    group: 'Start here',
    title: 'Authentication',
    blurb: 'Every request is signed. The key travels; the secret never leaves your server.',
    guide: 'auth',
  },
  {
    group: 'Start here',
    title: 'Errors',
    blurb: 'Failures are JSON with an error string and the HTTP status that fits.',
    guide: 'errors',
  },
  {
    group: 'Start here',
    method: 'GET',
    path: '/v1/me',
    title: 'Who this key is',
    blurb: 'The workspace a key belongs to, and whether it is scoped to a single plan. The first call to make: if this works, signing is right.',
    response: `{
  "workspace_id": 2,
  "workspace": "Acme Growth",
  "key_scope": "workspace"
}`,
    returns: [
      ['workspace_id', 'number', 'The workspace this key can see.'],
      ['workspace', 'string', 'Display name.'],
      ['key_scope', 'string | object', '`\"workspace\"`, or `{ "plan_id": 13 }` when the key is pinned to one plan.'],
    ],
  },
  {
    group: 'Plans',
    method: 'GET',
    path: '/v1/plans',
    title: 'List plans',
    blurb: 'Every plan in the workspace. A pinned key only sees its own plan. Artifact and execution counts are on the list, not on a single plan.',
    response: `{
  "data": [
    {
      "id": 12,
      "name": "Mexican fintech leads",
      "kind": "prospects",
      "status": "ready",
      "effort": "normal",
      "target": 25,
      "ready": true,
      "artifacts": 213,
      "executions": 9
    }
  ]
}`,
  },
  {
    group: 'Plans',
    method: 'POST',
    path: '/v1/plans',
    title: 'Create a plan',
    blurb:
      'Describe what you want in plain words. The search is written in the background, so the plan comes back as drafting — poll GET /v1/plans/{id} until status is ready, then run it. A pinned key cannot create plans. Returns 201.',
    body: `{
  "brief": "independent coffee roasters in Portland, Oregon",
  "name": "PDX roasters",
  "target": 25,
  "effort": "thorough",
  "kind": "prospects",
  "sites": "google.com, yelp.com",
  "columns": []
}`,
    fields: [
      ['brief', 'string', 'Required. What to hunt for, in a sentence.'],
      ['name', 'string', 'Optional label. The brief is used if you leave this out.'],
      ['target', 'number', 'How many results to aim for. 0–500.'],
      ['effort', 'string', '`quick`, `normal`, `thorough` or `exhaustive`.'],
      ['kind', 'string', '`prospects` (people/companies), `artifacts` (your columns), `report` (one document) or `assets` (files). Omit to let the brief decide.'],
      ['sites', 'string', 'Comma-separated sites to try first. Steering, not a wall — the plan may still go elsewhere.'],
      ['columns', 'object[]', 'For `artifacts` plans: `[{ "name": "Price", "prompt": "asking price in USD" }]`. Any columns make the plan collect exactly those fields.'],
    ],
    response: PLAN.replace('"status": "ready"', '"status": "drafting"').replace('"ready": true', '"ready": false'),
    status: '201',
  },
  {
    group: 'Plans',
    method: 'GET',
    path: '/v1/plans/{id}',
    title: 'Get a plan',
    blurb: 'One plan. Poll this after creating one: status moves from drafting to ready (or failed). `ready` is true only when it can be run.',
    response: PLAN,
    returns: [
      ['id', 'number', 'Plan id, used on every other call.'],
      ['kind', 'string', '`prospects`, `artifacts`, `report` or `assets`.'],
      ['status', 'string', '`drafting`, `ready` or `failed`.'],
      ['effort', 'string', '`quick`, `normal`, `thorough` or `exhaustive`.'],
      ['target', 'number', 'Default result target for a run.'],
      ['schedule.days', 'string', 'Weekdays as 0–6, comma-separated. `1` is Monday.'],
      ['models.*', 'string', 'Cursor model ids. Empty means Huntwell picks.'],
    ],
  },
  {
    group: 'Plans',
    method: 'PATCH',
    path: '/v1/plans/{id}',
    title: 'Update a plan',
    blurb: 'Anything you leave out keeps its current value. A plan that is still drafting can be renamed or retargeted; running it still needs ready.',
    body: `{
  "name": "PDX roasters v2",
  "target": 50,
  "effort": "exhaustive",
  "schedule_enabled": true,
  "schedule_time": "07:30",
  "schedule_days": "1,4",
  "models": { "scrape": "claude-sonnet-5-thinking-high", "enrich": "", "planner": "" }
}`,
    fields: [
      ['name', 'string', 'The label on the plan.'],
      ['description', 'string', 'Shown under the name in the app.'],
      ['target', 'number', '0–500.'],
      ['effort', 'string', '`quick`, `normal`, `thorough` or `exhaustive`.'],
      ['schedule_enabled', 'boolean', 'Whether the plan runs itself.'],
      ['schedule_time', 'string', '`HH:MM` in the workspace timezone.'],
      ['schedule_days', 'string', 'Weekdays 0–6, comma-separated.'],
      ['models', 'object', 'Optional Cursor ids for `scrape`, `enrich` and `planner`.'],
    ],
    response: PLAN,
  },
  {
    group: 'Plans',
    method: 'DELETE',
    path: '/v1/plans/{id}',
    title: 'Delete a plan',
    blurb: 'Deletes the plan and everything it found. This cannot be undone.',
    response: `{ "deleted": 13 }`,
  },
  {
    group: 'Executions',
    method: 'POST',
    path: '/v1/plans/{id}/executions',
    title: 'Run a plan',
    blurb: 'Starts an execution and returns immediately with 202. Any number of plans can run at once; when every slot is busy the execution waits as `queued` and starts by itself when one frees up. Poll the execution (or its log) until it finishes. 402 means no card on file or no credits; 409 means the plan is still drafting or this plan is already running.',
    params: [
      ['target', 'Override how many results to aim for, just for this run.'],
      ['max_tokens', 'The most billable tokens this run may spend. It stops there and keeps what it found, finishing as succeeded. Omit for no limit beyond your credits.'],
    ],
    exampleQuery: 'target=25&max_tokens=200000',
    response: `{ "id": 418, "status": "queued" }`,
    status: '202',
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/plans/{id}/executions',
    title: "A plan's executions",
    blurb: 'Runs of this plan, newest first.',
    params: [['limit', 'Default 25.']],
    exampleQuery: 'limit=25',
    response: `{ "data": [ ${RUN} ] }`,
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/executions/{id}',
    title: 'Check an execution',
    blurb: 'Status is queued, running, succeeded, failed or cancelled. Tokens and dollar cost are on the finished (or in-flight) run.',
    response: RUN,
    returns: [
      ['status', 'string', '`queued`, `running`, `succeeded`, `failed` or `cancelled`.'],
      ['trigger', 'string', 'How it started — `api`, the schedule, or the app.'],
      ['new_artifacts', 'number', 'Rows this run added.'],
      ['tokens', 'number', 'Input plus output tokens so far.'],
      ['cost_usd', 'number', 'What this run has spent.'],
    ],
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/executions/{id}/log',
    title: 'Watch an execution',
    blurb: 'What the run is doing, line by line. Pass the last seq you saw as `after` and you only get what is new — poll this while status is queued or running.',
    params: [
      ['after', 'Last seq already seen. Default 0.'],
      ['limit', 'Lines per call. Default 500.'],
    ],
    exampleQuery: 'after=40&limit=500',
    response: `{
  "data": [
    { "seq": 41, "ts": "2026-09-03T13:04:02Z", "stream": "stdout", "line": "[1/4 scrape] …" }
  ]
}`,
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/executions',
    title: 'List executions',
    blurb: 'Recent runs across the workspace, newest first. A pinned key only sees its plan.',
    params: [
      ['plan_id', 'Only this plan.'],
      ['limit', 'Default 25.'],
    ],
    exampleQuery: 'limit=25',
    response: `{ "data": [ ${RUN.replace('"status": "running"', '"status": "succeeded"').replace('"finished_at": null', '"finished_at": "2026-09-03T13:18:44Z"')} ] }`,
  },
  {
    group: 'Executions',
    method: 'POST',
    path: '/v1/executions/{id}/cancel',
    title: 'Cancel an execution',
    blurb: 'Stops a queued or running execution. Safe to call again after it has already finished.',
    response: `{ "id": 418, "status": "cancelled" }`,
  },
  {
    group: 'Results',
    method: 'GET',
    path: '/v1/plans/{id}/artifacts',
    title: "A plan's artifacts",
    blurb: 'Everything a plan has found — people, custom rows, documents and files — through one shape. Use /v1/prospects when you want every CRM field, not this summary.',
    params: [
      ['search', 'Free text across everything stored.'],
      ['limit', 'Default 100.'],
      ['offset', 'For paging.'],
    ],
    exampleQuery: 'limit=100&offset=0',
    response: `{
  "data": [
    { "kind": "prospect", "plan_id": 13, "plan": "PDX roasters",
      "label": "Matt Higgins", "sublabel": "Coava Coffee",
      "url": "https://coava.com", "last_seen_utc": "2026-09-03T13:20:00Z" }
  ],
  "total": 213,
  "limit": 100,
  "offset": 0
}`,
  },
  {
    group: 'Results',
    method: 'GET',
    path: '/v1/artifacts',
    title: 'Everything, across plans',
    blurb: 'The same artifact shape, unfiltered by plan — one searchable list of everything the workspace has found.',
    params: [
      ['plan_id', 'Narrow to one plan.'],
      ['search', 'Free text.'],
      ['limit', 'Default 100.'],
      ['offset', 'For paging.'],
    ],
    exampleQuery: 'limit=100',
    response: `{ "data": [ … ], "total": 1204, "limit": 100, "offset": 0 }`,
  },
  {
    group: 'Results',
    method: 'GET',
    path: '/v1/prospects',
    title: 'Sync prospects',
    blurb:
      'Every CRM field for people and companies a prospects plan has found. Built for a client that keeps its own copy: pass the last id you stored as `after`, and the reply carries `next_cursor` and `has_more`. Polling every few minutes is one small query and usually an empty page.',
    params: [
      ['plan_id', 'Only this plan. A pinned key ignores this and uses its own plan.'],
      ['after', 'Last id you have stored. Default 0 — the beginning.'],
      ['limit', '1–500. Default 100.'],
    ],
    exampleQuery: 'after=0&limit=100',
    response: `{
  "data": [
    {
      "id": 9001,
      "plan_id": 13,
      "plan": "PDX roasters",
      "name": "Matt Higgins",
      "title": "Roaster",
      "company": "Coava Coffee",
      "industry": "Coffee",
      "email": "matt@coava.com",
      "email_status": "verified",
      "phone": "",
      "website": "https://coava.com",
      "linkedin": "",
      "location": "Portland, OR",
      "notes": "",
      "estimated_value": null,
      "source_key": "coava.com",
      "first_seen_at": "2026-09-03T13:20:00Z",
      "last_seen_at": "2026-09-03T13:20:00Z"
    }
  ],
  "next_cursor": 9001,
  "has_more": false
}`,
    returns: [
      ['data', 'object[]', 'Prospects with id greater than `after`, in id order.'],
      ['next_cursor', 'number', 'Pass this back as `after` on the next call.'],
      ['has_more', 'boolean', 'True when this page was full — keep paging.'],
    ],
  },
  {
    group: 'Results',
    method: 'GET',
    path: '/v1/plans/{id}/graph',
    title: 'Knowledge graph',
    blurb: 'What the plan has searched, the pages those searches opened, what came out, and the edges between them — plus what each angle cost in tokens.',
    response: `{
  "queries": [ { "query": "portland coffee roasters", "hits": 3, "new_prospects": 2, "tokens": 180000 } ],
  "pages":   [ { "url": "https://coava.com/about", "host": "coava.com", "visits": 2 } ],
  "results": [ { "key": "coava.com", "label": "Coava Coffee" } ],
  "edges":   [ { "from_kind": "query", "from": "portland coffee roasters",
                 "to_kind": "page", "to": "coava.com/about", "weight": 2 } ],
  "totals":  { "queries": 2, "pages": 3, "results": 2, "tokens": 270000 }
}`,
  },
  {
    group: 'Outreach',
    method: 'POST',
    path: '/v1/outreach',
    title: 'Draft an email',
    blurb:
      'Writes a cold outreach email to one person, from your workspace\'s product description and rules — or, when the plan has its own outreach, that campaign\'s — ending with the key maker\'s footer. Give a `prospect_id` a plan found, or a `recipient` by hand (a name or a company is enough). One short model call, charged to your credits like a run; nothing is sent. Needs a workspace key — a key pinned to one plan is refused.',
    body: `{ "prospect_id": 9120 }

// or, to anyone:
{ "recipient": { "name": "Ana Ruiz", "email": "ana@coasthotels.com",
                 "title": "General Manager", "company": "Coast Hotels",
                 "notes": "Opened a second hotel in Lincoln City this spring" } }`,
    fields: [
      ['prospect_id', 'integer', 'A prospect in this workspace. Its name, title, company, industry, location, website and notes are used.'],
      ['recipient', 'object', 'Instead of a prospect: name, email, title, company, notes. Name or company required.'],
      ['plan_id', 'integer', 'Optional. Write it to this plan\'s own outreach (its campaign brief, offer and rules). A prospect uses its own plan\'s when this is left out; a plan without its own outreach uses the workspace\'s.'],
      ['design_id', 'integer', 'Optional. Write it with this saved outreach profile (Outreach → Profiles) — for any prospect or recipient. Wins over any plan\'s outreach.'],
    ],
    response: OUTREACH,
    returns: [
      ['email', 'string', 'The whole email, ready to paste: To, Subject, then the body and footer.'],
      ['subject', 'string', 'The subject line.'],
      ['body_with_footer', 'string', 'The body as sent: the draft, then your footer.'],
      ['to', 'string', '`Name <address>`, or whichever half is known.'],
      ['version', 'integer', 'Goes up with every revision, edit or restore.'],
      ['campaign', 'string', 'What it was written for: the saved profile\'s name or the plan\'s, or null for the workspace\'s settings.'],
    ],
    status: '201 Created · 402 no credits · 429 more than 30 drafts a minute · 503 drafting not set up on this server',
  },
  {
    group: 'Outreach',
    method: 'POST',
    path: '/v1/outreach/{id}/revise',
    title: 'Revise with feedback',
    blurb: 'Rewrites the draft as it stands now — hand edits included — following your feedback, and keeps the old version. Charged like a draft.',
    body: `{ "feedback": "Shorter, and lead with their new Lincoln City hotel" }`,
    fields: [['feedback', 'string', 'What to change, in plain words. Up to 2,000 characters.']],
    response: OUTREACH,
  },
  {
    group: 'Outreach',
    method: 'PATCH',
    path: '/v1/outreach/{id}',
    title: 'Edit by hand',
    blurb: 'Replaces the subject and body with your own text, as a new version. Free. The footer is not part of `body`.',
    body: `{ "subject": "Fewer no-shows at Coast Hotels", "body": "Hi Ana,\n\n…" }`,
    fields: [
      ['subject', 'string', 'Up to 300 characters.'],
      ['body', 'string', 'The email without the footer. Required.'],
    ],
    response: OUTREACH,
  },
  {
    group: 'Outreach',
    method: 'GET',
    path: '/v1/outreach/{id}',
    title: 'Get a draft',
    blurb: 'One draft, with `versions`: every version it went through, newest first — `source` is draft, revise, edit or restore, and a revision carries the `feedback` that asked for it.',
    response: OUTREACH,
  },
  {
    group: 'Outreach',
    method: 'POST',
    path: '/v1/outreach/{id}/restore',
    title: 'Go back to a version',
    blurb: 'Makes an earlier version current again — as a new version, so nothing is lost. Free.',
    body: `{ "version": 1 }`,
    response: OUTREACH,
  },
  {
    group: 'Outreach',
    method: 'GET',
    path: '/v1/outreach',
    title: 'List drafts',
    blurb: 'The workspace\'s drafts, most recently changed first.',
    params: [['limit', 'Up to 500. Default 100.']],
    exampleQuery: 'limit=25',
    response: `{ "data": [ ${'{'} "id": 57, "to": "Ana Ruiz <ana@coasthotels.com>", "subject": "…", "version": 2, … ${'}'} ] }`,
  },
  {
    group: 'Outreach',
    method: 'DELETE',
    path: '/v1/outreach/{id}',
    title: 'Delete a draft',
    blurb: 'Removes the draft and all its versions.',
    response: `{ "deleted": 57 }`,
  },
  {
    group: 'Outreach',
    method: 'GET',
    path: '/v1/outreach/profile',
    title: 'Drafting settings',
    blurb: 'What drafts are written from: the workspace\'s `product` description and `rules`, and the `footer` of the person who made the key.',
    response: `{
  "product": "We make booking software for independent hotels…",
  "rules": "Under 120 words. Never mention pricing.",
  "footer": "Sam Lee\nPartnerships, Acme"
}`,
  },
  {
    group: 'Outreach',
    method: 'PUT',
    path: '/v1/outreach/profile',
    title: 'Change drafting settings',
    blurb: 'Any of `product`, `rules` and `footer`; whatever you leave out stays as it is. Product and rules are shared by the workspace; the footer belongs to the key\'s maker.',
    body: `{ "rules": "Under 120 words. End with one question. Never mention pricing." }`,
    fields: [
      ['product', 'string', 'What you sell, what it does, who it is for. Up to 4,000 characters.'],
      ['rules', 'string', 'How every draft should read. Up to 4,000 characters.'],
      ['footer', 'string', 'Your sign-off, appended verbatim. Up to 1,000 characters.'],
    ],
  },
  {
    group: 'Outreach',
    method: 'GET',
    path: '/v1/plans/{id}/outreach',
    title: 'A plan\'s outreach',
    blurb: 'What drafts for this plan\'s prospects are written from. With `custom` on, the plan\'s `brief`, `product` and `rules` apply; off, the workspace settings do. The workspace\'s own are included so you can show what a blank field falls back to.',
    response: `{
  "plan_id": 42,
  "custom": true,
  "design_id": null,
  "brief": "Practice owners thinking about succession — lead with the sale, not the software.",
  "product": "",
  "rules": "Mention their city. Under 100 words.",
  "workspace": { "product": "We make booking software…", "rules": "Never mention pricing." }
}`,
  },
  {
    group: 'Outreach',
    method: 'PUT',
    path: '/v1/plans/{id}/outreach',
    title: 'Change a plan\'s outreach',
    blurb: 'Any of `custom`, `brief`, `product` and `rules`; whatever you leave out stays as it is. Returns the plan\'s outreach as above.',
    body: `{ "custom": true, "rules": "Mention their city. Under 100 words." }`,
    fields: [
      ['custom', 'boolean', 'Use this plan\'s own outreach rather than the workspace settings.'],
      ['brief', 'string', 'Who this campaign is for and the angle to take. Up to 4,000 characters.'],
      ['product', 'string', 'Replaces the workspace\'s product description for this plan. Up to 4,000 characters.'],
      ['rules', 'string', 'Added to the workspace\'s rules; these win where they differ. Up to 4,000 characters.'],
    ],
  },
  {
    group: 'Account',
    method: 'GET',
    path: '/v1/usage',
    title: 'Usage and credits',
    blurb: 'Prepaid credits remaining and what this period has cost. A run cannot spend past `credits_usd`.',
    response: `{
  "period_start": "2026-09-01T00:00:00Z",
  "budget_usd": 37.6,
  "used_usd": 12.4,
  "remaining_usd": 37.6,
  "credits_usd": 37.6,
  "tokens_used": 8420000
}`,
    returns: [
      ['credits_usd', 'number', 'Prepaid balance. Runs pause at zero.'],
      ['used_usd', 'number', 'Spent this period.'],
      ['tokens_used', 'number', 'Tokens this period.'],
    ],
  },
]

const METHOD_CLASS: Record<string, string> = { GET: 'get', POST: 'post', PUT: 'patch', PATCH: 'patch', DELETE: 'del' }

function OverviewGuide() {
  return (
    <>
      <ol className="docs-steps">
        <li>
          Create a key on the <Link to="/app/api-access">API keys</Link> page. You get a <b>key</b> and a <b>secret</b>.
          The secret is shown once.
        </li>
        <li>
          Sign every call — see Authentication. <code>GET /v1/me</code> is the check that signing works.
        </li>
        <li>
          <code>POST /v1/plans</code> with a <code>brief</code>. The plan comes back as <code>drafting</code>.
        </li>
        <li>
          Poll <code>GET /v1/plans/&#123;id&#125;</code> until <code>status</code> is <code>ready</code> (or{' '}
          <code>failed</code>).
        </li>
        <li>
          <code>POST /v1/plans/&#123;id&#125;/executions</code> starts a run. It returns at once with{' '}
          <code>queued</code>.
        </li>
        <li>
          Poll <code>GET /v1/executions/&#123;id&#125;</code> or the log until <code>succeeded</code> / <code>failed</code>{' '}
          / <code>cancelled</code>.
        </li>
        <li>
          Read what it found: <code>/v1/plans/&#123;id&#125;/artifacts</code> for the mixed list,{' '}
          <code>/v1/prospects</code> to sync people and companies into your own store.
        </li>
      </ol>
      <h4>Kinds of plan</h4>
      <Fields
        rows={[
          ['prospects', 'default', 'People and companies, with email, title, company and the rest.'],
          ['artifacts', 'custom rows', 'You name the columns; each row is those fields.'],
          ['report', 'one document', 'A Markdown report about the subject.'],
          ['assets', 'files', 'Documents found about the subject, stored as files.'],
        ]}
      />
      <h4>Limits</h4>
      <p className="muted">
        Calls are rate-limited to about one per second, with a burst of 20. A 429 includes{' '}
        <code>Retry-After: 60</code>. Eight failed auths from one address lock it out for 15 minutes.
      </p>
    </>
  )
}

function AuthGuide({
  origin,
  keyName,
  copy,
}: {
  origin: string
  keyName: string
  copy: (t: string) => void
}) {
  const [lang, setLang] = useState<'sh' | 'py' | 'js'>('sh')
  const sh = `KEY="${keyName}"
SECRET="your-api-secret"          # shown once, when the key was created
TS=$(( $(date +%s) * 1000 ))
NONCE=$(openssl rand -hex 16)     # new for every request
METHOD="POST"
PATH_="/v1/plans"
QUERY=""                          # everything after the ? , exactly as sent
BODY='{"brief":"independent coffee roasters in Portland"}'   # exactly the bytes you send; '' for none

BODY_SHA=$(printf '%s' "$BODY" | openssl dgst -sha256 -hex | sed 's/^.* //')
SIGN=$(printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s' "$METHOD" "$PATH_" "$QUERY" "$TS" "$NONCE" "$BODY_SHA" \\
  | openssl dgst -sha256 -hmac "$SECRET" -hex | sed 's/^.* //')

curl -s -X "$METHOD" "${origin}$PATH_" \\
  -H "X-HW-KEY: $KEY" \\
  -H "X-HW-TS: $TS" \\
  -H "X-HW-NONCE: $NONCE" \\
  -H "X-HW-SIGN: $SIGN" \\
  -H "Content-Type: application/json" \\
  --data-binary "$BODY"`

  const py = `import hashlib, hmac, json, os, secrets, time, requests

key = os.environ["HUNTWELL_KEY"]
secret = os.environ["HUNTWELL_SECRET"]

def call(method, path, query="", payload=None):
    # The body is signed byte for byte: serialise it once, sign those bytes,
    # and send the same bytes (data=, not json=, which would re-serialise).
    body = json.dumps(payload).encode() if payload is not None else b""
    ts = str(int(time.time() * 1000))
    nonce = secrets.token_hex(16)          # new for every request
    signed = f"{method}\\n{path}\\n{query}\\n{ts}\\n{nonce}\\n{hashlib.sha256(body).hexdigest()}"
    sign = hmac.new(secret.encode(), signed.encode(), hashlib.sha256).hexdigest()
    url = "${origin}" + path + (f"?{query}" if query else "")
    return requests.request(method, url, data=body, headers={
        "X-HW-KEY": key, "X-HW-TS": ts, "X-HW-NONCE": nonce, "X-HW-SIGN": sign,
        "Content-Type": "application/json",
    })

print(call("GET", "/v1/me").json())
print(call("POST", "/v1/plans", payload={"brief": "independent coffee roasters in Portland"}).json())`

  const js = `import crypto from "node:crypto";

const key = process.env.HUNTWELL_KEY;
const secret = process.env.HUNTWELL_SECRET;
const sha256 = (s) => crypto.createHash("sha256").update(s).digest("hex");

async function call(method, path, query = "", payload) {
  // Serialise once; sign and send the same string.
  const body = payload === undefined ? "" : JSON.stringify(payload);
  const ts = String(Date.now());
  const nonce = crypto.randomBytes(16).toString("hex"); // new for every request
  const signed = [method, path, query, ts, nonce, sha256(body)].join("\\n");
  const sign = crypto.createHmac("sha256", secret).update(signed).digest("hex");
  const r = await fetch("${origin}" + path + (query ? "?" + query : ""), {
    method,
    body: body || undefined,
    headers: { "X-HW-KEY": key, "X-HW-TS": ts, "X-HW-NONCE": nonce, "X-HW-SIGN": sign, "Content-Type": "application/json" },
  });
  return r.json();
}

console.log(await call("GET", "/v1/me"));
console.log(await call("POST", "/v1/plans", "", { brief: "independent coffee roasters in Portland" }));`

  const src = lang === 'sh' ? sh : lang === 'py' ? py : js
  return (
    <>
      <p>
        Your <b>key</b> identifies you and travels with the request. Your <b>secret</b> never leaves your server — it
        only signs. A captured request cannot be replayed against another endpoint, or after its window, without the
        secret.
      </p>
      <p>Send four headers on every call:</p>
      <Fields
        rows={[
          ['X-HW-KEY', 'string', 'Your API key.'],
          ['X-HW-TS', 'number', 'Milliseconds since the Unix epoch. More than 30 seconds old, or more than 5 seconds in the future, is refused. Keep the calling clock correct.'],
          ['X-HW-NONCE', 'string', '16–64 random characters (letters, digits, - and _), new for every request. A nonce is accepted once: the same request sent twice is refused, even inside its 30 seconds.'],
          ['X-HW-SIGN', 'hex', 'HMAC-SHA256 of the payload below, using your secret, as lower-case hex.'],
        ]}
      />
      <h4>What is signed</h4>
      <p className="muted">
        Six parts joined by newlines, in this order. Change any of them — including one byte of the body — and the
        signature stops matching.
      </p>
      <Code lang="plain">{'METHOD\\nPATH\\nQUERY\\nTIMESTAMP\\nNONCE\\nSHA256(BODY)'}</Code>
      <ul className="muted">
        <li>
          <code>METHOD</code> is the HTTP verb in capitals — <code>GET</code>, <code>POST</code>, <code>PUT</code>,{' '}
          <code>PATCH</code>, <code>DELETE</code>.
        </li>
        <li>
          <code>PATH</code> is the path only, no host and no query. <code>/v1/plans/13</code>, not the full URL.
        </li>
        <li>
          <code>QUERY</code> is everything after the <code>?</code>, exactly as you send it. Empty when there is none.
          Do not reorder or re-encode it.
        </li>
        <li>
          <code>TIMESTAMP</code> is the same milliseconds you put in <code>X-HW-TS</code>, and <code>NONCE</code> the
          same value as <code>X-HW-NONCE</code>.
        </li>
        <li>
          <code>SHA256(BODY)</code> is the lower-case hex SHA-256 of the exact bytes of the request body. With no
          body (every <code>GET</code>) it is the digest of nothing:{' '}
          <code>e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855</code>. Serialise the body once and
          send those same bytes — a library that re-serialises JSON on the way out will break the signature. Bodies
          are limited to 1&nbsp;MB.
        </li>
      </ul>
      <p className="muted">
        The secret is shown <b>once</b>, when the key is created. If it is lost, create a new key and revoke the old
        one. <code>Authorization: Bearer</code> and <code>?token=</code> are not accepted.
      </p>
      <div className="docs-code-head">
        <div className="docs-tabs">
          {(['sh', 'py', 'js'] as const).map((id) => (
            <button key={id} className={'btn sm' + (lang === id ? ' on' : '')} type="button" onClick={() => setLang(id)}>
              {id === 'sh' ? 'curl' : id === 'py' ? 'Python' : 'Node'}
            </button>
          ))}
        </div>
        <button className="btn sm" type="button" onClick={() => copy(src)}>
          Copy
        </button>
      </div>
      <Code lang={lang === 'sh' ? 'sh' : 'plain'}>{src}</Code>
    </>
  )
}

function ErrorsGuide() {
  return (
    <>
      <p className="muted">Every error body is the same shape:</p>
      <Code lang="json">{`{ "error": "that plan is still being drafted" }`}</Code>
      <p className="muted">
        A request refused before it reaches the API — an unknown key, a stale timestamp, a reused nonce or a bad
        signature — is answered <code>401</code> with a plain-text line saying which.
      </p>
      <h4>Status codes</h4>
      <Fields
        rows={[
          ['400', 'Bad request', 'A field is missing or a plan cannot be saved as sent.'],
          ['401', 'Unauthorized', 'Unknown key, missing headers, stale timestamp, a reused nonce, or a bad signature. The error text says which.'],
          ['413', 'Too large', 'The request body is over 1 MB.'],
          ['402', 'Payment required', 'No card on file, or no prepaid credits left.'],
          ['403', 'Forbidden', 'This key is pinned to another plan, or a pinned key tried to create a plan.'],
          ['404', 'Not found', 'That plan or execution is not in this workspace.'],
          ['409', 'Conflict', 'The plan is still drafting, or that plan is already running (other plans can run at the same time).'],
          ['429', 'Too many requests', 'Slow down. `Retry-After: 60`. Eight failed auths lock the address for 15 minutes.'],
        ]}
      />
    </>
  )
}

export function ApiDocs({ sampleKey }: { sampleKey?: string }) {
  const [active, setActive] = useState(0)
  const toast = useToast()
  const origin = window.location.origin
  const key = sampleKey || '$HUNTWELL_KEY'
  const ep = ENDPOINTS[active]

  const groups = useMemo(() => {
    const out: { group: string; items: { ep: Endpoint; i: number }[] }[] = []
    ENDPOINTS.forEach((e, i) => {
      const g = out.find((x) => x.group === e.group) || (out.push({ group: e.group, items: [] }), out[out.length - 1])
      g.items.push({ ep: e, i })
    })
    return out
  }, [])

  const copy = (t: string) => {
    navigator.clipboard.writeText(t).then(
      () => toast('Copied'),
      () => toast('Could not copy', true),
    )
  }

  // A signed request, because that is what a key issued today requires. The
  // example is a shell script rather than a bare curl: the signature covers
  // the query string, a fresh nonce and the body's digest, so all three are
  // settled before the call.
  const curl = useMemo(() => {
    if (!ep.method || !ep.path) return ''
    const path = ep.path.replace('{id}', '13')
    const query = ep.exampleQuery || ''
    // The first JSON object in the example, on one line — the bytes sent.
    const body = ep.body ? ep.body.split(/\n\s*\n/)[0].replace(/\n\s*/g, ' ').trim() : ''
    const url = query ? `${origin}${path}?$QUERY` : `${origin}$PATH_`
    const lines = [
      `KEY="${key}"`,
      `SECRET="your-api-secret"          # shown once, when the key was created`,
      `TS=$(( $(date +%s) * 1000 ))`,
      `NONCE=$(openssl rand -hex 16)     # new for every request`,
      `METHOD="${ep.method}"`,
      `PATH_="${path}"`,
      `QUERY="${query}"`,
      `BODY='${body}'`,
      ``,
      `BODY_SHA=$(printf '%s' "$BODY" | openssl dgst -sha256 -hex | sed 's/^.* //')`,
      `SIGN=$(printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s' "$METHOD" "$PATH_" "$QUERY" "$TS" "$NONCE" "$BODY_SHA" \\`,
      `  | openssl dgst -sha256 -hmac "$SECRET" -hex | sed 's/^.* //')`,
      ``,
      `curl -s -X "$METHOD" "${url}" \\`,
      `  -H "X-HW-KEY: $KEY" \\`,
      `  -H "X-HW-TS: $TS" \\`,
      `  -H "X-HW-NONCE: $NONCE" \\`,
      `  -H "X-HW-SIGN: $SIGN"${body ? ' \\' : ''}`,
    ]
    if (body) lines.push(`  -H "Content-Type: application/json" \\`, `  --data-binary "$BODY"`)
    return lines.join('\n')
  }, [ep, origin, key])

  return (
    <div className="docs">
      <nav className="docs-nav">
        {groups.map((g) => (
          <div key={g.group}>
            <div className="docs-group">{g.group}</div>
            {g.items.map(({ ep: e, i }) => (
              <button key={i} className={'docs-link' + (i === active ? ' active' : '')} onClick={() => setActive(i)}>
                {e.method ? <span className={'m ' + METHOD_CLASS[e.method]}>{e.method}</span> : <span className="m guide">DOC</span>}
                <span className="t">{e.title}</span>
              </button>
            ))}
          </div>
        ))}
      </nav>

      <div className="docs-pick">
        <Picker
          value={String(active)}
          onChange={(v) => setActive(+v)}
          searchFrom={6}
          options={ENDPOINTS.map((e, i) => ({
            value: String(i),
            label: e.path ? `${e.title} — ${e.path}` : e.title,
            icon: e.method ? (
              <span className={'m ' + METHOD_CLASS[e.method]}>{e.method}</span>
            ) : (
              <span className="m guide">DOC</span>
            ),
          }))}
        />
      </div>

      <div className="docs-body">
        {ep.method && ep.path && (
          <div className="docs-head">
            <span className={'m ' + METHOD_CLASS[ep.method]}>{ep.method}</span>
            <code className="docs-path">{ep.path}</code>
            {ep.status && <span className="muted sm">→ {ep.status}</span>}
          </div>
        )}
        <h2>{ep.title}</h2>
        <p className="muted">{ep.blurb}</p>

        {ep.guide === 'overview' && <OverviewGuide />}
        {ep.guide === 'auth' && <AuthGuide origin={origin} keyName={key} copy={copy} />}
        {ep.guide === 'errors' && <ErrorsGuide />}

        {ep.params && (
          <>
            <h4>Query parameters</h4>
            <table className="docs-params">
              <tbody>
                {ep.params.map(([n, d]) => (
                  <tr key={n}>
                    <td>
                      <code>{n}</code>
                    </td>
                    <td className="muted">{d}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}

        {ep.fields && (
          <>
            <h4>Body fields</h4>
            <Fields rows={ep.fields} />
          </>
        )}

        {ep.body && (
          <>
            <h4>Body</h4>
            <Code lang="json">{ep.body}</Code>
          </>
        )}

        {curl && (
          <>
            <div className="docs-code-head">
              <h4>Request</h4>
              <button className="btn sm" type="button" onClick={() => copy(curl)}>
                Copy
              </button>
            </div>
            <Code lang="sh">{curl}</Code>
          </>
        )}

        {ep.returns && (
          <>
            <h4>Response fields</h4>
            <Fields rows={ep.returns} />
          </>
        )}

        {ep.response && (
          <>
            <h4>Response{ep.status ? ` · ${ep.status}` : ''}</h4>
            <Code lang="json">{ep.response}</Code>
          </>
        )}
      </div>
    </div>
  )
}

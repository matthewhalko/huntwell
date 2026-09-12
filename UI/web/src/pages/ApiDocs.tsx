import React, { useMemo, useState } from 'react'
import { Picker } from '../components/Picker'

/**
 * The API reference, in the app.
 *
 * Not a link to a doc site: the reader is signed in, so the examples can carry
 * their own key and their own host, and the thing they are reading about is one
 * tab away. Endpoints on the left, one endpoint at a time on the right.
 */
/**
 * Syntax highlighting, hand-rolled.
 *
 * Two grammars, both tiny: the JSON we emit and the curl line we generate. A
 * highlighting library would be 40kB and a CDN dependency for something that is
 * this well defined, and the colours have to come from the theme tokens anyway.
 * Text goes through React as text nodes, so nothing here can inject markup.
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
      // A string followed by a colon is a key, and reads as one.
      if (m[1]) put(m[1], m[2] ? 'tok-key' : 'tok-str')
      if (m[2]) put(m[2], 'tok-punct')
      if (m[3]) put(m[3], 'tok-num')
      if (m[4]) put(m[4], 'tok-lit')
      if (m[5]) put(m[5], 'tok-punct')
    } else {
      if (m[1]) put(m[1], 'tok-str')
      if (m[3]) {
        put(m[2]) // the whitespace the flag was anchored to
        put(m[3], 'tok-flag')
      }
      if (m[4]) put(m[4], 'tok-cmd')
      if (m[5]) put(m[5], 'tok-url')
    }
    last = re.lastIndex
    if (m[0] === '') re.lastIndex++ // paranoia: never loop on a zero-width match
  }
  put(src.slice(last))
  return out
}

function Code({ children, lang }: { children: string; lang: 'json' | 'sh' }) {
  return <pre className={'docs-code lang-' + lang}>{paint(children, lang)}</pre>
}

export interface Endpoint {
  group: string
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE'
  path: string
  title: string
  blurb: string
  params?: [string, string][]
  body?: string
  response: string
}

export const ENDPOINTS: Endpoint[] = [
  {
    group: 'Start here',
    method: 'GET',
    path: '/v1/me',
    title: 'Who this key is',
    blurb: 'The workspace a key belongs to, and whether it is scoped to a single plan. Handy for checking a key works.',
    response: `{
  "workspace_id": 2,
  "workspace": "Acme Growth",
  "key_scope": "workspace"
}`,
  },
  {
    group: 'Plans',
    method: 'GET',
    path: '/v1/plans',
    title: 'List plans',
    blurb: 'Every plan in the workspace, with how many artifacts each has found.',
    response: `{
  "data": [
    {
      "id": 12,
      "name": "Mexican fintech leads",
      "description": "treasury leads at Mexican fintechs",
      "kind": "prospects",
      "status": "ready",
      "effort": "normal",
      "target": 25,
      "schedule": { "enabled": true, "time": "09:00", "days": "1", "next_run_at": "2026-09-08T13:00:00Z" },
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
      'Describe what you want in plain words. The search itself is written for you in the background, so the plan comes back as "drafting" — poll it until status is "ready", then run it.',
    body: `{
  "brief": "independent coffee roasters in Portland, Oregon",
  "name": "PDX roasters",
  "target": 25,
  "effort": "thorough"
}`,
    response: `{
  "id": 13,
  "name": "PDX roasters",
  "status": "drafting",
  "ready": false
}`,
  },
  {
    group: 'Plans',
    method: 'GET',
    path: '/v1/plans/{id}',
    title: 'Get a plan',
    blurb: 'One plan. Poll this after creating one to see status move from drafting to ready.',
    response: `{ "id": 13, "name": "PDX roasters", "status": "ready", "ready": true }`,
  },
  {
    group: 'Plans',
    method: 'PATCH',
    path: '/v1/plans/{id}',
    title: 'Update a plan',
    blurb: 'Anything you leave out keeps its current value. Effort is quick, normal, thorough or exhaustive. Models are Cursor ids for search, enrichment and the next-search planner; empty lets Cursor pick.',
    body: `{
  "target": 50,
  "effort": "exhaustive",
  "schedule_enabled": true,
  "schedule_time": "07:30",
  "schedule_days": "1,4",
  "models": { "scrape": "claude-sonnet-5-thinking-high", "enrich": "", "planner": "" }
}`,
    response: `{ "id": 13, "target": 50, "effort": "exhaustive" }`,
  },
  {
    group: 'Plans',
    method: 'DELETE',
    path: '/v1/plans/{id}',
    title: 'Delete a plan',
    blurb: 'Deletes the plan and everything it found.',
    response: `{ "deleted": 13 }`,
  },
  {
    group: 'Executions',
    method: 'POST',
    path: '/v1/plans/{id}/executions',
    title: 'Run a plan',
    blurb:
      'Starts an execution and returns immediately. 402 means the workspace has no card on file; 409 means the plan is still drafting or already running.',
    params: [['target', 'Override how many results to aim for, just for this execution.']],
    response: `{ "id": 418, "status": "queued" }`,
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/executions/{id}',
    title: 'Check an execution',
    blurb: 'Status is queued, running, succeeded, failed or cancelled.',
    response: `{
  "id": 418,
  "plan_id": 13,
  "plan": "PDX roasters",
  "status": "running",
  "started_at": "2026-09-03T13:02:11Z",
  "finished_at": null,
  "new_artifacts": 6
}`,
  },
  {
    group: 'Executions',
    method: 'GET',
    path: '/v1/executions/{id}/log',
    title: 'Watch an execution',
    blurb: 'What the execution is doing, line by line. Pass the last seq you saw as `after` and you only get what is new.',
    params: [
      ['after', 'Last seq already seen. Default 0.'],
      ['limit', 'Lines per call. Default 500.'],
    ],
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
    blurb: 'Recent executions across the workspace, newest first.',
    params: [
      ['plan_id', 'Only this plan.'],
      ['limit', 'Default 25.'],
    ],
    response: `{ "data": [ { "id": 418, "status": "succeeded", "new_artifacts": 6 } ] }`,
  },
  {
    group: 'Executions',
    method: 'POST',
    path: '/v1/executions/{id}/cancel',
    title: 'Cancel an execution',
    blurb: 'Stops a queued or running execution.',
    response: `{ "id": 418, "status": "cancelled" }`,
  },
  {
    group: 'Artifacts',
    method: 'GET',
    path: '/v1/plans/{id}/artifacts',
    title: "A plan's artifacts",
    blurb: 'Everything a plan has found — rows, documents and files come back through one shape.',
    params: [
      ['search', 'Free text across everything stored.'],
      ['limit', 'Default 100.'],
      ['offset', 'For paging.'],
    ],
    response: `{
  "data": [
    { "kind": "prospect", "plan": "PDX roasters", "label": "Matt Higgins",
      "sublabel": "Coava Coffee", "url": "https://coava.com", "seen": "2026-09-03T13:20:00Z" }
  ],
  "total": 213,
  "limit": 100,
  "offset": 0
}`,
  },
  {
    group: 'Artifacts',
    method: 'GET',
    path: '/v1/artifacts',
    title: 'Everything, across plans',
    blurb: 'The same shape, unfiltered by plan — one searchable list of everything the workspace has ever found.',
    params: [
      ['plan_id', 'Narrow to one plan.'],
      ['search', 'Free text.'],
    ],
    response: `{ "data": [ … ], "total": 1204 }`,
  },
  {
    group: 'Artifacts',
    method: 'GET',
    path: '/v1/plans/{id}/graph',
    title: 'Knowledge graph',
    blurb:
      'What the plan has searched, the pages those searches opened, what came out, and the edges between them — plus what each angle cost.',
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
    group: 'Account',
    method: 'GET',
    path: '/v1/usage',
    title: 'Usage and budget',
    blurb: 'What this period has cost and what is left of the budget.',
    response: `{
  "period_start": "2026-09-01T00:00:00Z",
  "budget_usd": 50.0,
  "used_usd": 12.4,
  "remaining_usd": 37.6,
  "tokens_used": 8420000
}`,
  },
]

const METHOD_CLASS: Record<string, string> = { GET: 'get', POST: 'post', PATCH: 'patch', DELETE: 'del' }

export function ApiDocs({ sampleKey }: { sampleKey?: string }) {
  const [active, setActive] = useState(0)
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

  const curl = useMemo(() => {
    const path = ep.path.replace('{id}', '13')
    const lines = [`curl -s ${origin}${path} \\`, `  -H "Authorization: Bearer ${key}"`]
    if (ep.method !== 'GET') lines[0] = `curl -s -X ${ep.method} ${origin}${path} \\`
    if (ep.body) {
      lines[lines.length - 1] += ' \\'
      lines.push(`  -H "Content-Type: application/json" \\`, `  -d '${ep.body.replace(/\n\s*/g, ' ')}'`)
    }
    return lines.join('\n')
  }, [ep, origin, key])

  const copy = (t: string) => navigator.clipboard.writeText(t).catch(() => {})

  return (
    <div className="docs">
      <nav className="docs-nav">
        {groups.map((g) => (
          <div key={g.group}>
            <div className="docs-group">{g.group}</div>
            {g.items.map(({ ep: e, i }) => (
              <button key={i} className={'docs-link' + (i === active ? ' active' : '')} onClick={() => setActive(i)}>
                <span className={'m ' + METHOD_CLASS[e.method]}>{e.method}</span>
                <span className="t">{e.title}</span>
              </button>
            ))}
          </div>
        ))}
      </nav>

      {/* On a phone the endpoint list is the whole screen before the reader
          reaches a word of documentation. One control instead: the endpoint
          you are reading, with every other one a tap away and filterable. */}
      <div className="docs-pick">
        <Picker
          value={String(active)}
          onChange={(v) => setActive(+v)}
          searchFrom={6}
          options={ENDPOINTS.map((e, i) => ({
            value: String(i),
            label: `${e.title} — ${e.path}`,
            icon: <span className={'m ' + METHOD_CLASS[e.method]}>{e.method}</span>,
          }))}
        />
      </div>

      <div className="docs-body">
        <div className="docs-head">
          <span className={'m ' + METHOD_CLASS[ep.method]}>{ep.method}</span>
          <code className="docs-path">{ep.path}</code>
        </div>
        <h2>{ep.title}</h2>
        <p className="muted">{ep.blurb}</p>

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

        {ep.body && (
          <>
            <h4>Body</h4>
            <Code lang="json">{ep.body}</Code>
          </>
        )}

        <div className="docs-code-head">
          <h4>Request</h4>
          <button className="btn sm" onClick={() => copy(curl)}>
            Copy
          </button>
        </div>
        <Code lang="sh">{curl}</Code>

        <h4>Response</h4>
        <Code lang="json">{ep.response}</Code>

        <p className="muted sm">
          Every response is JSON. Errors carry <code>{'{ "error": "…" }'}</code> with the status that fits: 401 unknown key, 403
          out of scope, 404 not found, 402 no card on file, 409 already running, 429 too many requests.
        </p>
      </div>
    </div>
  )
}

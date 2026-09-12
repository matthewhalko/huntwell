import React from 'react'
import { LogLine } from '../api'

/**
 * A run's log, read the way a person would ask about it: what did it actually
 * do? The pipeline already prints every step and the agent narrates every tool
 * call — this turns those lines into an audit trail of actions (searched here,
 * opened that, kept six, saved four) instead of a wall of output.
 *
 * Parsing on this side is deliberate: the log is the run's own record, written
 * by a ported module that tracks the CLI, and it stays exactly as it is.
 */
export type ActKind =
  | 'iteration'
  | 'stage'
  | 'search'
  | 'page'
  | 'read'
  | 'check'
  | 'found'
  | 'stored'
  | 'plan'
  | 'info'
  | 'done'
  | 'warn'
  | 'error'

export interface Act {
  seq: number
  kind: ActKind
  text: string
  detail?: string
  at: string
}

/// Friendly names for the pipeline's numbered stages.
const STAGES: Record<string, string> = {
  scrape: 'Searching the web',
  dedupe: 'Checking against what you already have',
  enrich: 'Filling in the details',
  store: 'Saving results',
  research: 'Researching the subject',
  compose: 'Writing the document',
  find: 'Looking for files',
  filter: 'Checking the files',
  fetch: 'Downloading files',
}

/// Old runs printed "prospects"; the product word is artifacts.
function asArtifacts(s: string): string {
  return s.replace(/\bprospects\b/g, 'artifacts').replace(/\bprospect\b/g, 'artifact')
}

const ENGINES: [RegExp, string][] = [
  [/(^|\.)google\./, 'Google'],
  [/bing\.com/, 'Bing'],
  [/duckduckgo\.com/, 'DuckDuckGo'],
  [/search\.brave\.com/, 'Brave Search'],
  [/ecosia\.org/, 'Ecosia'],
  [/startpage\.com/, 'Startpage'],
  [/search\.yahoo\.com/, 'Yahoo'],
  [/mojeek\.com/, 'Mojeek'],
  [/searx\./, 'SearX'],
  [/baidu\.com/, 'Baidu'],
]

/// A navigation is either a search (an engine plus the words typed into it) or
/// a page that was opened. They read very differently, so they are told apart.
function navigation(url: string): { kind: 'search' | 'page'; text: string; detail?: string } {
  try {
    const u = new URL(url)
    const engine = ENGINES.find(([re]) => re.test(u.hostname))?.[1]
    if (engine) {
      const q = ['q', 'query', 'p', 'text', 'wd'].map((k) => u.searchParams.get(k)).find(Boolean)
      const page = u.searchParams.get('start') || u.searchParams.get('first') || u.searchParams.get('offset')
      if (q) {
        return {
          kind: 'search',
          text: `Searched ${engine} for “${q}”`,
          detail: page && page !== '0' ? `results page ${Math.floor(Number(page) / 10) + 1}` : undefined,
        }
      }
      return { kind: 'search', text: `Searched ${engine}` }
    }
    const path = u.pathname === '/' ? '' : u.pathname
    return { kind: 'page', text: `Opened ${u.hostname.replace(/^www\./, '')}${path}` }
  } catch {
    return { kind: 'page', text: `Opened ${url}` }
  }
}

/// One log line in, at most one activity out. Lines that only repeat what a
/// neighbouring line already said (tool results, session banners, the run
/// header) return null: the point is a trail somebody can read.
function toAct(l: LogLine): Act | null {
  const raw = l.line.replace(/\s+$/, '')
  const at = l.ts
  const seq = l.seq
  const act = (kind: ActKind, text: string, detail?: string): Act => ({ seq, kind, text, detail, at })
  let m: RegExpMatchArray | null

  if ((m = raw.match(/^═══ iteration (\d+)\/(\d+)/))) return act('iteration', `Round ${m[1]} of ${m[2]}`)
  if ((m = raw.match(/^\[\d\/\d ([a-z ]+)\]\s*(.*)$/))) {
    const name = STAGES[m[1].trim()] || m[1].trim()
    return act('stage', name, m[2] && !/^nothing/.test(m[2]) ? undefined : m[2] || undefined)
  }
  if ((m = raw.match(/^\[iteration (\d+) done\] \+(\d+) new in (.+)$/)))
    return act('done', `Round ${m[1]} finished — ${m[2]} new`, `took ${m[3]}`)
  if ((m = raw.match(/^\[planner\] (.+)$/))) {
    if (/skipped/.test(m[1])) return act('plan', 'Already knows where to look next')
    if (/suggested (\d+)/.test(m[1])) return act('plan', `Chose ${m[1].match(/suggested (\d+)/)![1]} new places to look`)
    if (/asking/.test(m[1])) return act('plan', 'Working out where to look next')
    return act('plan', m[1])
  }
  if ((m = raw.match(/^\[stop\] (.+)$/))) return act('done', asArtifacts(m[1]))
  if ((m = raw.match(/^\[done\] (.+)$/))) return act('done', asArtifacts(m[1]))
  if ((m = raw.match(/^\[warn\] (.+)$/))) return act('warn', m[1])
  if ((m = raw.match(/^\[resume\] (.+)$/))) return act('info', m[1])
  // Pool dispatch, before the run process even starts.
  if (/^queued —/.test(raw)) return act('info', 'Waiting for a worker')
  if ((m = raw.match(/^routed to (\S+) on host (\d+)/))) return act('info', `Assigned to a worker (${m[1]})`)
  if (/^picked up by/.test(raw)) return act('info', 'Worker picked it up')

  // Stage results.
  if ((m = raw.match(/^\s*→ (\d+) rows returned in (.+)$/))) return act('found', `Came back with ${m[1]} rows`, `took ${m[2]}`)
  if ((m = raw.match(/^\s*→ (\d+) candidate file\(s\) in (.+)$/))) return act('found', `Found ${m[1]} files`, `took ${m[2]}`)
  if ((m = raw.match(/^\s*→ (\d+) new · (\d+) already stored · (\d+) unmappable · (\d+) below min value · (\d+) no name$/)))
    return act('check', `${m[1]} new · ${m[2]} you already had`, Number(m[3]) + Number(m[4]) + Number(m[5]) > 0 ? `${Number(m[3]) + Number(m[4]) + Number(m[5])} discarded` : undefined)
  if ((m = raw.match(/^\s*→ (\d+) new · (\d+) already stored/))) return act('check', `${m[1]} new · ${m[2]} you already had`)
  if ((m = raw.match(/^\s*→ (\d+) to fetch · (\d+) already stored · (\d+) rejected$/)))
    return act('check', `${m[1]} to download · ${m[2]} you already had`, Number(m[3]) ? `${m[3]} rejected` : undefined)
  if ((m = raw.match(/^\s*→ “(.+)” · (\d+) words · (\d+) source\(s\) in (.+)$/)))
    return act('found', `Wrote “${m[1]}”`, `${m[2]} words from ${m[3]} sources`)
  if ((m = raw.match(/^\s*✓ stored \d+\/\d+\s+(.+)$/))) return act('stored', `Saved ${m[1].split('|')[0].trim()}`)
  if (/^\s*✓ report stored/.test(raw)) return act('stored', 'Saved the report')
  if ((m = raw.match(/^\s*✓ (.+) · (.+) · (.+)$/))) return act('stored', `Saved ${m[1]}`, m[2])
  if ((m = raw.match(/^\s*\[(\d+)\/(\d+)\] (.+)$/))) return act('read', `Looking up ${m[3]}`, `${m[1]} of ${m[2]}`)

  // The agent's own narration: "   12.3s  → browser.navigate https://…"
  if ((m = raw.match(/^\s+\S+\s+([→↩▸·⚠✖])\s(.+)$/))) {
    const [marker, body] = [m[1], m[2]]
    if (marker === '⚠' || marker === '✖') {
      // Cursor internals leaked into older runs ("Tool browser_scroll not
      // found in namespace browser"). That is the model inventing a tool,
      // not a broken scrape — hide it rather than show JSON to the reader.
      if (/not found in namespace/i.test(body) || /browser_scroll/.test(body)) return null
      if (/tool error:\s*[{[]/.test(body)) return act('warn', 'A page action failed; it tried another way')
      return act(marker === '✖' ? 'error' : 'warn', body)
    }
    if (marker !== '→') return null // results, session banners, payload notes
    let t: RegExpMatchArray | null
    if ((t = body.match(/^browser\.navigate\s+(\S+)/))) {
      const n = navigation(t[1])
      return act(n.kind, n.text, n.detail)
    }
    if (/^browser\.(snapshot|take_snapshot)/.test(body)) return act('read', 'Read the page')
    if (/^browser\.evaluate/.test(body)) return act('read', 'Pulled details off the page')
    if ((t = body.match(/^browser\.click\s+(.+)$/))) return act('read', `Clicked ${t[1]}`)
    if (/^browser\.(type|fill|press)/.test(body)) return act('read', 'Typed into the page')
    if (/^browser\.(tabs|close|resize|wait)/.test(body)) return null
    if ((t = body.match(/^(?:artifacts|prospects)\.known\s+(.+)$/))) return act('check', 'Checked names against what you already have', t[1])
    if (/^(?:artifacts|prospects)\./.test(body)) return act('check', 'Checked where it has already looked')
    return null
  }
  return null
}

export function toActivity(lines: LogLine[]): Act[] {
  const out: Act[] = []
  for (const l of lines) {
    const a = toAct(l)
    if (!a) continue
    // Reading the same page twice in a row is one action to a reader.
    const prev = out[out.length - 1]
    if (prev && prev.kind === a.kind && prev.text === a.text && !a.detail) continue
    out.push(a)
  }
  return out.length > 800 ? out.slice(-800) : out
}

/// What the header strip says: where the run has got to.
export function runProgress(acts: Act[]) {
  let round = ''
  let stage = ''
  let found = 0
  let saved = 0
  let searches = 0
  let pages = 0
  for (const a of acts) {
    if (a.kind === 'iteration') round = a.text
    if (a.kind === 'stage') stage = a.text
    if (a.kind === 'search') searches++
    if (a.kind === 'page') pages++
    if (a.kind === 'stored') saved++
    const m = a.kind === 'found' && a.text.match(/(\d+)/)
    if (m) found += Number(m[1])
  }
  return { round, stage, found, saved, searches, pages }
}

const Ico = ({ children }: { children: React.ReactNode }) => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    {children}
  </svg>
)

const ICONS: Record<ActKind, React.ReactNode> = {
  search: (
    <Ico>
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </Ico>
  ),
  page: (
    <Ico>
      <path d="M6 3h8l4 4v14H6z" />
      <path d="M14 3v4h4" />
    </Ico>
  ),
  read: (
    <Ico>
      <path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6z" />
      <circle cx="12" cy="12" r="2.5" />
    </Ico>
  ),
  check: (
    <Ico>
      <path d="M20 6 9 17l-5-5" />
    </Ico>
  ),
  found: (
    <Ico>
      <path d="M4 6h16M4 12h16M4 18h10" />
    </Ico>
  ),
  stored: (
    <Ico>
      <ellipse cx="12" cy="6" rx="8" ry="3" />
      <path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6" />
    </Ico>
  ),
  plan: (
    <Ico>
      <path d="M12 3v4M12 17v4M3 12h4M17 12h4" />
      <circle cx="12" cy="12" r="3.5" />
    </Ico>
  ),
  stage: (
    <Ico>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </Ico>
  ),
  iteration: (
    <Ico>
      <path d="M4 12a8 8 0 0 1 8-8 8 8 0 0 1 7 4" />
      <path d="M20 12a8 8 0 0 1-8 8 8 8 0 0 1-7-4" />
    </Ico>
  ),
  info: (
    <Ico>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </Ico>
  ),
  done: (
    <Ico>
      <path d="M5 21V4h9l1 2h5v9h-6l-1-2H5" />
    </Ico>
  ),
  warn: (
    <Ico>
      <path d="M12 4 2.5 20h19z" />
      <path d="M12 10v4M12 17h.01" />
    </Ico>
  ),
  error: (
    <Ico>
      <circle cx="12" cy="12" r="9" />
      <path d="m15 9-6 6M9 9l6 6" />
    </Ico>
  ),
}

const clock = (ts: string) => {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
  } catch {
    return ''
  }
}

export function RunActivity({ acts, live }: { acts: Act[]; live: boolean }) {
  if (!acts.length) {
    return <p className="muted">{live ? 'Starting up…' : 'Nothing was recorded for this execution.'}</p>
  }
  return (
    <div className="feed">
      {acts.map((a) =>
        a.kind === 'iteration' ? (
          <div className="feed-round" key={a.seq}>
            <span>{a.text}</span>
          </div>
        ) : (
          <div className={'feed-row k-' + a.kind} key={a.seq}>
            <span className="feed-ico">{ICONS[a.kind]}</span>
            <span className="feed-txt">
              {a.text}
              {a.detail && <span className="feed-detail">{a.detail}</span>}
            </span>
            <span className="feed-at">{clock(a.at)}</span>
          </div>
        ),
      )}
      {live && (
        <div className="feed-row k-live">
          <span className="feed-ico">
            <span className="feed-dot" />
          </span>
          <span className="feed-txt muted">still working…</span>
        </div>
      )}
    </div>
  )
}

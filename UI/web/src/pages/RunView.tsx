import React, { useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { api, can, fmtTokens, LogLine, Run } from '../api'
import { useAuth } from '../auth'
import { StatusBadge, useToast } from '../components/ui'
import BrowserView from '../components/BrowserView'
import { RunActivity, runProgress, toActivity } from '../components/RunActivity'

/// "Today, 2:14 PM" when it is today, otherwise a short date with the clock.
function when(s?: string | null): string {
  if (!s) return '—'
  const d = new Date(s)
  if (isNaN(d.getTime())) return s
  const time = d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
  const now = new Date()
  if (d.toDateString() === now.toDateString()) return `Today, ${time}`
  const yest = new Date(now)
  yest.setDate(now.getDate() - 1)
  if (d.toDateString() === yest.toDateString()) return `Yesterday, ${time}`
  const opts: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric' }
  if (d.getFullYear() !== now.getFullYear()) opts.year = 'numeric'
  return `${d.toLocaleDateString(undefined, opts)}, ${time}`
}

/// How long between two moments, in words. A finished run rounds to the minute
/// past a minute and a half; a run still going keeps the seconds so it ticks.
function ranFor(start: string, end: string | null, nowMs: number): string {
  const a = new Date(start).getTime()
  const b = end ? new Date(end).getTime() : nowMs
  if (isNaN(a) || isNaN(b) || b < a) return '—'
  let sec = Math.round((b - a) / 1000)
  if (end && sec >= 90) sec = Math.round(sec / 60) * 60
  if (sec < 5) return 'a few seconds'
  if (sec < 60) return `${sec} seconds`
  const min = Math.floor(sec / 60)
  const rem = sec % 60
  if (min < 60) {
    if (!end && min < 10 && rem) return `${min} min ${rem} sec`
    return min === 1 ? '1 minute' : `${min} minutes`
  }
  const hr = Math.floor(min / 60)
  const minRem = min % 60
  const hours = hr === 1 ? '1 hour' : `${hr} hours`
  if (hr < 48) return minRem ? `${hours} ${minRem} min` : hours
  const days = Math.floor(hr / 24)
  const hrRem = hr % 24
  return hrRem ? `${days} days ${hrRem} hr` : `${days} days`
}

function startedBy(trigger: string): string {
  if (trigger === 'manual') return 'You'
  if (trigger === 'schedule') return 'A schedule'
  if (trigger === 'api') return 'The API'
  return trigger || '—'
}

export default function RunView() {
  const { me } = useAuth()
  const write = can(me, 'plans')
  const { id } = useParams()
  const nav = useNavigate()
  const toast = useToast()
  const [run, setRun] = useState<Run | null>(null)
  const [lines, setLines] = useState<LogLine[]>([])
  const [away, setAway] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  const pinned = useRef(true)
  const lastSeq = useRef(0)

  const atBottom = (el: HTMLDivElement) => el.scrollHeight - el.scrollTop - el.clientHeight < 48

  const jumpLatest = () => {
    pinned.current = true
    setAway(false)
    if (box.current) box.current.scrollTop = box.current.scrollHeight
  }

  const loadRun = () => api.get<Run>(`/api/executions/${id}`).then(setRun).catch(() => {})

  useEffect(() => {
    lastSeq.current = 0
    pinned.current = true
    setAway(false)
    setLines([])
    loadRun()
    const es = new EventSource(`/api/executions/${id}/log`)
    es.addEventListener('line', (e) => {
      const l: LogLine = JSON.parse((e as MessageEvent).data)
      if (l.seq <= lastSeq.current) return
      lastSeq.current = l.seq
      setLines((prev) => (prev.length > 5000 ? [...prev.slice(-4000), l] : [...prev, l]))
    })
    // Tokens are booked after each model call; this event is that booking,
    // so the cost ticks while the run is still going.
    es.addEventListener('meter', (e) => {
      try {
        const next: Run = JSON.parse((e as MessageEvent).data)
        if (next?.execution_id) setRun(next)
      } catch {
        /* a truncated frame is ignored; the next tick or poll fills in */
      }
    })
    es.addEventListener('done', () => {
      es.close()
      loadRun()
    })
    es.onerror = () => {
      /* EventSource reconnects on its own; a finished run closes above */
    }
    const t = setInterval(loadRun, 5000)
    return () => {
      es.close()
      clearInterval(t)
    }
  }, [id])

  useEffect(() => {
    if (pinned.current && box.current) box.current.scrollTop = box.current.scrollHeight
  }, [lines])

  // Opened in a new tab, never framed: the URL is a live remote control of a
  // browser holding this workspace's signed-in sessions, and it is fetched
  // fresh each time rather than held anywhere.
  // Huntwell's own viewer, not a link out to whoever is running the browser.
  // Read-only on purpose: watching is what was asked for, and a view that
  // could drive would be a way to take the browser away from the run — or to
  // reach the sites it is signed in to.
  const [watching, setWatching] = useState(false)
  const watch = async () => {
    try {
      await api.get<{ frames: string }>(`/api/executions/${id}/browser`)
      setWatching(true)
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const cancel = async () => {
    try {
      await api.post(`/api/executions/${id}/cancel`)
      toast('Execution cancelled')
      nav(run?.plan_id ? `/app/plans/${run.plan_id}` : '/app/plans')
    } catch (e: any) {
      toast(e.message, true)
    }
  }

  const live = !!run && (run.status === 'running' || run.status === 'queued')
  // A running execution's "running for" should move without waiting on the
  // five-second poll that refreshes the rest of the record.
  const [nowMs, setNowMs] = useState(() => Date.now())
  useEffect(() => {
    if (!live) return
    const t = setInterval(() => setNowMs(Date.now()), 1000)
    return () => clearInterval(t)
  }, [live])
  const acts = toActivity(lines)
  const prog = runProgress(acts)
  return (
    <>
      {watching && <BrowserView id={id!} onClose={() => setWatching(false)} />}
      <div className="page-head run-head">
        <div className="run-head-top">
          <h1>
            Execution #{id} {run && <StatusBadge status={run.status} />}
          </h1>
          <div className="row">
            {live && run?.watchable && (
              <button className="btn" onClick={watch}>
                ▷ Watch the browser
              </button>
            )}
            {live && write && (
              <button className="btn danger" onClick={cancel}>
                ■ Cancel execution
              </button>
            )}
            {run && !live && run.new_prospects > 0 && (
              <Link to={`/app/prospects?plan_id=${run.plan_id}`} className="btn primary">
                See artifacts
              </Link>
            )}
          </div>
        </div>
        {run && (
          <dl className="run-facts">
            <div>
              <dt>Plan</dt>
              <dd>
                <Link to={`/app/plans/${run.plan_id}`}>{run.source}</Link>
              </dd>
            </div>
            <div>
              <dt>Started</dt>
              <dd>{when(run.started_at)}</dd>
            </div>
            <div>
              <dt>Finished</dt>
              <dd>{run.finished_at ? when(run.finished_at) : run.status === 'queued' ? 'Not yet' : live ? 'Still going' : '—'}</dd>
            </div>
            <div>
              <dt>{run.status === 'queued' ? 'Waiting' : live ? 'Running for' : 'Ran for'}</dt>
              <dd>{ranFor(run.started_at, live ? null : run.finished_at, nowMs)}</dd>
            </div>
            <div>
              <dt>Started by</dt>
              <dd>{startedBy(run.trigger)}</dd>
            </div>
          </dl>
        )}
      </div>
      {run && (live || run.tokens > 0 || run.new_prospects > 0) && (
        <div className="run-cost">
          <span className="rc-amount">{fmtTokens(run.tokens)}</span>
          <span className="muted">tokens</span>
          <span className="rc-sep" />
          <span>
            <b>{run.new_prospects}</b> found
          </span>
          {run.new_prospects > 0 && run.tokens > 0 ? (
            <span className="muted">{fmtTokens(Math.round(run.tokens / run.new_prospects))} each</span>
          ) : (
            run.status !== 'running' &&
            run.status !== 'queued' && <span className="danger">nothing found for it</span>
          )}
        </div>
      )}

      {(prog.round || prog.stage) && (
        <div className="run-strip">
          {prog.round && <span className="rs-round">{prog.round}</span>}
          {prog.stage && <span className="rs-stage">{prog.stage}</span>}
          <span className="rs-counts">
            {prog.searches} search{prog.searches === 1 ? '' : 'es'} · {prog.pages} page{prog.pages === 1 ? '' : 's'} opened ·{' '}
            {prog.saved} saved
          </span>
        </div>
      )}

      <div className="feed-wrap">
        <div
          className="feed-box"
          ref={box}
          onScroll={() => {
            const el = box.current
            if (!el) return
            const bottom = atBottom(el)
            pinned.current = bottom
            setAway(!bottom)
          }}
        >
          <RunActivity acts={acts} live={live} />
        </div>
        {away && (
          <button type="button" className="btn sm feed-jump" onClick={jumpLatest}>
            Jump to latest
          </button>
        )}
      </div>
    </>
  )
}

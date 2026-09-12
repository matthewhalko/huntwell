import React, { useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { api, fmtDate, fmtTokens, LogLine, Run, usd } from '../api'
import { StatusBadge, useToast } from '../components/ui'
import { RunActivity, runProgress, toActivity } from '../components/RunActivity'

export default function RunView() {
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
  const watch = async () => {
    try {
      const r = await api.get<{ url: string }>(`/api/executions/${id}/browser`)
      window.open(r.url, '_blank', 'noopener,noreferrer')
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
  const acts = toActivity(lines)
  const prog = runProgress(acts)
  return (
    <>
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
            {live && (
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
          <div className="sub">
            <Link to={`/app/plans/${run.plan_id}`}>{run.source}</Link> · {run.trigger} · started {fmtDate(run.started_at)}
            {run.finished_at && <> · finished {fmtDate(run.finished_at)}</>}
            {!live && run.new_prospects > 0 && <> · +{run.new_prospects} new artifact{run.new_prospects === 1 ? '' : 's'}</>}
          </div>
        )}
      </div>
      {run && (live || run.cost_usd > 0 || run.new_prospects > 0) && (
        <div className="run-cost">
          <span className="rc-amount">{usd(run.cost_usd)}</span>
          <span className="muted">{fmtTokens(run.tokens)} tokens</span>
          <span className="rc-sep" />
          <span>
            <b>{run.new_prospects}</b> found
          </span>
          {run.cost_per_result != null ? (
            <span className="muted">{usd(run.cost_per_result)} each</span>
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

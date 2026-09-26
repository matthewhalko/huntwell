import React, { useEffect, useMemo, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { fmtTokens, Run, usd } from '../api'

export type ChartUnit = 'tokens' | 'usd'

function axisUsd(n: number): string {
  if (n >= 1000) return `$${(n / 1000).toFixed(1)}k`
  if (n >= 10) return `$${Math.round(n)}`
  return `$${n.toFixed(2)}`
}

/// Tokens or dollars each run spent, oldest on the left.
///
/// Drawn in CSS pixels at a fixed height so widening the page makes bars
/// thinner, not axis type larger. `lookback` is how many recent runs to keep.
export function TokenChart({ runs, unit, lookback = 100 }: { runs: Run[]; unit: ChartUnit; lookback?: number }) {
  const nav = useNavigate()
  const wrapRef = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  const [tip, setTip] = useState<{ x: number; y: number; r: Run } | null>(null)
  const take = Math.min(200, Math.max(10, Math.round(lookback) || 100))
  const rows = useMemo(
    () => [...runs].sort((a, b) => a.execution_id - b.execution_id).slice(-take),
    [runs, take],
  )

  useEffect(() => {
    const el = wrapRef.current
    if (!el) return
    const measure = () => setWidth(el.clientWidth)
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const value = (r: Run) => (unit === 'usd' ? r.cost_usd || 0 : r.tokens || 0)
  const label = (n: number) => (unit === 'usd' ? axisUsd(n) : fmtTokens(Math.round(n)))
  const max = Math.max(unit === 'usd' ? 0.01 : 1, ...rows.map(value))
  const W = Math.max(280, width)
  const H = 220
  const L = unit === 'usd' ? 52 : 46
  const B = 28
  const T = 10
  const R = 8
  const innerW = W - L - R
  const innerH = H - T - B
  const n = rows.length
  const gap = n > 60 ? 1 : n > 30 ? 1.5 : n > 16 ? 3 : 5
  const bw = n ? Math.max(2, (innerW - gap * Math.max(0, n - 1)) / n) : 0
  const xEvery = n <= 12 ? 1 : Math.ceil(n / 12)

  if (!rows.length) {
    return <p className="muted">Run a search and token use per execution shows up here.</p>
  }

  return (
    <div className="token-chart-wrap" ref={wrapRef}>
      {width > 0 && (
        <svg
          className="token-chart"
          width={W}
          height={H}
          role="img"
          aria-label={unit === 'usd' ? 'Dollars spent per execution' : 'Tokens used per execution'}
        >
          {[0, 0.5, 1].map((p) => {
            const v = max * p
            const y = T + innerH - p * innerH
            return (
              <g key={p}>
                <line x1={L} x2={W - R} y1={y} y2={y} className="grid" />
                <text x={L - 6} y={y + 3.5} textAnchor="end" className="tick">
                  {label(v)}
                </text>
              </g>
            )
          })}
          <line x1={L} x2={L} y1={T} y2={T + innerH} className="axis" />
          <line x1={L} x2={W - R} y1={T + innerH} y2={T + innerH} className="axis" />
          {rows.map((r, i) => {
            const h = (value(r) / max) * innerH
            const x = L + i * (bw + gap)
            const y = T + innerH - h
            return (
              <g key={r.execution_id}>
                <rect
                  className="bar"
                  x={x}
                  y={h ? y : T + innerH - 1}
                  width={bw}
                  height={h || 1}
                  rx={Math.min(2, bw / 2)}
                  onClick={() => nav(`/app/executions/${r.execution_id}`)}
                  onMouseMove={(e) => {
                    const box = (e.currentTarget.ownerSVGElement as SVGSVGElement).getBoundingClientRect()
                    setTip({ x: e.clientX - box.left, y: e.clientY - box.top, r })
                  }}
                  onMouseLeave={() => setTip(null)}
                >
                  <title>
                    #{r.execution_id} · {unit === 'usd' ? usd(r.cost_usd || 0) : `${fmtTokens(r.tokens)} tokens`}
                  </title>
                </rect>
                {i % xEvery === 0 && (
                  <text x={x + bw / 2} y={H - 8} textAnchor="middle" className="tick">
                    #{r.execution_id}
                  </text>
                )}
              </g>
            )
          })}
        </svg>
      )}
      {tip && (
        <div className="token-chart-tip" style={{ left: tip.x, top: tip.y }}>
          <b>#{tip.r.execution_id}</b> {tip.r.source}
          <div className="muted">{unit === 'usd' ? usd(tip.r.cost_usd || 0) : `${fmtTokens(tip.r.tokens || 0)} tokens`}</div>
        </div>
      )}
    </div>
  )
}

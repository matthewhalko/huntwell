import React, { useEffect, useRef, useState } from 'react'
import { api, fmtTokens } from '../api'
import { seedLabel } from './SearchSeeds'

/**
 * A plan's knowledge graph: nodes and edges on a canvas.
 *
 * Searches, pages and results (and the seeds that started them) are the
 * nodes; the trail edges are what led to what. A small spring layout
 * settles the picture. Drag to pan, scroll to zoom, drag a node to pin it.
 */
type Kind = 'seed' | 'query' | 'page' | 'result'

interface Node {
  id: string
  kind: Kind
  label: string
  sub: string
  tokens: number
  x: number
  y: number
  vx: number
  vy: number
  r: number
}

interface Edge {
  from: string
  to: string
  weight: number
}

interface GraphData {
  seeds?: { seed_key: string; seed_json: string; status: string }[]
  queries: { query_key: string; query: string; engine: string; hits: number; max_depth: number; new_prospects: number; tokens: number }[]
  pages: { url_key?: string; url: string; host: string; visits: number }[]
  results: { key: string; label: string; sub: string }[]
  edges: { from?: string; to?: string; from_key?: string; to_key?: string; weight: number }[]
  totals: { seeds?: number; queries: number; pages: number; results: number; tokens: number; executions: number }
  live: boolean
}

interface Palette {
  seed: string
  query: string
  page: string
  result: string
  edge: string
  text: string
  surface: string
  font: string
}

type Pt = { x: number; y: number }

function pageId(p: GraphData['pages'][number]) {
  return p.url_key || p.url
}

function readPalette(el: HTMLElement | null): Palette {
  const cs = getComputedStyle(el || document.documentElement)
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback
  return {
    seed: v('--brand-pink', '#e01e5a'),
    query: v('--brand-yellow', '#ecb22e'),
    page: v('--brand-sky', '#36c5f0'),
    result: v('--brand-green', '#2eb67d'),
    edge: v('--border-strong', '#888'),
    text: v('--text', '#111'),
    surface: v('--surface', '#fff'),
    font: v('--font', 'sans-serif'),
  }
}

const KIND_LABEL: Record<Kind, string> = {
  seed: 'seeds',
  query: 'searches',
  page: 'pages',
  result: 'results',
}

export function KnowledgeGraph({ planId, live: runLive }: { planId: number; live?: boolean }) {
  const [data, setData] = useState<GraphData | null>(null)
  const [err, setErr] = useState('')
  const [hover, setHover] = useState<Node | null>(null)
  const [show, setShow] = useState<Record<Kind, boolean>>({ seed: true, query: true, page: true, result: true })
  const [pal, setPal] = useState<Palette>(() => readPalette(null))
  const wrapRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const nodesRef = useRef<Node[]>([])
  const edgesRef = useRef<Edge[]>([])
  const posRef = useRef<Map<string, Pt>>(new Map())
  const alphaRef = useRef(1)
  const viewRef = useRef({ x: 0, y: 0, k: 1 })
  const dragRef = useRef<{ x: number; y: number; node: Node | null } | null>(null)
  const fittedRef = useRef(false)

  useEffect(() => {
    const sample = () => setPal(readPalette(wrapRef.current))
    sample()
    const obs = new MutationObserver(sample)
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    mq.addEventListener('change', sample)
    return () => {
      obs.disconnect()
      mq.removeEventListener('change', sample)
    }
  }, [])

  const load = () =>
    api
      .get<GraphData>(`/api/plans/${planId}/graph`)
      .then((d) => {
        setData(d)
        setErr('')
      })
      .catch((e) => setErr(e.message || 'Could not load the graph'))

  useEffect(() => {
    posRef.current = new Map()
    fittedRef.current = false
    viewRef.current = { x: 0, y: 0, k: 1 }
    load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planId])

  const following = !!runLive || !!data?.live
  useEffect(() => {
    if (!following) return
    const t = setInterval(load, 3000)
    return () => clearInterval(t)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [following, planId])

  useEffect(() => {
    if (!data) return
    const seen = posRef.current
    const nodes: Node[] = []
    const push = (id: string, kind: Kind, label: string, sub: string, weight: number, tokens = 0) => {
      const at = seen.get(id)
      const band = kind === 'seed' ? -220 : kind === 'query' ? -80 : kind === 'page' ? 80 : 220
      nodes.push({
        id,
        kind,
        label,
        sub,
        tokens,
        x: at ? at.x : band + (Math.random() - 0.5) * 140,
        y: at ? at.y : (Math.random() - 0.5) * 400,
        vx: 0,
        vy: 0,
        r: Math.min(18, 5 + Math.sqrt(Math.max(weight, 1)) * 2),
      })
    }
    const aliases = new Map<string, string>()
    const alias = (key: string | undefined, id: string) => {
      if (key) aliases.set(key, id)
    }
    for (const s of data.seeds || []) {
      push(s.seed_key, 'seed', seedLabel(s.seed_json), s.status === 'pending' ? 'waiting' : 'used', 4)
      alias(s.seed_key, s.seed_key)
    }
    for (const q of data.queries) {
      push(q.query_key, 'query', q.query, `${q.engine} · ${q.hits}× · ${q.new_prospects} found`, q.new_prospects + 1, q.tokens)
      alias(q.query_key, q.query_key)
      alias(q.query, q.query_key)
    }
    for (const p of data.pages) {
      const id = pageId(p)
      push(id, 'page', p.host.replace(/^www\./, ''), p.url, p.visits || 1)
      alias(id, id)
      alias(p.url_key, id)
      alias(p.url, id)
    }
    for (const r of data.results) {
      push(r.key, 'result', r.label, r.sub, 3)
      alias(r.key, r.key)
    }

    const ids = new Set(nodes.map((n) => n.id))
    const resolve = (k?: string) => {
      if (!k) return ''
      return aliases.get(k) || (ids.has(k) ? k : '')
    }
    const edges: Edge[] = []
    for (const e of data.edges || []) {
      const from = resolve(e.from || e.from_key)
      const to = resolve(e.to || e.to_key)
      if (from && to && from !== to) edges.push({ from, to, weight: e.weight || 1 })
    }

    const grew = nodes.length !== nodesRef.current.length
    nodesRef.current = nodes
    edgesRef.current = edges
    if (grew) alphaRef.current = Math.max(alphaRef.current, 0.5)
  }, [data])

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const onWheel = (e: WheelEvent) => {
      e.preventDefault()
      const k = viewRef.current.k * (e.deltaY < 0 ? 1.1 : 0.9)
      viewRef.current.k = Math.min(3.2, Math.max(0.2, k))
    }
    canvas.addEventListener('wheel', onWheel, { passive: false })
    return () => canvas.removeEventListener('wheel', onWheel)
  }, [])

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    let raf = 0

    const step = () => {
      const nodes = nodesRef.current.filter((n) => show[n.kind])
      const visible = new Set(nodes.map((n) => n.id))
      const edges = edgesRef.current.filter((e) => visible.has(e.from) && visible.has(e.to))
      const byId = new Map(nodes.map((n) => [n.id, n]))

      if (alphaRef.current > 0.004) {
        const a0 = alphaRef.current
        for (let i = 0; i < nodes.length; i++) {
          const a = nodes[i]
          for (let j = i + 1; j < nodes.length; j++) {
            const b = nodes[j]
            let dx = b.x - a.x
            let dy = b.y - a.y
            const d2 = dx * dx + dy * dy || 0.01
            if (d2 > 90000) continue
            const f = (900 * a0) / d2
            const d = Math.sqrt(d2)
            dx /= d
            dy /= d
            a.vx -= dx * f
            a.vy -= dy * f
            b.vx += dx * f
            b.vy += dy * f
          }
        }
        for (const e of edges) {
          const a = byId.get(e.from)!
          const b = byId.get(e.to)!
          const dx = b.x - a.x
          const dy = b.y - a.y
          const d = Math.sqrt(dx * dx + dy * dy) || 0.01
          const f = ((d - 130) * 0.02 * a0) / d
          a.vx += dx * f
          a.vy += dy * f
          b.vx -= dx * f
          b.vy -= dy * f
        }
        for (const n of nodes) {
          n.vx += -n.x * 0.0015 * a0
          n.vy += -n.y * 0.0015 * a0
          n.vx *= 0.86
          n.vy *= 0.86
          n.x += n.vx
          n.y += n.vy
          posRef.current.set(n.id, { x: n.x, y: n.y })
        }
        alphaRef.current *= 0.985
      }

      const dpr = window.devicePixelRatio || 1
      const w = canvas.clientWidth
      const h = canvas.clientHeight
      if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
        canvas.width = w * dpr
        canvas.height = h * dpr
      }
      const ctx = canvas.getContext('2d')!
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      ctx.fillStyle = pal.surface
      ctx.fillRect(0, 0, w, h)

      if (!fittedRef.current && nodes.length) {
        let minX = Infinity
        let maxX = -Infinity
        let minY = Infinity
        let maxY = -Infinity
        for (const n of nodes) {
          minX = Math.min(minX, n.x)
          maxX = Math.max(maxX, n.x)
          minY = Math.min(minY, n.y)
          maxY = Math.max(maxY, n.y)
        }
        const bw = Math.max(240, maxX - minX + 160)
        const bh = Math.max(180, maxY - minY + 140)
        viewRef.current.k = Math.min(1.2, (w * 0.86) / bw, (h * 0.8) / bh)
        viewRef.current.x = -((minX + maxX) / 2) * viewRef.current.k
        viewRef.current.y = -((minY + maxY) / 2) * viewRef.current.k
        fittedRef.current = true
      }

      const view = viewRef.current
      ctx.save()
      ctx.translate(w / 2 + view.x, h / 2 + view.y)
      ctx.scale(view.k, view.k)

      for (const e of edges) {
        const a = byId.get(e.from)!
        const b = byId.get(e.to)!
        const focused = hover && (hover.id === e.from || hover.id === e.to)
        ctx.strokeStyle = pal.edge
        ctx.globalAlpha = hover ? (focused ? 1 : 0.12) : 0.7
        ctx.lineWidth = focused ? 2.2 : 1.2 + Math.min(2, e.weight * 0.3)
        ctx.beginPath()
        ctx.moveTo(a.x, a.y)
        ctx.lineTo(b.x, b.y)
        ctx.stroke()
      }
      ctx.globalAlpha = 1

      for (const n of nodes) {
        const color = pal[n.kind]
        ctx.globalAlpha = hover && hover.id !== n.id ? 0.4 : 1
        ctx.beginPath()
        ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2)
        ctx.fillStyle = color
        ctx.fill()
        if (view.k > 0.7 && (n.kind !== 'page' || n.r > 7 || hover?.id === n.id)) {
          ctx.fillStyle = pal.text
          ctx.font = `500 11px ${pal.font}`
          ctx.textAlign = 'center'
          ctx.textBaseline = 'top'
          const label = n.label.length > 28 ? n.label.slice(0, 28) + '…' : n.label
          ctx.fillText(label, n.x, n.y + n.r + 5)
        }
        ctx.globalAlpha = 1
      }

      ctx.restore()
      raf = requestAnimationFrame(step)
    }
    raf = requestAnimationFrame(step)
    return () => cancelAnimationFrame(raf)
  }, [show, hover, data, pal])

  const toWorld = (ev: React.MouseEvent) => {
    const c = canvasRef.current!
    const rect = c.getBoundingClientRect()
    const view = viewRef.current
    return {
      x: (ev.clientX - rect.left - rect.width / 2 - view.x) / view.k,
      y: (ev.clientY - rect.top - rect.height / 2 - view.y) / view.k,
    }
  }
  const nodeAt = (p: Pt) =>
    nodesRef.current.find((n) => show[n.kind] && (n.x - p.x) ** 2 + (n.y - p.y) ** 2 < (n.r + 6) ** 2) || null

  const kindColor = (k: Kind) => pal[k]
  const count = (k: Kind) => {
    if (!data) return '—'
    if (k === 'seed') return data.totals.seeds ?? data.seeds?.length ?? 0
    if (k === 'query') return data.totals.queries
    if (k === 'page') return data.totals.pages
    return data.totals.results
  }

  return (
    <div className="graph" ref={wrapRef}>
      {err && <div className="error">{err}</div>}
      <div className="graph-stage">
        <div className="graph-bar">
          {(['seed', 'query', 'page', 'result'] as Kind[]).map((k) => (
            <button key={k} className={'graph-key' + (show[k] ? '' : ' off')} onClick={() => setShow({ ...show, [k]: !show[k] })}>
              <span className="dot" style={{ background: kindColor(k) }} />
              {KIND_LABEL[k]}
              <b>{count(k)}</b>
            </button>
          ))}
          {following && (
            <span className="graph-live">
              <span className="dot" /> live
            </span>
          )}
          <span className="graph-tokens">{data ? `${fmtTokens(data.totals.tokens)} tokens across ${data.totals.executions} execution(s)` : ''}</span>
        </div>
        <canvas
          ref={canvasRef}
          onMouseMove={(e) => {
            const p = toWorld(e)
            const d = dragRef.current
            if (d?.node) {
              d.node.x = p.x
              d.node.y = p.y
              d.node.vx = 0
              d.node.vy = 0
              posRef.current.set(d.node.id, { x: p.x, y: p.y })
              return
            }
            if (d) {
              viewRef.current.x += e.clientX - d.x
              viewRef.current.y += e.clientY - d.y
              dragRef.current = { x: e.clientX, y: e.clientY, node: null }
              return
            }
            setHover(nodeAt(p))
          }}
          onMouseDown={(e) => {
            dragRef.current = { x: e.clientX, y: e.clientY, node: nodeAt(toWorld(e)) }
          }}
          onMouseUp={() => (dragRef.current = null)}
          onMouseLeave={() => {
            dragRef.current = null
            setHover(null)
          }}
        />
        {hover && (
          <div className="graph-tip">
            <div className="tip-kind" style={{ color: kindColor(hover.kind) }}>
              {hover.kind === 'query' ? 'search' : hover.kind}
            </div>
            <div className="tip-label">{hover.label}</div>
            {hover.sub && <div className="tip-sub">{hover.sub}</div>}
            {hover.kind === 'query' && hover.tokens > 0 && <div className="tip-sub">≈ {fmtTokens(hover.tokens)} tokens spent here</div>}
          </div>
        )}
        {data && !data.queries.length && !data.pages.length && !data.seeds?.length && (
          <div className="graph-empty">Nothing mapped yet — run this plan and its graph builds itself.</div>
        )}
      </div>
    </div>
  )
}

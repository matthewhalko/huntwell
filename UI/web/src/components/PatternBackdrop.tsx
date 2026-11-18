import React from 'react'

/// Behind the sign-in screens: the aubergine ground and a dot grid that slides
/// slowly, a light that sits behind the form brightening the dots around it,
/// and now and then one dot lights up green and rings out, a find, before the
/// next turns up somewhere else. The whole surface ripples a little, as if it
/// were water: two slow, shallow waves bend every dot, the light and the finds
/// alike.
///
/// A canvas, because a ripple moves each dot by its own amount: about three
/// thousand dots, a few multiplications each, per frame. It stops while the
/// tab is hidden. Someone who asked for less motion gets the grid and the
/// light, standing still.

/// The grid's cell (px) and how far it slides each second, one cell in 24s.
const CELL = 24
const SLIDE = CELL / 24
/// The light behind the form: its size as a share of the screen's longer
/// side, never less than enough to show around the form.
const LIGHT = 0.36
const LIGHT_MIN = 430
/// The finds: how many at once, one cycle (s), and the gap between them.
const PINGS = 6
const PING_S = 9.6
const PING_GAP = 1.6
/// The ripple: how far it bends things (px), its wavelengths (px), its period (s).
const RIPPLE = 3
const WAVE_A = 300
const WAVE_B = 420
const RIPPLE_S = 7

export function PatternBackdrop() {
  const canvas = React.useRef<HTMLCanvasElement>(null)

  React.useEffect(() => {
    const cv = canvas.current
    const ctx = cv?.getContext('2d')
    if (!cv || !ctx) return
    const still = !!window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    const green = getComputedStyle(document.documentElement).getPropertyValue('--brand-green').trim() || 'green'
    let w = 0
    let h = 0
    let raf = 0
    const t0 = performance.now()
    // Each find's place; a new one each time it has rung out.
    const pings = Array.from({ length: PINGS }, () => ({ x: 0, y: 0, cycle: -1 }))

    const resize = () => {
      const dpr = Math.min(2, window.devicePixelRatio || 1)
      w = cv.clientWidth
      h = cv.clientHeight
      cv.width = Math.round(w * dpr)
      cv.height = Math.round(h * dpr)
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    }

    /// A grid point away from the middle, where the form sits, and the corner
/// where the Help button is.
    const spot = () => {
      for (let i = 0; i < 20; i++) {
        const x = Math.round((Math.random() * w) / CELL) * CELL
        const y = Math.round((Math.random() * h) / CELL) * CELL
        const behindForm = Math.abs(x - w / 2) < 260 && Math.abs(y - h / 2) < 300
        // Nor under the Help button in the corner.
        const behindHelp = x > w - 180 && y > h - 120
        if (!behindForm && !behindHelp) return { x, y }
      }
      return { x: CELL * 2, y: CELL * 2 }
    }

    /// How the water bends a point at time `s`.
    const ripple = (x: number, y: number, s: number): [number, number] => {
      const p = (2 * Math.PI * s) / RIPPLE_S
      return [
        x + RIPPLE * Math.sin((2 * Math.PI * (y + 0.4 * x)) / WAVE_A + p),
        y + RIPPLE * Math.sin((2 * Math.PI * (x - 0.3 * y)) / WAVE_B - p * 0.8),
      ]
    }

    const draw = (s: number) => {
      ctx.clearRect(0, 0, w, h)
      const r = Math.max(LIGHT_MIN, LIGHT * Math.max(w, h))
      const lx = w / 2
      const ly = h / 2
      // An oval, wider than tall like the form, not a disc.
      const rx = r * 1.3
      const ry = r * 0.82

      // The light's own glow: the same oval, very faint, fading all the way.
      ctx.save()
      ctx.translate(lx, ly)
      ctx.scale(rx / ry, 1)
      const glow = ctx.createRadialGradient(0, 0, 0, 0, 0, ry)
      glow.addColorStop(0, 'rgba(255,255,255,0.05)')
      glow.addColorStop(0.55, 'rgba(255,255,255,0.022)')
      glow.addColorStop(1, 'rgba(255,255,255,0)')
      ctx.fillStyle = glow
      ctx.fillRect(-ry, -ry, 2 * ry, 2 * ry)
      ctx.restore()

      // The grid, slid along, rippled, and brighter where the light falls.
      // The light fades smoothly (no bright plateau, no edge), and how far it
      // reaches changes slowly with direction, so its outline never settles
      // into a shape you can name.
      const shift = still ? 0 : (s * SLIDE) % CELL
      const drift = still ? 0 : s
      for (let gy = shift - CELL; gy < h + CELL; gy += CELL) {
        for (let gx = shift - CELL; gx < w + CELL; gx += CELL) {
          const [x, y] = still ? [gx, gy] : ripple(gx, gy, s)
          const nx = (gx - lx) / rx
          const ny = (gy - ly) / ry
          const angle = Math.atan2(ny, nx)
          const reach = 1 + 0.22 * Math.sin(3 * angle + 0.11 * drift) + 0.14 * Math.sin(5 * angle - 0.07 * drift + 1.3)
          const lit = Math.exp((-2.2 * (nx * nx + ny * ny)) / (reach * reach))
          ctx.fillStyle = `rgba(255,255,255,${(0.08 + 0.42 * lit).toFixed(3)})`
          ctx.beginPath()
          ctx.arc(x, y, 1.2 + 0.2 * lit, 0, Math.PI * 2)
          ctx.fill()
        }
      }

      if (still) return
      // The finds: a dot that glows green, a ring that spreads, then gone.
      for (let i = 0; i < PINGS; i++) {
        const local = s + i * PING_GAP
        const cycle = Math.floor(local / PING_S)
        const p = pings[i]
        if (p.cycle !== cycle) Object.assign(p, spot(), { cycle })
        const f = (local % PING_S) / PING_S
        const [x, y] = ripple(p.x + shift, p.y + shift, s)
        // The dot: up fast, held, gone by 30% of the cycle.
        const dot = f < 0.04 ? f / 0.04 : f < 0.1 ? 1 : f < 0.3 ? 1 - (f - 0.1) / 0.2 : 0
        if (dot > 0) {
          const grow = f < 0.04 ? 0.4 + 0.9 * (f / 0.04) : f < 0.1 ? 1.3 - 0.3 * ((f - 0.04) / 0.06) : 1
          ctx.globalAlpha = dot
          ctx.fillStyle = green
          ctx.shadowColor = green
          ctx.shadowBlur = 10
          ctx.beginPath()
          ctx.arc(x, y, 3 * grow, 0, Math.PI * 2)
          ctx.fill()
          ctx.shadowBlur = 0
        }
        // The ring: from 3% to 28%, spreading to nine times the dot.
        if (f > 0.03 && f < 0.28) {
          const g = (f - 0.03) / 0.25
          ctx.globalAlpha = 0.9 * (1 - g)
          ctx.strokeStyle = green
          ctx.lineWidth = 1.5
          ctx.beginPath()
          ctx.arc(x, y, 4 + 23 * g, 0, Math.PI * 2)
          ctx.stroke()
        }
        ctx.globalAlpha = 1
      }
    }

    const frame = (now: number) => {
      draw((now - t0) / 1000)
      raf = requestAnimationFrame(frame)
    }
    const start = () => {
      if (!raf && !still) raf = requestAnimationFrame(frame)
    }
    const stop = () => {
      cancelAnimationFrame(raf)
      raf = 0
    }
    const onVisible = () => (document.visibilityState === 'visible' ? start() : stop())
    // Its own size, not the window's: the app mounts hidden under the first
    // paint's skeleton, so the canvas starts at zero and grows later.
    const ro = new ResizeObserver(() => {
      resize()
      if (still) draw(0)
    })
    ro.observe(cv)
    start()
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      stop()
      ro.disconnect()
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [])

  return (
    <div className="pat-backdrop" aria-hidden>
      <canvas className="pat-canvas" ref={canvas} />
      <div className="pat-vignette" />
    </div>
  )
}

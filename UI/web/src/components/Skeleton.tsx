import React, { useEffect, useRef, useState } from 'react'

/**
 * Placeholders shaped like what is loading, instead of a blank page or a
 * spinner. The classes are the ones `index.html` uses for the skeleton shown
 * before the script runs, so the hand-over from that one to these is seamless.
 * Styles: the "skeletons" section of styles.css.
 */

/** A table's worth of rows: what most data on a page looks like. */
export function SkeletonRows({ rows = 6, head = true }: { rows?: number; head?: boolean }) {
  return (
    <div className="skel-rows" aria-hidden>
      {head && (
        <div className="skel-row head">
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
        </div>
      )}
      {Array.from({ length: rows }, (_, i) => (
        <div className="skel-row" key={i}>
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
        </div>
      ))}
    </div>
  )
}

/** A page heading: title, a line under it, and an action on the right. */
export function SkeletonHead({ action = true }: { action?: boolean }) {
  return (
    <div className="skel-head" aria-hidden>
      <div>
        <span className="skel skel-title" />
        <span className="skel skel-sub" />
      </div>
      {action && <span className="skel skel-btn" />}
    </div>
  )
}

/**
 * A whole page while its data loads. `table` is a list page; `detail` is one
 * thing with tabs and cards (a plan, a run).
 */
export function PageSkeleton({ kind = 'table' }: { kind?: 'table' | 'detail' }) {
  return (
    <div aria-busy="true" aria-label="Loading">
      <SkeletonHead />
      {kind === 'detail' ? (
        <>
          <div className="skel-tabs" aria-hidden>
            <span className="skel" />
            <span className="skel" />
            <span className="skel" />
            <span className="skel" />
          </div>
          <SkeletonRows rows={3} head={false} />
          <div className="skel-cards" aria-hidden>
            {[0, 1].map((i) => (
              <div className="skel-card" key={i}>
                <span className="skel" />
                <span className="skel" />
                <span className="skel" />
                <span className="skel" />
              </div>
            ))}
          </div>
        </>
      ) : (
        <SkeletonRows />
      )}
    </div>
  )
}

/**
 * The signed-in app's frame — topbar, rail, pane — with a page skeleton in it,
 * while the session is being checked. Mirrors `Layout`'s markup (and the
 * static copy in index.html) so the real frame lands exactly on top of it.
 */
export function AppSkeleton() {
  return (
    <div className="shell" aria-busy="true" aria-label="Loading">
      <div className="topbar">
        <span className="topbar-brand">
          <span className="grad-text spark" aria-hidden>
            ✦
          </span>
          <span>huntwell</span>
        </span>
      </div>
      <aside className="rail" aria-hidden>
        <nav className="rail-nav">
          {Array.from({ length: 8 }, (_, i) => (
            <span className="rail-item" key={i}>
              <span className="tile skel" />
              <span className="rlbl skel" />
            </span>
          ))}
        </nav>
      </aside>
      <div className="pane">
        <div className="content">
          <PageSkeleton />
        </div>
      </div>
    </div>
  )
}

/** Form fields inside a sign-in card, while what the form needs is fetched. */
export function SkeletonFields({ fields = 2 }: { fields?: number }) {
  return (
    <div className="skel-fields" aria-busy="true" aria-label="Loading">
      {Array.from({ length: fields }, (_, i) => (
        <span className="skel skel-field" key={i} />
      ))}
      <span className="skel skel-btn" />
    </div>
  )
}

/** A sign-in page's card, empty: the static skeleton's twin for /login and friends. */
export function AuthSkeleton() {
  return (
    <div className="skel-auth" aria-busy="true" aria-label="Loading">
      <div className="card raised" aria-hidden>
        <span className="skel skel-title" />
        <span className="skel" />
        <span className="skel" />
        <span className="skel skel-btn" />
      </div>
    </div>
  )
}

// ---- when a skeleton shows, and for how long ----------------------------------
//
// Huntwell usually answers in tens of milliseconds, and a skeleton that shows
// for a few frames and vanishes reads as a flicker. So a skeleton waits a beat
// before appearing — most loads finish inside it and show their content at
// once — and once it has appeared it stays a minimum time, so it is seen and
// gone, never flashed.

/** A load faster than this shows no skeleton at all. */
export const SKELETON_DELAY_MS = 150
/** A skeleton, once shown, stays at least this long. */
export const SKELETON_MIN_MS = 700

export type LoadView = 'wait' | 'skeleton' | 'ready'

/**
 * What to draw while something loads: `wait` (nothing yet — it may be here
 * in a moment), `skeleton`, or `ready`. `ready` only ever comes with `ready`
 * true, so a caller can test both and keep its type narrowing:
 * `if (data === null || view !== 'ready') return view === 'skeleton' ? <Loading /> : null`.
 */
export function useLoadView(ready: boolean): LoadView {
  const [shown, setShown] = useState(false)
  const shownAt = useRef<number | null>(null)
  useEffect(() => {
    if (!ready) {
      if (shownAt.current !== null) return
      const t = setTimeout(() => {
        shownAt.current = performance.now()
        setShown(true)
      }, SKELETON_DELAY_MS)
      return () => clearTimeout(t)
    }
    if (shownAt.current === null) return
    const left = SKELETON_MIN_MS - (performance.now() - shownAt.current)
    const done = () => {
      shownAt.current = null
      setShown(false)
    }
    if (left <= 0) {
      done()
      return
    }
    const t = setTimeout(done, left)
    return () => clearTimeout(t)
  }, [ready])
  if (shown) return 'skeleton'
  return ready ? 'ready' : 'wait'
}

/** The public site's skeleton: the React twin of public/skeleton.js's. */
export function SiteSkeleton() {
  return (
    <div className="landing" aria-busy="true" aria-label="Loading">
      <header className="site-head">
        <span className="brand" style={{ color: 'var(--text)' }}>
          <span className="grad-text">✦</span>
          <span className="word">huntwell</span>
        </span>
        <div className="skel-nav" aria-hidden>
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
          <span className="skel" />
        </div>
        <span className="skel skel-btn" aria-hidden />
      </header>
      <div className="skel-hero" aria-hidden>
        <span className="skel skel-title" />
        <span className="skel skel-title" />
        <span className="skel skel-sub" />
        <span className="skel skel-sub" />
        <span className="skel skel-btn" />
      </div>
    </div>
  )
}

/** The skeleton for the page this address is — the same choice skeleton.js makes. */
function routeSkeleton(path: string) {
  if (/^\/app(\/|$)/.test(path)) return <AppSkeleton />
  if (/^\/(login|signup|forgot|verify|join)(\/|$)/.test(path)) return <AuthSkeleton />
  return <SiteSkeleton />
}

/**
 * The first load: the skeleton public/skeleton.js painted the moment the page
 * arrived stays up until SKELETON_MIN_MS after the page started, instead of
 * being replaced a few frames later. The app mounts underneath at once —
 * hidden, fetching — so the wait costs nothing it would not have spent anyway.
 */
export function FirstLoadHold({ children }: { children: React.ReactNode }) {
  const [held, setHeld] = useState(() => performance.now() < SKELETON_MIN_MS)
  useEffect(() => {
    if (!held) return
    const t = setTimeout(() => setHeld(false), Math.max(0, SKELETON_MIN_MS - performance.now()))
    return () => clearTimeout(t)
  }, [held])
  return (
    <>
      {held && routeSkeleton(window.location.pathname)}
      <div style={held ? { display: 'none' } : { display: 'contents' }}>{children}</div>
    </>
  )
}


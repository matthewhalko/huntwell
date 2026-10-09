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

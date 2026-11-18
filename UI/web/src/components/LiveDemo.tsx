import React from 'react'

/// The public site's "watch it work": a search typed into Huntwell, the
/// browser going from page to page, and rows landing in the table as it reads
/// them. Scripted, not a recording — it follows the theme, costs nothing to
/// load and never goes stale — and every scene is a use case from
/// pages/Site.tsx. The data is invented and the page says so.

type Page = {
  url: string
  /// What the status line says it is doing there.
  doing: string
  kind: 'search' | 'page'
  /// Rows read off this page, in the order they land.
  rows: [string, string, string][]
}

type Scene = { tab: string; ask: string; cols: [string, string, string]; pages: Page[] }

const SCENES: Scene[] = [
  {
    tab: 'Sales prospecting',
    ask: 'Hotel general managers on the Oregon coast',
    cols: ['Name', 'Hotel', 'Email'],
    pages: [
      { url: 'search: oregon coast hotel general manager', doing: 'Searching the web', kind: 'search', rows: [] },
      {
        url: 'seastackinn.example/about/team',
        doing: 'Reading the team page',
        kind: 'page',
        rows: [['Dana Whitfield', 'Seastack Inn', 'dana@seastackinn.example']],
      },
      {
        url: 'coastlodging.example/members',
        doing: 'Going through a member directory',
        kind: 'page',
        rows: [
          ['Marco Ruiz', 'Tidepool Lodge', 'marco@tidepool.example'],
          ['Priya Nair', 'Driftwood Hotel', 'gm@driftwood.example'],
        ],
      },
      {
        url: 'netartsbay.example/contact',
        doing: 'Filling in a missing email',
        kind: 'page',
        rows: [['Sam Okafor', 'Netarts Bay Resort', 'sam@netartsbay.example']],
      },
    ],
  },
  {
    tab: 'Listing watch',
    ask: 'Used Land Cruisers under $40k in Texas',
    cols: ['Model', 'Year', 'Price'],
    pages: [
      { url: 'search: used land cruiser texas', doing: 'Searching the web', kind: 'search', rows: [] },
      {
        url: 'lonestarmotors.example/inventory',
        doing: 'Reading a dealer’s inventory',
        kind: 'page',
        rows: [
          ['Land Cruiser 100', '2004', '$27,900'],
          ['Land Cruiser 200', '2011', '$38,500'],
        ],
      },
      {
        url: 'txclassifieds.example/autos/toyota',
        doing: 'Checking the classifieds',
        kind: 'page',
        rows: [['Land Cruiser 80', '1995', '$31,000']],
      },
      {
        url: 'hillcountry4x4.example/for-sale',
        doing: 'Skipping one over $40k',
        kind: 'page',
        rows: [['Land Cruiser 100', '2006', '$34,250']],
      },
    ],
  },
  {
    tab: 'Hiring',
    ask: 'Remote React jobs posted this week',
    cols: ['Role', 'Company', 'Pay'],
    pages: [
      { url: 'search: remote react developer jobs', doing: 'Searching the web', kind: 'search', rows: [] },
      {
        url: 'remoteboard.example/react',
        doing: 'Reading a job board',
        kind: 'page',
        rows: [
          ['Senior React Engineer', 'Northwind', '$160–185k'],
          ['Frontend Developer', 'Brightloop', '$120–140k'],
        ],
      },
      {
        url: 'parcelworks.example/careers',
        doing: 'Opening a careers page',
        kind: 'page',
        rows: [['React Native Engineer', 'Parcelworks', '$145k']],
      },
      {
        url: 'startupjobs.example/frontend',
        doing: 'Leaving out last week’s posts',
        kind: 'page',
        rows: [['UI Engineer', 'Lumen Health', '$130–150k']],
      },
    ],
  },
]

// The timeline, in ms from the start of a scene.
const TYPE_AT = 500
const CHAR_MS = 42
const PAGE_MS = 2300
/// When on a page the browser has loaded it and the first row is read.
const LOADED_MS = 450
const FIRST_ROW_MS = 950
const ROW_GAP_MS = 520
const HOLD_MS = 3200

function timeline(s: Scene) {
  const typed = TYPE_AT + s.ask.length * CHAR_MS
  const pressed = typed + 350
  const browse = pressed + 450
  const end = browse + s.pages.length * PAGE_MS
  return { typed, pressed, browse, end, next: end + HOLD_MS }
}

/// Every row of a scene with the moment it lands and where on its page it
/// was read.
function rowsOf(s: Scene, browse: number) {
  return s.pages.flatMap((p, pi) => p.rows.map((r, k) => ({ r, at: browse + pi * PAGE_MS + FIRST_ROW_MS + k * ROW_GAP_MS, pi, k })))
}

const reducedMotion = () => typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches

export default function LiveDemo() {
  const [scene, setScene] = React.useState(0)
  const still = React.useMemo(reducedMotion, [])
  // Time into the current scene. Someone who asked for less motion gets each
  // scene finished, and it changes only when they pick another tab.
  const [t, setT] = React.useState(() => (still ? Infinity : 0))
  const ref = React.useRef<HTMLDivElement>(null)
  const [onScreen, setOnScreen] = React.useState(false)

  React.useEffect(() => {
    const el = ref.current
    if (!el || typeof IntersectionObserver === 'undefined') return setOnScreen(true)
    const io = new IntersectionObserver(([e]) => setOnScreen(e.isIntersecting), { threshold: 0.35 })
    io.observe(el)
    return () => io.disconnect()
  }, [])

  const s = SCENES[scene]
  const tl = timeline(s)

  // Only runs while it can be seen, so nobody scrolls down to a scene that
  // finished without them.
  React.useEffect(() => {
    if (still || !onScreen) return
    let last = performance.now()
    const id = setInterval(() => {
      const now = performance.now()
      // The step is taken now: React may run the updater later, after
      // `last` has moved on.
      const step = now - last
      if (document.visibilityState === 'visible') setT((x) => x + step)
      last = now
    }, 50)
    return () => clearInterval(id)
  }, [still, onScreen])

  React.useEffect(() => {
    if (t >= tl.next && t !== Infinity) {
      setScene((n) => (n + 1) % SCENES.length)
      setT(0)
    }
  }, [t, tl.next])

  const pick = (n: number) => {
    setScene(n)
    setT(still ? Infinity : 0)
  }

  const typed = s.ask.slice(0, Math.max(0, Math.floor((t - TYPE_AT) / CHAR_MS)))
  const typing = t < tl.typed + 200
  const pressing = t >= tl.pressed && t < tl.pressed + 220
  const browsing = t >= tl.browse
  const done = t >= tl.end
  const pi = Math.min(s.pages.length - 1, Math.max(0, Math.floor((t - tl.browse) / PAGE_MS)))
  const page = s.pages[pi]
  const into = t - tl.browse - pi * PAGE_MS
  const loaded = done || into >= LOADED_MS
  const rows = rowsOf(s, tl.browse)
  const landed = rows.filter((x) => t >= x.at)
  // The block on the page being read right now, if any.
  const reading = done ? -1 : rows.find((x) => x.pi === pi && t >= x.at - 380 && t < x.at + 260)?.k ?? -1
  const secs = browsing ? (Math.min(t, tl.end) - tl.browse) / 1000 : 0
  // Shown time runs about thirty times faster than the scene: a real run
  // takes minutes, and saying "0:09" would claim otherwise.
  const shown = Math.round(secs * 30)
  const clock = `${Math.floor(shown / 60)}:${String(shown % 60).padStart(2, '0')}`

  const status = !browsing
    ? typing
      ? 'Describe what you want in plain words'
      : 'Starting a browser…'
    : done
      ? `Done — ${landed.length} results, deduplicated and saved`
      : `${page.doing}…`

  return (
    <div className="demo" ref={ref}>
      <div className="demo-tabs" role="group" aria-label="Pick an example">
        {SCENES.map((x, n) => (
          <button key={x.tab} type="button" className={'demo-tab' + (n === scene ? ' on' : '')} aria-pressed={n === scene} onClick={() => pick(n)}>
            {x.tab}
          </button>
        ))}
      </div>

      <div
        className="demo-win"
        role="img"
        aria-label={`An example search: "${s.ask}". Huntwell opens pages in a browser one after another and adds a row to the results table for each match it reads: ${s.cols.join(', ')}.`}
      >
        <div className="demo-bar" aria-hidden>
          <span className="demo-dots">
            <i />
            <i />
            <i />
          </span>
          <span className="demo-addr">huntwell.ai/app</span>
        </div>

        <div className="demo-body" aria-hidden>
          <div className="demo-ask">
            <span className="demo-ask-text">
              {typed || <span className="demo-ph">What are you looking for?</span>}
              {typing && <span className="demo-caret" />}
            </span>
            <span className={'demo-go' + (pressing ? ' down' : '') + (browsing ? ' busy' : '')}>{browsing && !done ? 'Searching' : 'Search'}</span>
          </div>

          <div className="demo-split">
            <div className={'demo-browser' + (browsing ? '' : ' idle')}>
              <div className="demo-url">
                <span className="demo-lock">{page.kind === 'search' ? '⌕' : '🔒'}</span>
                <span className="demo-url-text" key={browsing ? pi : -1}>
                  {browsing ? page.url : 'about:blank'}
                </span>
                {browsing && !loaded && <span className="demo-load" />}
              </div>
              <div className="demo-page" key={`${scene}-${browsing ? pi : -1}`}>
                {!browsing ? (
                  <div className="demo-blank">✦</div>
                ) : !loaded ? null : page.kind === 'search' ? (
                  [0, 1, 2, 3].map((n) => (
                    <div className="demo-hit" key={n} style={{ animationDelay: `${n * 90}ms` }}>
                      <span className="demo-hit-t" style={{ width: `${[64, 52, 70, 46][n]}%` }} />
                      <span className="demo-ln" style={{ width: `${[88, 80, 92, 74][n]}%` }} />
                    </div>
                  ))
                ) : (
                  <>
                    <span className="demo-h" />
                    <span className="demo-ln" style={{ width: '82%' }} />
                    <span className="demo-ln" style={{ width: '70%' }} />
                    {[0, 1, 2].map((n) => (
                      <div className={'demo-block' + (n === reading ? ' read' : '') + (landed.some((x) => x.pi === pi && x.k === n) ? ' took' : '')} key={n}>
                        <span className="demo-ln" style={{ width: `${[58, 66, 50][n]}%` }} />
                        <span className="demo-ln" style={{ width: `${[40, 48, 36][n]}%` }} />
                      </div>
                    ))}
                  </>
                )}
              </div>
              <div className={'demo-status' + (done ? ' ok' : '')}>
                {browsing && !done && <span className="demo-spin" />}
                <span>{status}</span>
              </div>
            </div>

            <div className="demo-results">
              <div className="demo-res-head">
                <span>Results</span>
                <span className="demo-count">
                  {landed.length} found{browsing ? ` · ${clock}` : ''}
                </span>
              </div>
              <table>
                <thead>
                  <tr>
                    {s.cols.map((c) => (
                      <th key={c}>{c}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {landed.map((x) => (
                    <tr key={`${scene}-${x.pi}-${x.k}`} className={t - x.at < 1100 ? 'new' : ''}>
                      {x.r.map((v, c) => (
                        <td key={c}>{v}</td>
                      ))}
                    </tr>
                  ))}
                  {/* Empty rows keep the table its full height, so nothing
                      below it jumps as rows arrive. */}
                  {Array.from({ length: Math.max(0, 4 - landed.length) }, (_, n) => (
                    <tr className="demo-empty" key={`e${n}`}>
                      <td colSpan={3}>
                        <span className="demo-ln" />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      </div>
      <p className="demo-note">An illustration with made-up data. A real run reads many more pages and takes a few minutes.</p>
    </div>
  )
}

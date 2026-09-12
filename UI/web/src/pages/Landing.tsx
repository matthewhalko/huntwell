import React from 'react'
import { Link } from 'react-router-dom'
import { ThemeToggle } from '../theme'
import { useAuth } from '../auth'

/// Where to find Huntwell. Set a URL and its icon appears in the footer; leave
/// it empty and it does not — a marketing page linking to a profile that does
/// not exist is worse than one icon fewer.
const SOCIAL: { name: string; url: string; icon: React.ReactNode }[] = [
  {
    name: 'X',
    url: 'https://x.com/huntwell',
    icon: <path d="M18.9 2H22l-7.5 8.6L23.3 22h-6.9l-5.4-7-6.2 7H1.7l8-9.2L1 2h7.1l4.9 6.5L18.9 2zm-1.2 18h1.9L7.4 3.9H5.4L17.7 20z" />,
  },
  {
    name: 'LinkedIn',
    url: 'https://www.linkedin.com/company/huntwell',
    icon: (
      <path d="M4.98 3.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5zM3 9h4v12H3V9zm6 0h3.8v1.7h.05c.53-1 1.83-2.05 3.76-2.05 4.02 0 4.76 2.6 4.76 6V21h-4v-5.3c0-1.27-.02-2.9-1.8-2.9-1.8 0-2.07 1.38-2.07 2.8V21H9V9z" />
    ),
  },
  {
    name: 'GitHub',
    url: 'https://github.com/huntwell',
    icon: (
      <path d="M12 2a10 10 0 0 0-3.16 19.49c.5.09.68-.22.68-.48l-.01-1.7c-2.78.6-3.37-1.34-3.37-1.34-.45-1.16-1.11-1.47-1.11-1.47-.91-.62.07-.6.07-.6 1 .07 1.53 1.03 1.53 1.03.9 1.53 2.34 1.09 2.91.83.09-.65.35-1.09.63-1.34-2.22-.25-4.56-1.11-4.56-4.94 0-1.09.39-1.98 1.03-2.68-.1-.25-.45-1.27.1-2.64 0 0 .84-.27 2.75 1.02a9.5 9.5 0 0 1 5 0c1.91-1.29 2.75-1.02 2.75-1.02.55 1.37.2 2.39.1 2.64.64.7 1.03 1.59 1.03 2.68 0 3.84-2.34 4.69-4.57 4.94.36.31.68.92.68 1.85l-.01 2.75c0 .27.18.58.69.48A10 10 0 0 0 12 2z" />
    ),
  },
]

/// What Huntwell does, in the order someone new cares about it: the thing it
/// builds, the three shapes that thing comes in, and the fact that it is not a
/// solo tool. Every entry maps to something real — the plan kinds, workspaces,
/// schedules and the /v1 API — so nothing here is a promise the app can't keep.
const FEATURES: { title: string; body: string; icon: React.ReactNode }[] = [
  {
    title: 'Build a database',
    body: 'Name the columns you want and Huntwell fills them in, row by row, from whatever it finds. Cars, jobs, prices, properties. Anything with a shape.',
    icon: (
      <>
        <ellipse cx="12" cy="5" rx="8" ry="3" />
        <path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5" />
        <path d="M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" />
      </>
    ),
  },
  {
    title: 'Find sales prospects',
    body: 'Describe the companies or people you sell to and get them back with the details that matter. Who they are, where they are, how to reach them.',
    icon: (
      <>
        <circle cx="9" cy="8" r="4" />
        <path d="M2 21a7 7 0 0 1 14 0" />
        <path d="M18 8h4M20 6v4" />
      </>
    ),
  },
  {
    title: 'Compile reports',
    body: 'Ask a question instead of asking for a list and get back a written brief on the subject, sourced from what it read. Print it straight to PDF.',
    icon: (
      <>
        <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
        <path d="M14 3v5h5" />
        <path d="M9 13h6M9 17h4" />
      </>
    ),
  },
  {
    title: 'Take it with you',
    body: 'Pull your results straight into Google Sheets with a link that refreshes itself, or wire Huntwell into your own apps through the API.',
    icon: (
      <>
        <path d="M4 20h16" />
        <path d="M12 4v10" />
        <path d="m8 10 4 4 4-4" />
      </>
    ),
  },
  {
    title: 'Work as a team',
    body: 'Invite your team into a shared workspace. Everyone sees the same searches and the same results, and anyone can pick one up and run it again.',
    icon: (
      <>
        <circle cx="8" cy="9" r="3" />
        <circle cx="17" cy="10" r="2.5" />
        <path d="M2 20a6 6 0 0 1 12 0" />
        <path d="M15 20a5 5 0 0 1 7-4.6" />
      </>
    ),
  },
  {
    title: 'Keep it current',
    body: 'Put a search on a schedule and it runs itself. Every morning, every Monday, however often the answer changes. New rows just show up.',
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </>
    ),
  },
]

/// What the hero cycles through. Two shapes come back from a plan — a table of
/// rows, or one written document — so the examples alternate between them
/// rather than implying everything is a list.
type Example = { ask: string; shape: 'table' | 'doc'; cols?: [string, string, string]; got: string }

const EXAMPLES: Example[] = [
  {
    ask: 'hotel GMs on the Oregon coast',
    shape: 'table',
    cols: ['name', 'company', 'email'],
    got: 'deduped, stored, yours',
  },
  {
    ask: 'a report on Kapital\u2019s funding history',
    shape: 'doc',
    got: 'written from the pages it read',
  },
  {
    ask: 'used Land Cruisers under $40k in Texas',
    shape: 'table',
    cols: ['model', 'year', 'price'],
    got: 'deduped, stored, yours',
  },
  {
    ask: 'remote React jobs posted this week',
    shape: 'table',
    cols: ['role', 'company', 'pay'],
    got: 'deduped, stored, yours',
  },
  {
    ask: 'a report on Peru\u2019s payment rails',
    shape: 'doc',
    got: 'written from the pages it read',
  },
]

/// Rows for the table shape: three columns, widths varied so it reads as data
/// rather than a placeholder grid.
const ROWS: [number, number, number][] = [
  [62, 70, 64],
  [54, 64, 58],
  [66, 56, 62],
  [58, 68, 54],
]

/// Line widths for the document shape.
const LINES = [240, 228, 236, 210, 160]

export default function Landing() {
  // This page is reachable signed in as well as out, so it has to know which.
  const { me } = useAuth()
  const [i, setI] = React.useState(0)
  React.useEffect(() => {
    // Auto-rotation is motion; someone who asked for less of it gets the first
    // example and nothing moving.
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return
    const t = setInterval(() => setI((n) => (n + 1) % EXAMPLES.length), 3600)
    return () => clearInterval(t)
  }, [])
  const ex = EXAMPLES[i]

  return (
    <div className="landing">
      <header>
        <div className="brand" style={{ color: 'var(--text)', padding: 0 }}>
          <span className="grad-text" aria-hidden>
            ✦
          </span>
          <span className="word">huntwell</span>
        </div>
        <div className="row">
          <ThemeToggle />
          {me ? (
            <Link to="/app" className="btn primary">
              Open Huntwell
            </Link>
          ) : (
            <>
              <Link to="/login" className="btn">
                Sign in
              </Link>
              <Link to="/signup" className="btn primary">
                Get started
              </Link>
            </>
          )}
        </div>
      </header>

      <section className="hero">
        <div>
          <h1>
            Find what you are
            <br />
            <span className="grad-text">looking for</span>.
          </h1>
          <p className="lede">
            The new way to search the internet. AI-powered browsing that turns web pages into data you can use.
          </p>
          <div className="row" style={{ marginTop: '1.4rem' }}>
            {/* Signed in, "Search now" means the app. Sending someone who
                already has an account to a sign-up form is the thing the old
                redirect was hiding. */}
            <Link to={me ? '/app' : '/signup'} className="btn primary lg">
              Search now
            </Link>
            {!me && (
              <Link to="/login" className="btn lg">
                I have an account
              </Link>
            )}
          </div>
        </div>
        <div className="art">
          <svg
            className="flow"
            viewBox="0 0 320 300"
            role="img"
            aria-label="Examples of what you can ask Huntwell for, such as leads, cars, jobs or a written report, and what comes back: a table of rows, or a document."
          >
            <defs>
              <linearGradient id="fl-grad" className="fl-grad" gradientUnits="userSpaceOnUse" x1="20" y1="290" x2="300" y2="10">
                <stop offset="0" />
                <stop offset="0.38" />
                <stop offset="0.66" />
                <stop offset="1" />
              </linearGradient>
              <marker id="fl-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                <polygon className="fl-mk" points="0,1 10,5 0,9" />
              </marker>
            </defs>

            <rect className="fl-card" x="20" y="16" width="280" height="76" rx="12" />
            <text className="fl-strong fl-swap" key={`ask-${i}`} x="160" y="60" textAnchor="middle">
              {ex.ask}
              <tspan className="fl-caret"> ▍</tspan>
            </text>

            <line className="fl-line" x1="160" y1="92" x2="160" y2="124" markerEnd="url(#fl-arrow)" />
            <text className="fl-muted" x="172" y="112">runs a real browser</text>

            <rect className="fl-card" x="20" y="136" width="280" height="124" rx="12" />
            <g className="fl-swap" key={`got-${i}`}>
              {ex.shape === 'table' && ex.cols ? (
                <>
                  <text className="fl-tag" x="40" y="162">{ex.cols[0]}</text>
                  <text className="fl-tag" x="128" y="162">{ex.cols[1]}</text>
                  <text className="fl-tag" x="216" y="162">{ex.cols[2]}</text>
                  <line className="fl-rule" x1="40" y1="170" x2="280" y2="170" />
                  <text className="fl-spark" x="286" y="152" textAnchor="middle">✦</text>
                  {ROWS.map(([a, b, c], r) => (
                    <React.Fragment key={r}>
                      <rect className="fl-bar" x="40" y={182 + r * 20} width={a} height="9" rx="4" />
                      <rect className="fl-bar" x="128" y={182 + r * 20} width={b} height="9" rx="4" />
                      <rect className="fl-bar" x="216" y={182 + r * 20} width={c} height="9" rx="4" />
                    </React.Fragment>
                  ))}
                </>
              ) : (
                <>
                  <rect className="fl-bar" x="40" y="156" width="132" height="11" rx="5" />
                  <text className="fl-spark" x="286" y="167" textAnchor="middle">✦</text>
                  {LINES.map((w, r) => (
                    <rect className="fl-line-bar" key={r} x="40" y={182 + r * 15} width={w} height="6" rx="3" />
                  ))}
                </>
              )}
            </g>

            <text className="fl-muted fl-swap" key={`got-cap-${i}`} x="160" y="284" textAnchor="middle">
              {ex.got}
            </text>
          </svg>
        </div>
      </section>

      {/* What it does. Six things, each one sentence past the headline, because
          a landing page that only makes a promise leaves the reader guessing
          which of their problems it is a promise about. */}
      <section className="feats">
        <h2>
          Ask for anything. <span className="grad-text">Get data.</span>
        </h2>
        <p className="feats-lede">Lists, reports, files. Huntwell works out how to find them.</p>
        <div className="feat-grid">
          {FEATURES.map((f) => (
            <div className="feat" key={f.title}>
              <span className="feat-ico">
                <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                  {f.icon}
                </svg>
              </span>
              <h3>{f.title}</h3>
              <p>{f.body}</p>
            </div>
          ))}
        </div>
      </section>

      <footer className="site-foot">
        {/* One last invitation. A footer that only holds legal text wastes the
            best-read strip on the page. */}
        <div className="foot-cta">
          <div>
            <h2>Ready to find them?</h2>
            <p>Describe who you're after. Your first list is minutes away.</p>
          </div>
          <Link to={me ? '/app' : '/signup'} className="btn primary lg">
            Search now
          </Link>
        </div>

        <div className="foot-cols">
          <div className="foot-brand">
            <div className="brand" style={{ color: 'var(--text)', padding: 0, fontSize: '1.3rem' }}>
              <span className="grad-text" aria-hidden>
                ✦
              </span>
              <span className="word">huntwell</span>
            </div>
            {SOCIAL.some((x) => x.url) && (
              <div className="foot-social">
                {SOCIAL.filter((x) => x.url).map((x) => (
                  <a key={x.name} href={x.url} target="_blank" rel="noreferrer noopener" aria-label={x.name} title={x.name}>
                    <svg width="17" height="17" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
                      {x.icon}
                    </svg>
                  </a>
                ))}
              </div>
            )}
          </div>

          <div className="foot-col">
            <h3>Product</h3>
            <Link to="/signup">Create an account</Link>
            <Link to="/login">Sign in</Link>
            <Link to="/app/api-access">API</Link>
            <Link to="/app/usage">Usage &amp; billing</Link>
          </div>

          <div className="foot-col">
            <h3>What it finds</h3>
            <span>Companies and the people in them</span>
            <span>Custom lists of cars, jobs, tenders</span>
            <span>Written reports with sources</span>
            <span>Files and documents</span>
          </div>

          <div className="foot-col">
            <h3>How it works</h3>
            <span>Sees pages the way you do</span>
            <span>Follows the trail, link by link</span>
            <span>Remembers what it already found</span>
            <span>Goes back later to check for more</span>
          </div>
        </div>

        <div className="foot-base">
          <span>
            © {new Date().getFullYear()} Yak Systems, Inc. All rights reserved. <Link to="/terms">Terms</Link> ·{' '}
            <Link to="/privacy">Privacy</Link>
          </span>
          <span className="foot-made">So you can stop opening tabs.</span>
        </div>
      </footer>
    </div>
  )
}

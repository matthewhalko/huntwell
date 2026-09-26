import React from 'react'
import { Link } from 'react-router-dom'
import { SiteFooter, SiteHeader } from '../components/Site'
import { useAuth } from '../auth'
import { useAuthConfig } from './Auth'

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
    body: 'Download your results as a CSV whenever you like, or wire Huntwell into your own apps and CRM through the API.',
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
  // Invite-only: the sign-up page asks to join the waitlist, so say that.
  const inviteOnly = !!useAuthConfig()?.invite_only
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
      <SiteHeader />

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
              {me || !inviteOnly ? 'Search now' : 'Request access'}
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
        <div className="row" style={{ justifyContent: 'center', marginTop: '1.8rem' }}>
          <Link to="/product" className="btn">
            How it works
          </Link>
          <Link to="/use-cases" className="btn">
            See use cases
          </Link>
        </div>
      </section>

      <SiteFooter />
    </div>
  )
}

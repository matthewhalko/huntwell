import React from 'react'
import { Link, NavLink, useLocation } from 'react-router-dom'
import { ThemeToggle } from '../theme'
import { useAuth } from '../auth'
import { useAuthConfig } from '../pages/Auth'

/// The public site's pages, in the order someone new asks about them: what it
/// is, what people use it for, what it costs, how to build on it, and whether
/// it is safe. One list feeds the top menu, the phone menu and the footer.
export const SITE_PAGES: { to: string; label: string }[] = [
  { to: '/product', label: 'Product' },
  { to: '/use-cases', label: 'Use cases' },
  { to: '/pricing', label: 'Pricing' },
  { to: '/developers', label: 'Developers' },
  { to: '/security', label: 'Security' },
]

/// Titles and descriptions per public page — mirrors `PAGES` in
/// `cmd/src/web/seo.rs`, which writes them into the first response. This copy
/// keeps them right as someone moves between pages without a reload.
const PAGE_META: Record<string, { title: string; description: string }> = {
  '/': {
    title: 'Huntwell — AI web research that turns pages into data',
    description: "Describe what you're looking for and Huntwell searches the web in a real browser, returning prospects, custom tables, written reports and files you can use.",
  },
  '/product': {
    title: 'How Huntwell works — from a sentence to structured data',
    description: 'Describe it, review the plan, and Huntwell browses for you: deduplicated results, schedules that skip when nothing is new, CSV export and an API.',
  },
  '/use-cases': {
    title: 'Use cases — prospecting, listings, research and more | Huntwell',
    description: 'Sales prospecting, market and listing watch, hiring research, research briefs, tenders and document collection: what people use Huntwell for.',
  },
  '/pricing': {
    title: 'Pricing — pay as you go, no seat fees | Huntwell',
    description: 'Prepaid credits from $5 to $500. Each run is charged for the work it does, every run shows its cost, and your whole team is included.',
  },
  '/developers': {
    title: 'Developers — the Huntwell API | Huntwell',
    description: 'Create plans, start runs and sync results from your own code. HMAC-signed requests, keys you can pin to a plan or an address, and a full reference.',
  },
  '/security': {
    title: 'Security — how Huntwell keeps your data yours | Huntwell',
    description: 'Managed sign-in with two-factor, isolated workspaces, a browser per run, pages that cannot instruct the agent, and a signed API with an audit log.',
  },
  '/terms': { title: 'Terms of Service | Huntwell', description: 'The terms that apply to using Huntwell.' },
  '/privacy': { title: 'Privacy Policy | Huntwell', description: 'What Huntwell collects, why, and what you can ask us to do with it.' },
  '/login': { title: 'Sign in | Huntwell', description: '' },
  '/signup': { title: 'Request access | Huntwell', description: '' },
  '/forgot': { title: 'Reset your password | Huntwell', description: '' },
  '/verify': { title: 'Confirm your email | Huntwell', description: '' },
}

/// Keeps `<title>`, the description and the canonical link in step with the
/// route. Mounted once, in `App`. Inside the app the tab just says Huntwell.
export function RouteMeta() {
  const { pathname } = useLocation()
  React.useEffect(() => {
    const path = pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname
    const meta = PAGE_META[path]
    document.title = meta?.title ?? 'Huntwell'
    const set = (selector: string, attr: string, value: string | null) => {
      const el = document.head.querySelector(selector)
      if (el && value !== null) el.setAttribute(attr, value)
    }
    if (meta?.description) set('meta[name="description"]', 'content', meta.description)
    // Only public pages carry a canonical, written by the server with the
    // site's own address; keep its origin and move only the path.
    const canon = document.head.querySelector<HTMLLinkElement>('link[rel="canonical"]')
    if (meta?.description && canon) {
      const origin = new URL(canon.href, window.location.href).origin
      set('link[rel="canonical"]', 'href', origin + (path === '/' ? '/' : path))
    }
  }, [pathname])
  return null
}

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

/// The primary call to action, worded for who is looking: the app for someone
/// signed in, the waitlist while sign-up is invite-only, sign-up otherwise.
export function useCta(): { to: string; label: string } {
  const { me } = useAuth()
  const inviteOnly = !!useAuthConfig()?.invite_only
  if (me) return { to: '/app', label: 'Open Huntwell' }
  return { to: '/signup', label: inviteOnly ? 'Request access' : 'Get started' }
}

function Brand() {
  return (
    <Link to="/" className="brand" aria-label="Huntwell home" style={{ color: 'var(--text)', padding: 0, textDecoration: 'none' }}>
      <span className="grad-text" aria-hidden>
        ✦
      </span>
      <span className="word">huntwell</span>
    </Link>
  )
}

export function SiteHeader() {
  const { me } = useAuth()
  const cta = useCta()
  const [open, setOpen] = React.useState(false)
  const loc = useLocation()
  // A tap on a menu link navigates; the menu should not still be hanging open
  // over the page it opened.
  React.useEffect(() => setOpen(false), [loc.pathname])
  React.useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open])

  return (
    <header className="site-head">
      <Brand />
      <nav className="site-nav" aria-label="Main">
        {SITE_PAGES.map((p) => (
          <NavLink key={p.to} to={p.to} className={({ isActive }) => (isActive ? 'active' : undefined)}>
            {p.label}
          </NavLink>
        ))}
      </nav>
      <div className="row site-actions">
        <ThemeToggle />
        {!me && (
          <Link to="/login" className="btn site-signin">
            Sign in
          </Link>
        )}
        <Link to={cta.to} className="btn primary">
          {cta.label}
        </Link>
        <button
          type="button"
          className="btn site-menu-btn"
          aria-label={open ? 'Close menu' : 'Open menu'}
          aria-expanded={open}
          aria-controls="site-menu"
          onClick={() => setOpen((o) => !o)}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
            {open ? <path d="M6 6l12 12M18 6 6 18" /> : <path d="M4 7h16M4 12h16M4 17h16" />}
          </svg>
        </button>
      </div>
      {open && (
        <nav id="site-menu" className="site-menu" aria-label="Main">
          {SITE_PAGES.map((p) => (
            <NavLink key={p.to} to={p.to} className={({ isActive }) => (isActive ? 'active' : undefined)}>
              {p.label}
            </NavLink>
          ))}
          {!me && <Link to="/login">Sign in</Link>}
        </nav>
      )}
    </header>
  )
}

export function SiteFooter({ cta = true }: { cta?: boolean }) {
  const { me } = useAuth()
  const action = useCta()
  return (
    <footer className="site-foot">
      {/* One last invitation. A footer that only holds legal text wastes the
          best-read strip on the page. */}
      {cta && (
        <div className="foot-cta">
          <div>
            <h2>Ready to find them?</h2>
            <p>Describe what you're after. Your first results are minutes away.</p>
          </div>
          <Link to={me ? '/app' : action.to} className="btn primary lg">
            {me ? 'Search now' : action.label}
          </Link>
        </div>
      )}

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
          <h3>Huntwell</h3>
          {SITE_PAGES.map((p) => (
            <Link key={p.to} to={p.to}>
              {p.label}
            </Link>
          ))}
        </div>

        <div className="foot-col">
          <h3>What it finds</h3>
          <span>Companies and the people in them</span>
          <span>Custom lists of cars, jobs, tenders</span>
          <span>Written reports with sources</span>
          <span>Files and documents</span>
        </div>

        <div className="foot-col">
          <h3>Account</h3>
          {me ? <Link to="/app">Open Huntwell</Link> : <Link to="/login">Sign in</Link>}
          {!me && <Link to={action.to}>{action.label}</Link>}
          <Link to="/terms">Terms</Link>
          <Link to="/privacy">Privacy</Link>
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
  )
}

/// A public page: the header, a titled intro, the body, the footer. Every page
/// but the landing uses it, so they all read as one site.
export function SitePage({
  eyebrow,
  title,
  lede,
  children,
}: {
  eyebrow: string
  title: React.ReactNode
  lede: React.ReactNode
  children: React.ReactNode
}) {
  React.useEffect(() => {
    window.scrollTo(0, 0)
  }, [])
  return (
    <div className="landing">
      <SiteHeader />
      <main className="site-page">
        <div className="site-intro">
          <span className="site-eyebrow">{eyebrow}</span>
          <h1>{title}</h1>
          <p className="lede">{lede}</p>
        </div>
        {children}
      </main>
      <SiteFooter />
    </div>
  )
}

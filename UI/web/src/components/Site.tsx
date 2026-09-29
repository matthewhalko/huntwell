import React from 'react'
import { Link, NavLink, useLocation } from 'react-router-dom'
import { ThemeToggle } from '../theme'
import { useAuth } from '../auth'
import { useAuthConfig } from '../pages/Auth'

/// The public site's pages, in the order someone new asks about them: what it
/// is, and what people use it for. One list feeds the top menu, the phone menu
/// and the footer.
export const SITE_PAGES: { to: string; label: string }[] = [
  { to: '/product', label: 'Product' },
  { to: '/use-cases', label: 'Use cases' },
  { to: '/pricing', label: 'Pricing' },
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
    title: 'Pricing — pay for the tokens a run spends | Huntwell',
    description: 'Huntwell charges by how many tokens a browse uses. Prepaid credits, no seat fee, and a cap on every run.',
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

export function SiteFooter() {
  const { me } = useAuth()
  const action = useCta()
  return (
    <footer className="site-foot">
      <div className="foot-cols">
        <div className="foot-brand">
          <div className="brand" style={{ color: 'var(--text)', padding: 0 }}>
            <span className="grad-text" aria-hidden>
              ✦
            </span>
            <span className="word">huntwell</span>
          </div>
          <p>Describe what you are searching for, and we will find it.</p>
        </div>

        <div className="foot-col">
          <h3>Links</h3>
          {SITE_PAGES.map((p) => (
            <Link key={p.to} to={p.to}>
              {p.label}
            </Link>
          ))}
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

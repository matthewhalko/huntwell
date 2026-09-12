import React from 'react'
import { Link, NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { ThemeToggle } from '../theme'
import { useAuth } from '../auth'
import { api } from '../api'
import { browserZone } from './Setup'
import { Workspace } from '../api'

// Monochrome line icons (lucide-style): stroke follows the rail's text colour.
function Ico({ children }: { children: React.ReactNode }) {
  return (
    <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {children}
    </svg>
  )
}

const links = [
  {
    to: '/app',
    label: 'Home',
    end: true,
    ico: (
      <Ico>
        <path d="m3 10.5 9-7.5 9 7.5" />
        <path d="M5 9.5V21h14V9.5" />
      </Ico>
    ),
  },
  {
    to: '/app/plans',
    label: 'Search plans',
    ico: (
      <Ico>
        <path d="M9 3 3 5v16l6-2 6 2 6-2V3l-6 2-6-2z" />
        <path d="M9 3v16" />
        <path d="M15 5v16" />
      </Ico>
    ),
  },
  {
    to: '/app/executions',
    label: 'History',
    ico: (
      <Ico>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </Ico>
    ),
  },
  {
    to: '/app/prospects',
    label: 'Results',
    ico: (
      <Ico>
        <path d="M12 3l1.9 5.8L20 12l-6.1 3.2L12 21l-1.9-5.8L4 12l6.1-3.2z" />
      </Ico>
    ),
  },
  {
    to: '/app/usage',
    label: 'Usage',
    ico: (
      <Ico>
        <line x1="6" y1="20" x2="6" y2="12" />
        <line x1="12" y1="20" x2="12" y2="5" />
        <line x1="18" y1="20" x2="18" y2="15" />
      </Ico>
    ),
  },
  {
    to: '/app/api-access',
    label: 'API',
    ico: (
      <Ico>
        <circle cx="8" cy="16" r="3" />
        <path d="M10.5 13.5 20 4" />
        <path d="m15 9 3 3" />
      </Ico>
    ),
  },
  {
    to: '/app/settings',
    label: 'Settings',
    ico: (
      <Ico>
        <path d="M20 7h-9" />
        <path d="M14 17H5" />
        <circle cx="17" cy="17" r="3" />
        <circle cx="7" cy="7" r="3" />
      </Ico>
    ),
  },
]

/// How many rail items fit, measured rather than guessed.
///
/// Tile widths follow their labels ("Search plans" is wider than "Home"), and
/// the row has to fit a 320px phone and a 768px tablet from the same code, so
/// a hard-coded count is either too many on one or too few on the other. The
/// pass renders every tile once, measures it, and then keeps what fits.
///
/// Returns the number of leading links to show. When that is every link there
/// is no "More" tile, and the row centres itself.
function fitCount(nav: HTMLElement, total: number): number {
  const kids = Array.from(nav.children) as HTMLElement[]
  if (kids.length < total) return total // mid-measure; try again next pass
  const w = kids.map((k) => k.getBoundingClientRect().width)
  const rail = nav.parentElement
  if (!rail) return total
  const cs = getComputedStyle(rail)
  const avail = rail.clientWidth - parseFloat(cs.paddingLeft || '0') - parseFloat(cs.paddingRight || '0')
  // The gap between tiles is real width: without it this keeps one tile too
  // many and the row scrolls.
  const gap = parseFloat(getComputedStyle(nav).columnGap || '0') || 0
  const links = w.slice(0, total)
  const span = (widths: number[]) => widths.reduce((a, b) => a + b, 0) + Math.max(0, widths.length - 1) * gap
  if (span(links) <= avail) return total
  // Room has to be left for the More tile, which is the last child.
  const more = w[w.length - 1]
  let used = more
  let n = 0
  for (const x of links) {
    if (used + x + gap > avail) break
    used += x + gap
    n++
  }
  return Math.max(1, n)
}

/// The bottom rail is a different list on a phone, not just a restyled one, so
/// this has to be a JS media query rather than a CSS one.
function usePhone(): boolean {
  const q = '(max-width: 860px)'
  const [is, setIs] = React.useState(() => (typeof window === 'undefined' ? false : window.matchMedia(q).matches))
  React.useEffect(() => {
    const mq = window.matchMedia(q)
    const fn = () => setIs(mq.matches)
    mq.addEventListener('change', fn)
    return () => mq.removeEventListener('change', fn)
  }, [])
  return is
}

export default function Layout() {
  const { me, refresh } = useAuth()
  // Which workspaces this person can work in. One is the normal case, and then
  // there is nothing to choose between, so the switcher stays hidden.
  const [spaces, setSpaces] = React.useState<Workspace[]>([])
  React.useEffect(() => {
    if (!me) return
    api
      .get<{ workspaces: Workspace[] }>('/api/team/workspaces')
      .then((r) => setSpaces(r.workspaces || []))
      .catch(() => {})
  }, [me?.account_id, me?.workspace_id])
  const switchTo = async (id: number) => {
    await api.post('/api/team/switch', { workspace_id: id })
    await refresh()
    // Everything on screen belongs to the old workspace.
    window.location.assign('/app')
  }
  // The timezone is inferred, not asked for: if this browser is somewhere else
  // than the account says — a move, a trip, a new laptop — the account follows
  // it, and every schedule's next fire is recomputed server-side. Someone who
  // picked a zone in Settings is left alone; `browser_timezone` is the passive
  // channel, and the server ignores it once the choice is manual.
  React.useEffect(() => {
    if (!me || !me.onboarded || me.timezone_auto === false) return
    const zone = browserZone()
    if (!zone || zone === me.timezone) return
    api
      .put('/api/auth/me', { browser_timezone: zone })
      .then(() => refresh())
      .catch(() => {})
  }, [me?.timezone, me?.timezone_auto, me?.onboarded])
  const nav = useNavigate()
  const loc = useLocation()
  const phone = usePhone()
  const [more, setMore] = React.useState(false)
  // null means "not measured yet": the rail renders every tile for one frame so
  // it can be measured, then settles on what fits.
  const [fits, setFits] = React.useState<number | null>(null)
  const navRef = React.useRef<HTMLElement | null>(null)
  React.useLayoutEffect(() => {
    if (!phone) {
      if (fits !== null) setFits(null)
      return
    }
    if (fits !== null || !navRef.current) return
    setFits(fitCount(navRef.current, links.length))
  }, [phone, fits])
  // A rotation or a resized window changes the answer; re-measure from scratch.
  React.useEffect(() => {
    if (!phone) return
    const fn = () => setFits(null)
    window.addEventListener('resize', fn)
    window.addEventListener('orientationchange', fn)
    return () => {
      window.removeEventListener('resize', fn)
      window.removeEventListener('orientationchange', fn)
    }
  }, [phone])
  const shown = phone ? fits ?? links.length : links.length
  const railLinks = links.slice(0, shown)
  const spill = phone ? links.slice(shown) : []
  // "More" lights up when the page you are on lives inside it, so the rail
  // never looks like nothing is selected.
  const spillActive = spill.some((l) => loc.pathname.startsWith(l.to))
  // Navigating away closes it; so does Escape.
  React.useEffect(() => setMore(false), [loc.pathname])
  React.useEffect(() => {
    if (!more) return
    const fn = (e: KeyboardEvent) => e.key === 'Escape' && setMore(false)
    window.addEventListener('keydown', fn)
    return () => window.removeEventListener('keydown', fn)
  }, [more])
  const logout = async () => {
    await api.post('/api/auth/logout')
    await refresh()
    nav('/')
  }
  return (
    <div className="shell">
      {/* full-width chrome topbar; the whole Huntwell logo (emblem + wordmark)
          anchors the corner, sitting over the rail column */}
      <div className="topbar">
        {/* The wordmark goes home, the way every other site's does. */}
        <Link to="/app" className="topbar-brand" title="Huntwell — home">
          <span className="grad-text spark" aria-hidden>
            ✦
          </span>
          <span>huntwell</span>
        </Link>
        <div className="who">
          {spaces.length > 1 && (
            <select
              className="ws-switch"
              value={me?.workspace_id ?? ''}
              onChange={(e) => switchTo(+e.target.value)}
              title="Which workspace you're working in"
            >
              {spaces.map((w) => (
                <option key={w.workspace_id} value={w.workspace_id}>
                  {w.own ? `${w.name} (you)` : w.name}
                </option>
              ))}
            </select>
          )}
          <ThemeToggle />
        </div>
      </div>
      {/* Slack-style workspace rail: 36px icon tiles with tiny labels underneath
          (same pattern as yak's AppSidebar). The chrome colour matches the topbar. */}
      <aside className="rail">
        <nav className={'rail-nav' + (phone && spill.length === 0 ? ' centered' : '')} ref={navRef}>
          {railLinks.map((l) => (
            <NavLink key={l.to} to={l.to} end={l.end} className={({ isActive }) => 'rail-item' + (isActive ? ' active' : '')} title={l.label}>
              <span className="tile">{l.ico}</span>
              <span className="rlbl">{l.label}</span>
            </NavLink>
          ))}
          {/* Everything that did not fit, behind one tile. It is rendered during
              the measuring pass too, so its width is part of the sum. */}
          {phone && (fits === null || spill.length > 0) && (
            <button
              className={'rail-item' + (spillActive || more ? ' active' : '')}
              onClick={() => setMore((v) => !v)}
              aria-haspopup="menu"
              aria-expanded={more}
              title="More"
            >
              <span className="tile">
                <Ico>
                  <circle cx="5" cy="12" r="1.4" />
                  <circle cx="12" cy="12" r="1.4" />
                  <circle cx="19" cy="12" r="1.4" />
                </Ico>
              </span>
              <span className="rlbl">More</span>
            </button>
          )}
        </nav>
        <div className="spacer" />
        {/* On a phone this lives in the More sheet instead. */}
        {!phone && (
          <button className="rail-item" onClick={logout} title={'Sign out' + (me?.email ? ` (${me.email})` : '')}>
            <span className="tile">
              <Ico>
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                <path d="m16 17 5-5-5-5" />
                <path d="M21 12H9" />
              </Ico>
            </span>
            <span className="rlbl">Sign out</span>
          </button>
        )}
      </aside>
      {/* What "More" opens: the rail items that did not fit, plus Sign out,
          which is the one thing nobody should have to hunt for. */}
      {phone && more && (
        <>
          <div className="more-bg" onClick={() => setMore(false)} />
          <div className="more-sheet" role="menu">
            {spill.map((l) => (
              <NavLink key={l.to} to={l.to} role="menuitem" className={({ isActive }) => 'more-item' + (isActive ? ' active' : '')}>
                {l.ico}
                <span>{l.label}</span>
              </NavLink>
            ))}
            <button className="more-item danger" role="menuitem" onClick={logout}>
              <Ico>
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                <path d="m16 17 5-5-5-5" />
                <path d="M21 12H9" />
              </Ico>
              <span>Sign out{me?.email ? ` (${me.email})` : ''}</span>
            </button>
          </div>
        </>
      )}

      {/* the rounded workspace pane floating inside the chrome, Slack-style */}
      <div className="pane">
        <div className="content">
          <Outlet />
        </div>
      </div>
    </div>
  )
}

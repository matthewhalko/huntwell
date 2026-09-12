import React, { useEffect, useState } from 'react'
import { ago, api, BrowserConnection, fmtDate } from '../api'
import { useAuth } from '../auth'
import { Field, useToast } from '../components/ui'
import { Theme, useTheme } from '../theme'
import { TeamSection } from '../components/TeamSection'
import { Picker } from '../components/Picker'

/// Every zone this browser knows, for the picker. `supportedValuesOf` is the
/// one-liner where it exists; the fallback is short on purpose, since the zone
/// is already right for almost everyone before they open this.
function zoneList(here: string): string[] {
  let all: string[] = []
  try {
    all = (Intl as any).supportedValuesOf?.('timeZone') || []
  } catch {
    all = []
  }
  if (!all.length) {
    all = [
      'UTC', 'America/New_York', 'America/Chicago', 'America/Denver', 'America/Los_Angeles', 'America/Sao_Paulo',
      'Europe/London', 'Europe/Berlin', 'Europe/Madrid', 'Europe/Warsaw', 'Africa/Lagos', 'Africa/Johannesburg',
      'Asia/Dubai', 'Asia/Kolkata', 'Asia/Singapore', 'Asia/Tokyo', 'Australia/Sydney', 'Pacific/Auckland',
    ]
  }
  // Whatever is set has to be in the list, or the select would show blank.
  return all.includes(here) ? all : [here, ...all]
}

export default function Settings() {
  const { me, refresh } = useAuth()
  const { theme, setTheme } = useTheme()
  const toast = useToast()
  const [name, setName] = useState(me?.display_name || '')
  const [cur, setCur] = useState('')
  const [nw, setNw] = useState('')
  const auto = me?.timezone_auto !== false
  const zone = me?.timezone || 'UTC'
  const zones = React.useMemo(() => zoneList(zone), [zone])
  const save = async () => {
    try {
      await api.put('/api/auth/me', { display_name: name, theme })
      await refresh()
      toast('Saved')
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  /// Picking a zone pins it: the browser stops overriding it from here on.
  const pickZone = async (tz: string) => {
    try {
      await api.put('/api/auth/me', { timezone: tz })
      await refresh()
      toast('Timezone saved')
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const pickTheme = async (t: Theme) => {
    setTheme(t)
    try {
      await api.put('/api/auth/me', { theme: t })
    } catch {}
  }
  const changePw = async () => {
    try {
      await api.post('/api/auth/password', { current: cur, new: nw })
      setCur('')
      setNw('')
      toast('Password changed')
    } catch (e: any) {
      toast(e.message, true)
    }
  }

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Settings</h1>
          <div className="sub">{me?.email}</div>
        </div>
      </div>
      <div className="two">
        <div className="stack">
          <div className="card">
            <h3>Appearance</h3>
            <div className="seg">
              {(['light', 'dark', 'system'] as Theme[]).map((t) => (
                <button key={t} className={theme === t ? 'active' : ''} onClick={() => pickTheme(t)}>
                  {t[0].toUpperCase() + t.slice(1)}
                </button>
              ))}
            </div>
            <p className="muted" style={{ marginTop: '0.6rem', marginBottom: 0 }}>
              Saved to your account, so it follows you to another browser.
            </p>
          </div>
          <div className="card">
            <h3>Profile</h3>
            <Field label="Display name">
              <input type="text" value={name} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field
              label="Timezone"
              hint={
                auto
                  ? 'Taken from this browser. Scheduled searches run at this local time; pick one to set it yourself.'
                  : 'Scheduled searches run at this local time, wherever you sign in from.'
              }
            >
              <Picker value={zone} onChange={pickZone} options={zones.map((z) => ({ value: z, label: z.replace(/_/g, ' ') }))} />
            </Field>
            <button className="btn primary" onClick={save}>
              Save
            </button>
          </div>
          <div className="card">
            <h3>Password</h3>
            <Field label="Current password">
              <input type="password" value={cur} onChange={(e) => setCur(e.target.value)} autoComplete="current-password" />
            </Field>
            <Field label="New password" hint="At least 10 characters.">
              <input type="password" value={nw} onChange={(e) => setNw(e.target.value)} autoComplete="new-password" />
            </Field>
            <button className="btn" disabled={!cur || nw.length < 10} onClick={changePw}>
              Change password
            </button>
          </div>
        </div>
        <div className="stack">
        <TeamSection />
        <ConnectedLogins />
        </div>
      </div>
    </>
  )
}

/// Sites people most often want a signed-in session for. The URL is the site's
/// own login page — we open it, the user signs in there, and nothing about
/// those credentials comes back to us.
///
/// The big social platforms are deliberately NOT here. Anyone may still connect
/// one through "another site", but a named one-click shortcut is Huntwell
/// recommending it, and a shipped recommendation to scrape a platform whose
/// terms forbid it is the evidence that turns a customer's breach into our
/// inducement of it. Suggest the sites whose terms we are not steering people
/// through; leave the rest to the person who accepted them.
const LOGIN_SITES: { site: string; label: string; url: string }[] = [
  { site: 'crunchbase', label: 'Crunchbase', url: 'https://www.crunchbase.com/login' },
  { site: 'glassdoor', label: 'Glassdoor', url: 'https://www.glassdoor.com/profile/login_input.htm' },
  { site: 'indeed', label: 'Indeed', url: 'https://secure.indeed.com/auth' },
  { site: 'reddit', label: 'Reddit', url: 'https://www.reddit.com/login' },
  { site: 'github', label: 'GitHub', url: 'https://github.com/login' },
  { site: 'yelp', label: 'Yelp', url: 'https://www.yelp.com/login' },
  { site: 'zillow', label: 'Zillow', url: 'https://www.zillow.com/user/acct/login/' },
  { site: 'wellfound', label: 'Wellfound', url: 'https://wellfound.com/login' },
]

// Connect a signed-in session to a site once; runs reuse it when they read
// pages on that site.
function ConnectedLogins() {
  const toast = useToast()
  const { me, refresh } = useAuth()
  const [conns, setConns] = useState<BrowserConnection[]>([])
  const [available, setAvailable] = useState(true)
  // Per-workspace switch, separate from whether the server supports the feature
  // at all. Starts true so the section does not flash a "not enabled" notice
  // before the first response arrives.
  const [enabled, setEnabled] = useState(true)
  const [pending, setPending] = useState<{ session_id: string; site: string; url: string } | null>(null)
  const [customUrl, setCustomUrl] = useState('')
  const [showCustom, setShowCustom] = useState(false)

  const load = () =>
    api
      .get<{ connections: BrowserConnection[]; available: boolean; enabled: boolean }>('/api/browser/connections')
      .then((r) => {
        setConns(r.connections || [])
        setAvailable(r.available)
        setEnabled(r.enabled)
      })
      .catch(() => {})
  useEffect(() => {
    load()
  }, [])

  const connectedAt = (site: string) => conns.find((c) => c.site === site)?.connected_at

  const connect = async (site: string, url: string) => {
    try {
      const r = await api.post<{ session_id: string; live_view_url: string }>('/api/browser/login', { site, url })
      window.open(r.live_view_url, '_blank', 'noopener,noreferrer')
      setPending({ session_id: r.session_id, site, url })
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const finish = async () => {
    if (!pending) return
    try {
      await api.post('/api/browser/login/finish', pending)
      setPending(null)
      toast('Session connected')
      load()
    } catch (e: any) {
      toast(e.message, true)
    }
  }
  const connectCustom = () => {
    try {
      const host = new URL(customUrl).hostname.replace(/^www\./, '')
      connect(host, customUrl)
      setCustomUrl('')
      setShowCustom(false)
    } catch {
      toast('Enter a full URL, e.g. https://example.com/login', true)
    }
  }

  // Sites already connected first, so the state of things reads at a glance.
  const sites = [...LOGIN_SITES].sort((a, b) => Number(!!connectedAt(b.site)) - Number(!!connectedAt(a.site)))
  const extra = conns.filter((c) => !LOGIN_SITES.some((s) => s.site === c.site))

  return (
    <div className="card">
      <h3>Connected logins</h3>
      <p className="muted" style={{ marginTop: 0 }}>
        Some pages only exist once you are signed in. Connect an authenticated session to a site and your search plans can
        read those pages too.
      </p>
      <p className="notice" style={{ marginTop: '0.6rem' }}>
        You sign in on the site itself, exactly as you normally would. <b>We never see your password, and no password or
        login is stored on our servers.</b> You can disconnect a site at any time.
      </p>
      {/* The restricted platforms. Off unless the account says it holds the
          right to collect there — recorded with a date, because that is what a
          dispute turns on. */}
      <label className="check ack">
        <input
          type="checkbox"
          checked={!!me?.platform_ack}
          onChange={async (e) => {
            try {
              await api.put('/api/auth/me', { platform_ack: e.target.checked })
              await refresh()
            } catch (err: any) {
              toast(err.message, true)
            }
          }}
        />
        <span>
          I have the right to collect data from LinkedIn, Facebook, Instagram, X and Threads, and I accept responsibility
          for doing so, including any claim brought by those platforms.{' '}
          {me?.platform_ack_at ? (
            <span className="muted">Accepted {fmtDate(me.platform_ack_at)} — applies to everyone in this workspace.</span>
          ) : (
            <span className="muted">Left unticked, plans skip those sites and they cannot be connected.</span>
          )}
        </span>
      </label>

      {!available ? (
        <p className="notice">Connected logins aren't set up on this server yet.</p>
      ) : !enabled ? (
        <p className="notice">Connected logins aren't enabled for this workspace yet. Get in touch and we'll turn them on.</p>
      ) : pending ? (
        <div className="notice" style={{ marginTop: '0.7rem' }}>
          A sign-in window opened in a new tab — sign in to <b style={{ textTransform: 'capitalize' }}>{pending.site}</b>{' '}
          there (password, 2FA and all), then come back and confirm.
          <div className="row" style={{ gap: '0.6rem', marginTop: '0.6rem' }}>
            <button className="btn primary" onClick={finish}>
              I've finished signing in
            </button>
            <button className="btn ghost" onClick={() => setPending(null)}>
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <>
          <div className="site-grid">
            {sites.map((s) => {
              const at = connectedAt(s.site)
              return (
                <button key={s.site} className={'site-tile' + (at ? ' on' : '')} onClick={() => connect(s.site, s.url)}>
                  <span className="nm">{s.label}</span>
                  <span className="st">{at ? `✓ connected ${ago(at)}` : 'Connect'}</span>
                </button>
              )
            })}
          </div>
          {extra.length > 0 && (
            <table style={{ marginTop: '0.8rem' }}>
              <tbody>
                {extra.map((c) => (
                  <tr key={c.site}>
                    <td>
                      <b>{c.site}</b>
                    </td>
                    <td className="num muted">connected {ago(c.connected_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {showCustom ? (
            <div className="row" style={{ gap: '0.6rem', flexWrap: 'nowrap', marginTop: '0.8rem' }}>
              <input
                type="url"
                autoFocus
                placeholder="https://site.com/login"
                value={customUrl}
                onChange={(e) => setCustomUrl(e.target.value)}
                style={{ flex: 1, width: 'auto', minWidth: 0 }}
              />
              <button className="btn" disabled={!/^https?:\/\//.test(customUrl)} onClick={connectCustom}>
                Connect
              </button>
            </div>
          ) : (
            <button className="btn ghost sm" style={{ marginTop: '0.8rem' }} onClick={() => setShowCustom(true)}>
              Another site…
            </button>
          )}
        </>
      )}
    </div>
  )
}

import React from 'react'
import { useLocation } from 'react-router-dom'
import { api, ApiError } from '../api'
import { useAuth } from '../auth'
import { Turnstile, useAuthConfig } from '../pages/Auth'
import { useLoadView } from './Skeleton'
import { SnapshotRegion } from './SnapshotRegion'
import { MAX_SHOTS, canSnapshot, captureTab } from '../supportShots'

/// The support bubble: a chat with Huntwell from any page, public or in the
/// app. Each chat is a thread in the operators' Slack channel, and they
/// answer there or in admin → Support. Server side: `cmd/src/web/support.rs`.
///
/// As in Alpha Tech's terminal, a person has one chat at a time: they can end
/// it, start a new one, and read their earlier ones. Signed in, the session
/// is the way in. A visitor gets a token per chat, kept in this browser (the
/// earlier ones too, for the list); a reply email links back with it after
/// `#support=`, which never reaches a server.

type Msg = { id: number; from: 'you' | 'support'; body: string; images?: number[]; at: string }
type Conversation = { id: number; name: string; email: string; status: 'open' | 'closed' | 'ended'; unread: number; messages: Msg[] }
type Summary = { conversation_id: number; status: string; last_message_at: string; title: string; unread: number; token?: string | null }

const TOKEN_KEY = 'huntwell.support'
/// A visitor's earlier chats' tokens, newest first.
const PAST_KEY = 'huntwell.support.past'
const PAST_MAX = 20
const BODY_MAX = 4000
const POLL_OPEN_MS = 4000
const POLL_CLOSED_MS = 30000
/// Shown at the foot of the panel for anyone who would rather write.
const SUPPORT_EMAIL = 'support@huntwell.ai'

function readToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) || ''
  } catch {
    return ''
  }
}
function writeToken(t: string) {
  try {
    if (t) localStorage.setItem(TOKEN_KEY, t)
    else localStorage.removeItem(TOKEN_KEY)
  } catch {
    // Private mode: the chat still works on this page, and the reply email
    // still links back to it.
  }
}

function readPast(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(PAST_KEY) || '[]')
    return Array.isArray(v) ? v.filter((t): t is string => typeof t === 'string' && t.startsWith('hwc_')) : []
  } catch {
    return []
  }
}
function writePast(tokens: string[]) {
  try {
    localStorage.setItem(PAST_KEY, JSON.stringify([...new Set(tokens)].slice(0, PAST_MAX)))
  } catch {
    // Private mode: the list of earlier chats lasts as long as the page.
  }
}

/// Screenshots already fetched, by id: an image never changes once sent.
const shotUrls = new Map<number, string>()

/// A screenshot in the chat. Fetched with the chat's own way in (a POST with
/// the visitor's token, never a URL) and shown as a blob.
function Shot({ id, token }: { id: number; token: string }) {
  const [url, setUrl] = React.useState(() => shotUrls.get(id) ?? '')
  React.useEffect(() => {
    if (url) return
    let live = true
    fetch('/api/support/image', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify({ id, token }),
    })
      .then((r) => (r.ok ? r.blob() : Promise.reject(r.status)))
      .then((b) => {
        const u = URL.createObjectURL(b)
        shotUrls.set(id, u)
        if (live) setUrl(u)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [id, token, url])
  if (!url) return <span className="support-shot support-shot-wait skel" aria-label="Loading screenshot" />
  return (
    <a href={url} target="_blank" rel="noreferrer" title="Open full size">
      <img className="support-shot" src={url} alt="Screenshot" />
    </a>
  )
}

function when(iso: string) {
  const d = new Date(iso)
  const today = new Date().toDateString() === d.toDateString()
  return today ? d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }) : d.toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })
}

export default function SupportChat() {
  const { me } = useAuth()
  const loc = useLocation()
  const cfg = useAuthConfig()
  // Signed in and verified: the session speaks for them. Anyone else,
  // including someone stuck before verifying, writes as a visitor.
  const signed = !!me && me.email_verified
  const [token, setToken] = React.useState(readToken)
  // Signed in: an earlier chat on screen, by id; null for the current one.
  const [viewId, setViewId] = React.useState<number | null>(null)
  const [pane, setPane] = React.useState<'chat' | 'history'>('chat')
  // An earlier chat opened from the list: the back button returns to it.
  const [fromHistory, setFromHistory] = React.useState(false)
  // Screenshots waiting to be sent, and a captured frame waiting for an area
  // to be picked from it (the panel steps out of the picture meanwhile).
  const [shots, setShots] = React.useState<string[]>([])
  const [frame, setFrame] = React.useState<HTMLCanvasElement | null>(null)
  const [snapping, setSnapping] = React.useState(false)
  const [chats, setChats] = React.useState<Summary[] | null>(null)
  const [open, setOpen] = React.useState(false)
  const [conv, setConv] = React.useState<Conversation | null>(null)
  const [loaded, setLoaded] = React.useState(false)
  const [draft, setDraft] = React.useState('')
  const [email, setEmail] = React.useState('')
  const [name, setName] = React.useState('')
  const [challenge, setChallenge] = React.useState('')
  const [resetKey, setResetKey] = React.useState(0)
  const [sending, setSending] = React.useState(false)
  const [error, setError] = React.useState('')
  const list = React.useRef<HTMLDivElement>(null)
  const input = React.useRef<HTMLTextAreaElement>(null)
  const view = useLoadView(loaded)

  // A reply email's link: `#support=hwc_…` for a visitor, `#support` in the
  // app. Taken once, then cleared from the address bar.
  React.useEffect(() => {
    const h = window.location.hash
    if (!h.startsWith('#support')) return
    const t = h.startsWith('#support=') ? decodeURIComponent(h.slice('#support='.length)) : ''
    if (t.startsWith('hwc_')) {
      writeToken(t)
      setToken(t)
    }
    setOpen(true)
    history.replaceState(null, '', window.location.pathname + window.location.search)
  }, [])

  const fetchChat = React.useCallback(
    async (seen: boolean) => {
      try {
        if (signed) {
          const q = new URLSearchParams()
          if (viewId !== null) q.set('id', String(viewId))
          if (seen) q.set('open', '1')
          const r = await api.get<{ conversation: Conversation | null }>(`/api/support/mine${q.toString() ? '?' + q.toString() : ''}`)
          setConv(r.conversation)
        } else if (token) {
          const r = await api.post<{ conversation: Conversation }>('/api/support/thread', { token, open: seen })
          setConv(r.conversation)
        } else {
          setConv(null)
        }
      } catch (e) {
        // A visitor's chat that is gone (or a token from elsewhere) is
        // forgotten, so the next message starts a fresh one.
        if (e instanceof ApiError && e.status === 404) {
          if (signed) {
            setViewId(null)
          } else {
            writeToken('')
            setToken('')
          }
          setConv(null)
        }
      } finally {
        setLoaded(true)
      }
    },
    [signed, token, viewId],
  )

  // Who is asking changed (signed in or out): start from what they have.
  React.useEffect(() => {
    setConv(null)
    setLoaded(false)
    if (signed || token) fetchChat(false)
    else setLoaded(true)
  }, [signed, token, fetchChat])

  // Open: keep up with replies, and mark them read. Closed: check now and
  // then for a reply, so the bubble can show it is waiting.
  React.useEffect(() => {
    if (!signed && !token) return
    if (!open && !conv) return
    if (open) fetchChat(true)
    const t = setInterval(() => document.visibilityState === 'visible' && fetchChat(open), open ? POLL_OPEN_MS : POLL_CLOSED_MS)
    return () => clearInterval(t)
  }, [open, signed, token, !!conv, fetchChat])

  const count = conv?.messages.length ?? 0
  // To the newest message: on opening, on each new one, and once the first
  // load is on screen (a skeleton may have stood in for it).
  React.useEffect(() => {
    if (open && list.current) list.current.scrollTop = list.current.scrollHeight
  }, [open, count, view])
  React.useEffect(() => {
    if (open) setTimeout(() => input.current?.focus(), 60)
  }, [open, !!conv])

  // The extension's sign-in runs in a small popup that is all consent: no bubble.
  if (loc.pathname.startsWith('/app/extension/')) return null

  // A visitor's first step: their name, their email and the bot check. Only
  // then does the chat open for them to write in.
  const introducing = !signed && !conv
  const needsChallenge = introducing && !!cfg?.turnstile_site_key
  const chatOff = introducing && cfg !== null && cfg.support_chat === false
  const canBegin = !sending && name.trim().length > 0 && /^\S+@\S+\.\S+$/.test(email.trim()) && (!needsChallenge || !!challenge)
  const ended = conv?.status === 'ended'
  const canSend = (draft.trim().length > 0 || shots.length > 0) && !sending && !ended && (signed || !!conv)

  async function begin() {
    if (!canBegin) return
    setSending(true)
    setError('')
    try {
      const r = await api.post<{ token: string; conversation: Conversation }>('/api/support/start', {
        name: name.trim(),
        email: email.trim(),
        page: loc.pathname,
        turnstile_token: challenge,
      })
      writeToken(r.token)
      setToken(r.token)
      setConv(r.conversation)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      // A Turnstile token is single-use: ask for a fresh one.
      setChallenge('')
      setResetKey((k) => k + 1)
    } finally {
      setSending(false)
    }
  }

  async function send() {
    if (!canSend) return
    setSending(true)
    setError('')
    const body = draft.trim()
    try {
      if (signed) {
        const r = await api.post<{ conversation: Conversation }>('/api/support/mine', { body, images: shots, page: loc.pathname, id: conv?.id })
        setConv(r.conversation)
        setViewId(null)
      } else if (conv && token) {
        const r = await api.post<{ conversation: Conversation }>('/api/support/send', { token, body, images: shots })
        setConv(r.conversation)
      }
      setDraft('')
      setShots([])
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSending(false)
    }
  }

  /// Take a screenshot: capture this tab with the panel out of the way, then
  /// let them drag out the area to send.
  async function snapshot() {
    setError('')
    if (!canSnapshot()) {
      // Phones have no screen capture, and browsers allow it only on https
      // (or localhost): say why rather than hiding the button.
      setError(window.isSecureContext ? "This browser can't take screenshots." : 'Screenshots need a secure (https) connection.')
      return
    }
    setSnapping(true)
    try {
      setFrame(await captureTab())
    } catch (e) {
      setSnapping(false)
      // Cancelling the browser's share prompt is not an error worth showing.
      if (!(e instanceof DOMException && e.name === 'NotAllowedError')) setError('The screenshot could not be taken.')
    }
  }

  const picked = React.useCallback((image: string | null) => {
    setFrame(null)
    setSnapping(false)
    if (image) setShots((cur) => [...cur, image].slice(0, MAX_SHOTS))
    setTimeout(() => input.current?.focus(), 60)
  }, [])

  /// End the chat on screen. It stays on screen, read-only, with a way to
  /// start another.
  async function endChat() {
    if (!conv || ended) return
    setError('')
    try {
      if (signed) {
        await api.post('/api/support/mine/end', { id: conv.id })
        // No longer the current chat: keep it on screen by its id (which loads it).
        if (viewId === conv.id) await fetchChat(true)
        else setViewId(conv.id)
      } else {
        await api.post('/api/support/end', { token })
        await fetchChat(true)
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  /// A fresh chat: the one on screen is ended first if it is still going.
  /// A visitor's goes to their earlier chats, and they introduce themselves
  /// again (with their name and email filled in).
  async function startNew() {
    setError('')
    setPane('chat')
    setFromHistory(false)
    setShots([])
    try {
      if (conv && !ended && conv.messages.length > 0) {
        if (signed) await api.post('/api/support/mine/end', { id: conv.id })
        else await api.post('/api/support/end', { token })
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    }
    if (signed) {
      setViewId(null)
    } else {
      if (token) writePast([token, ...readPast()])
      if (conv) {
        setName(conv.name)
        setEmail(conv.email)
      }
      writeToken('')
      setToken('')
      setChallenge('')
      setResetKey((k) => k + 1)
    }
    setConv(null)
    setDraft('')
  }

  async function showHistory() {
    setPane('history')
    setFromHistory(false)
    setChats(null)
    try {
      if (signed) {
        setChats((await api.get<{ chats: Summary[] }>('/api/support/mine/history')).chats)
      } else {
        const tokens = [token, ...readPast()].filter(Boolean)
        setChats(tokens.length ? (await api.post<{ chats: Summary[] }>('/api/support/history', { tokens })).chats : [])
      }
    } catch (e) {
      setChats([])
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  function openChat(c: Summary) {
    setPane('chat')
    setFromHistory(true)
    setShots([])
    setError('')
    if (signed) {
      setViewId(c.conversation_id)
    } else if (c.token && c.token !== token) {
      writePast([token, ...readPast()].filter((t) => t && t !== c.token))
      writeToken(c.token)
      setToken(c.token)
    }
  }

  const unread = !open && conv ? conv.unread : 0
  const canEnd = !!conv && !ended && conv.messages.length > 0 && pane === 'chat'
  const hasHistory = signed || !!token || readPast().length > 0
  const inApp = loc.pathname.startsWith('/app')
  const firstName = (signed ? me?.display_name : conv?.name)?.trim().split(/\s+/)[0]

  return (
    <div className={'support' + (inApp ? ' in-app' : '')}>
      {frame && <SnapshotRegion frame={frame} onDone={picked} />}
      {open && (
        <section className={'support-panel' + (snapping ? ' snapping' : '')} role="dialog" aria-label="Chat with Huntwell support">
          <header className="support-head">
            <div>
              <div className="support-title">
                <span className="grad-text" aria-hidden>
                  ✦
                </span>{' '}
                Huntwell support
              </div>
            </div>
            <div className="support-acts">
              {canEnd && (
                <button type="button" className="support-act" onClick={endChat} title="End this chat">
                  End
                </button>
              )}
              {pane === 'history' || fromHistory ? (
                <button
                  type="button"
                  className="support-act"
                  onClick={() => (pane === 'history' ? setPane('chat') : showHistory())}
                  title={pane === 'history' ? 'Back to the chat' : 'Back to earlier chats'}
                  aria-label="Back"
                >
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                    <path d="M19 12H5" />
                    <path d="m12 19-7-7 7-7" />
                  </svg>
                  Back
                </button>
              ) : (
                hasHistory && (
                  <button type="button" className="support-act" onClick={showHistory} title="Earlier chats" aria-label="Earlier chats">
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
                      <path d="M3 3v5h5" />
                      <path d="M12 7v5l3 2" />
                    </svg>
                  </button>
                )
              )}
              {(signed || !!conv) && (
                <button type="button" className="support-act" onClick={startNew} title="New chat" aria-label="New chat">
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                    <path d="M12 20h9" />
                    <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" />
                  </svg>
                </button>
              )}
              <button type="button" className="support-x" onClick={() => setOpen(false)} aria-label="Close chat">
                ×
              </button>
            </div>
          </header>

          <div className="support-list" ref={list}>
            {view === 'skeleton' ? (
              <div className="support-skel" aria-busy="true" aria-label="Loading">
                <span className="skel" style={{ width: '70%' }} />
                <span className="skel" style={{ width: '45%', alignSelf: 'flex-end' }} />
                <span className="skel" style={{ width: '60%' }} />
              </div>
            ) : view === 'wait' ? null : pane === 'history' ? (
              chats === null ? (
                <div className="support-skel" aria-busy="true" aria-label="Loading">
                  <span className="skel" />
                  <span className="skel" />
                </div>
              ) : chats.length === 0 ? (
                <div className="support-note">No earlier chats yet.</div>
              ) : (
                <ul className="support-chats">
                  {chats.map((c) => (
                    <li key={c.conversation_id}>
                      <button type="button" className={'support-chat' + (c.conversation_id === conv?.id ? ' on' : '')} onClick={() => openChat(c)}>
                        <span className="support-chat-title">{c.title}</span>
                        <span className="support-chat-meta">
                          {when(c.last_message_at)} · {c.status === 'ended' ? 'Ended' : 'Open'}
                          {c.unread > 0 && <span className="support-chat-new">{c.unread} new</span>}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )
            ) : chatOff ? (
              <div className="support-msg support-from">Chat isn't available right now. Please try again later.</div>
            ) : introducing ? (
              <div className="support-intro">
                <div className="support-msg support-from">Hi! Before we start, what's your name?</div>
                <form
                  className="support-begin"
                  onSubmit={(e) => {
                    e.preventDefault()
                    begin()
                  }}
                >
                  <label>
                    <span>Your name</span>
                    <input type="text" value={name} maxLength={80} onChange={(e) => setName(e.target.value)} autoComplete="name" autoFocus required />
                  </label>
                  <label>
                    <span>Your email, so we can reply</span>
                    <input type="email" value={email} maxLength={320} onChange={(e) => setEmail(e.target.value)} autoComplete="email" required />
                  </label>
                  {needsChallenge && cfg?.turnstile_site_key && (
                    <div className="support-challenge">
                      <Turnstile siteKey={cfg.turnstile_site_key} onToken={setChallenge} resetKey={resetKey} />
                    </div>
                  )}
                  {error && (
                    <div className="support-error" role="alert">
                      {error}
                    </div>
                  )}
                  <button type="submit" className="btn primary" disabled={!canBegin}>
                    {sending ? 'Starting…' : 'Start chat'}
                  </button>
                </form>
              </div>
            ) : (
              <>
                <div className="support-msg support-from">
                  {firstName ? `Hi ${firstName}! ` : 'Hi! '}What can we help you with? Questions about Huntwell, your searches or your account are all welcome.
                </div>
                {conv?.messages.map((m) => (
                  <div key={m.id} className={'support-msg ' + (m.from === 'you' ? 'support-mine' : 'support-from')}>
                    {m.images?.map((id) => <Shot key={id} id={id} token={signed ? '' : token} />)}
                    {m.body}
                    <span className="support-when">{when(m.at)}</span>
                  </div>
                ))}
                {conv && !ended && conv.messages[conv.messages.length - 1]?.from === 'you' && (
                  <div className="support-human" role="status">
                    <span className="support-human-ico" aria-hidden>
                      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <circle cx="12" cy="8" r="4" />
                        <path d="M4 21a8 8 0 0 1 16 0" />
                      </svg>
                    </span>
                    <span>
                      <b>A real person is on it.</b> Hold tight, someone from the Huntwell team will answer you right here{signed ? '' : ' and by email'}.
                    </span>
                  </div>
                )}
                {conv?.status === 'closed' && <div className="support-note">This chat was closed. Write again any time and it opens back up.</div>}
              </>
            )}
          </div>

          {!introducing && pane === 'chat' && ended && (
            <div className="support-ended">
              <span>This chat has ended.</span>
              <button type="button" className="btn primary sm" onClick={startNew}>
                Start a new chat
              </button>
            </div>
          )}
          {!introducing && pane === 'chat' && !ended && (
            <form
              className="support-compose"
              onSubmit={(e) => {
                e.preventDefault()
                send()
              }}
            >
              {error && (
                <div className="support-error" role="alert">
                  {error}
                </div>
              )}
              {shots.length > 0 && (
                <div className="support-shots">
                  {shots.map((src, i) => (
                    <span key={i} className="support-shot-pending">
                      <img src={src} alt={`Screenshot ${i + 1} to send`} />
                      <button type="button" aria-label="Remove screenshot" title="Remove" onClick={() => setShots((cur) => cur.filter((_, j) => j !== i))}>
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              )}
              <div className="support-row">
                <button
                  type="button"
                  className="support-snap"
                  onClick={snapshot}
                  disabled={sending || shots.length >= MAX_SHOTS || !!frame}
                  title="Take a screenshot of this page to send"
                  aria-label="Take a screenshot"
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                    <path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z" />
                    <circle cx="12" cy="13" r="3" />
                  </svg>
                </button>
                <textarea
                  ref={input}
                  rows={2}
                  placeholder="Write a message…"
                  value={draft}
                  maxLength={BODY_MAX}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                      e.preventDefault()
                      send()
                    }
                  }}
                  aria-label="Message"
                />
                <button type="submit" className="btn primary support-send" disabled={!canSend} aria-label="Send">
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                    <path d="M22 2 11 13" />
                    <path d="M22 2 15 22l-4-9-9-4z" />
                  </svg>
                </button>
              </div>
            </form>
          )}
          <a className="support-mail" href={`mailto:${SUPPORT_EMAIL}`}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <rect x="3" y="5" width="18" height="14" rx="2" />
              <path d="m3 7 9 6 9-6" />
            </svg>
            <span>
              Prefer email? <b>{SUPPORT_EMAIL}</b>
            </span>
          </a>
        </section>
      )}

      <button
        type="button"
        className={'support-bubble' + (open ? ' on' : '')}
        onClick={() => setOpen((o) => !o)}
        aria-label={open ? 'Close support chat' : unread ? `Support chat, ${unread} new ${unread === 1 ? 'reply' : 'replies'}` : 'Chat with support'}
        aria-expanded={open}
      >
        {open ? (
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden>
            <path d="M6 9l6 6 6-6" />
          </svg>
        ) : (
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
        )}
        {!open && <span className="support-label">Help</span>}
        {unread > 0 && <span className="support-dot">{unread > 9 ? '9+' : unread}</span>}
      </button>
    </div>
  )
}

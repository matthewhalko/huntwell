// Huntwell for Chrome — the side panel: sign in, see the queue, open one.

import { api, clearToken, getAuth, normaliseServer, setAuth, SignedOut, DEFAULT_SERVER } from './lib.js'

const $ = (id) => document.getElementById(id)
let items = []
let activeId = null

// ---- sign-in: OAuth authorization code + PKCE through launchWebAuthFlow ----------

function b64url(bytes) {
  return btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function randomString(n) {
  return b64url(crypto.getRandomValues(new Uint8Array(n)))
}

async function sha256(text) {
  return crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
}

/// May the extension talk to this server? Huntwell's own address and this
/// machine are in the manifest; anything else is asked for, once.
async function reach(server) {
  const origins = [server + '/*']
  if (await chrome.permissions.contains({ origins })) return true
  return chrome.permissions.request({ origins })
}

async function signIn() {
  const err = $('signin-error')
  err.hidden = true
  const server = normaliseServer($('server').value)
  if (!server) {
    err.textContent = 'Enter your Huntwell address, e.g. https://huntwell.ai'
    err.hidden = false
    return
  }
  const btn = $('signin-btn')
  btn.disabled = true
  try {
    if (!(await reach(server))) throw new Error('Huntwell for Chrome needs permission to reach ' + server)
    const verifier = randomString(48)
    const challenge = b64url(await sha256(verifier))
    const state = randomString(16)
    const redirectUri = chrome.identity.getRedirectURL('cb')
    const q = new URLSearchParams({ redirect_uri: redirectUri, code_challenge: challenge, code_challenge_method: 'S256', state })
    const back = await chrome.identity.launchWebAuthFlow({ url: `${server}/app/extension/authorize?${q}`, interactive: true })
    const got = new URL(back)
    if (got.searchParams.get('state') !== state) throw new Error('That sign-in did not match — try again')
    const code = got.searchParams.get('code')
    if (!code) throw new Error(got.searchParams.get('error') || 'Sign-in was cancelled')
    const res = await fetch(server + '/ext/v1/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code, code_verifier: verifier, redirect_uri: redirectUri }),
    })
    const data = await res.json().catch(() => ({}))
    if (!res.ok || !data.access_token) throw new Error(data.error || 'Huntwell did not sign you in — try again')
    await setAuth(server, data.access_token)
    await show()
  } catch (e) {
    // Closing the sign-in window is not an error worth shouting about.
    const msg = String(e.message || e)
    if (!/did not approve|closed|cancel/i.test(msg)) {
      err.textContent = msg
      err.hidden = false
    }
  } finally {
    btn.disabled = false
  }
}

async function signOut() {
  try {
    await api('/signout', { method: 'POST' })
  } catch {}
  await clearToken()
  await show()
}

// ---- the queue ------------------------------------------------------------------

function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag)
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') e.className = v
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v)
    else e.setAttribute(k, v)
  }
  for (const k of kids.flat()) if (k != null && k !== false) e.append(k)
  return e
}

function card(item) {
  const chan = item.channel === 'reddit' ? 'Reddit' : 'LinkedIn'
  const who = item.name || (item.reddit ? 'u/' + item.reddit : '(no name)')
  const sub = [item.title, item.company].filter(Boolean).join(' · ') || (item.channel === 'reddit' && item.name ? 'u/' + item.reddit : '')
  const note =
    item.channel === 'linkedin' && !item.linkedin
      ? 'No profile link — this opens a LinkedIn search. Open the right person, then use “Fill message” on the page.'
      : null
  const busy = (btn, fn) => async () => {
    btn.disabled = true
    try {
      await fn()
    } catch (e) {
      fail(e)
    } finally {
      btn.disabled = false
    }
  }
  const openBtn = el('button', { class: 'btn primary sm' }, `Open & fill on ${chan}`)
  openBtn.addEventListener('click', busy(openBtn, async () => {
    activeId = item.id
    render()
    const r = await chrome.runtime.sendMessage({ type: 'open', item })
    if (r?.error) throw new Error(r.error)
  }))
  const sentBtn = el('button', { class: 'btn sm', title: 'Already sent it? Record it.' }, 'Mark sent')
  sentBtn.addEventListener('click', busy(sentBtn, async () => {
    await api(`/outreach/${item.id}/sent`, { method: 'POST', body: { via: item.channel } })
    await load()
  }))
  const skipBtn = el('button', { class: 'btn ghost sm', title: 'Take it off the queue' }, 'Skip')
  skipBtn.addEventListener('click', busy(skipBtn, async () => {
    await api(`/outreach/${item.id}/unqueue`, { method: 'POST' })
    await load()
  }))
  return el(
    'div',
    { class: 'item' + (item.id === activeId ? ' active' : '') },
    el('div', { class: 'item-head' }, el('div', {}, el('div', { class: 'item-name' }, who), sub && el('div', { class: 'item-sub' }, sub)), el('span', { class: 'chan ' + item.channel }, chan)),
    item.subject && el('div', { class: 'item-subject' }, item.subject),
    el('div', { class: 'item-preview' }, item.message),
    note && el('div', { class: 'item-note' }, note),
    el('div', { class: 'item-actions' }, openBtn, sentBtn, skipBtn),
  )
}

function render() {
  $('list').replaceChildren(...items.map(card))
  $('empty').hidden = items.length > 0
}

function fail(e) {
  if (e instanceof SignedOut) return show()
  $('app-error').textContent = String(e.message || e)
  $('app-error').hidden = false
}

async function load() {
  $('app-error').hidden = true
  try {
    const r = await api('/queue')
    items = r.items || []
    render()
  } catch (e) {
    fail(e)
  }
}

async function show() {
  const { server, token } = await getAuth()
  $('server').value = server || DEFAULT_SERVER
  $('signin').hidden = !!token
  $('app').hidden = !token
  if (!token) return
  try {
    const me = await api('/me')
    $('who').textContent = me.name || me.email
    $('where').textContent = [me.workspace, me.name ? me.email : ''].filter(Boolean).join(' · ')
    $('account').hidden = false
  } catch (e) {
    if (e instanceof SignedOut) return show()
  }
  await load()
}

$('signin-btn').addEventListener('click', signIn)
$('server').addEventListener('keydown', (e) => e.key === 'Enter' && signIn())
$('signout').addEventListener('click', signOut)
$('refresh').addEventListener('click', load)
$('open-outreach').addEventListener('click', async () => {
  const { server } = await getAuth()
  chrome.tabs.create({ url: server + '/app/outreach' })
})
// The wordmark goes home, as it does in the app.
$('home').addEventListener('click', async (e) => {
  e.preventDefault()
  const { server } = await getAuth()
  chrome.tabs.create({ url: server + '/app' })
})
chrome.runtime.onMessage.addListener((msg) => {
  if (msg?.type === 'queue-changed') load()
})
// Drafts get queued in the app while the panel is open; keep up.
setInterval(() => !$('app').hidden && document.visibilityState === 'visible' && load(), 30_000)
document.addEventListener('visibilitychange', () => document.visibilityState === 'visible' && !$('app').hidden && load())
show()

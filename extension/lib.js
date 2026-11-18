// Shared by the side panel and the background worker: where Huntwell is, the
// extension's key, and calls to /ext/v1 with it. The key lives in
// chrome.storage.local, which only this extension can read.

export const DEFAULT_SERVER = 'https://huntwell.ai'

export async function getAuth() {
  const { server, token } = await chrome.storage.local.get(['server', 'token'])
  return { server: server || DEFAULT_SERVER, token: token || '' }
}

export async function setAuth(server, token) {
  await chrome.storage.local.set({ server, token })
}

export async function clearToken() {
  await chrome.storage.local.remove('token')
}

/// "huntwell.example.com/" → "https://huntwell.example.com"; null when it is not
/// an address. http only for this machine (a dev server).
export function normaliseServer(raw) {
  let s = (raw || '').trim()
  if (!s) return null
  if (!/^https?:\/\//i.test(s)) s = 'https://' + s
  let u
  try {
    u = new URL(s)
  } catch {
    return null
  }
  const local = u.hostname === 'localhost' || u.hostname === '127.0.0.1'
  if (u.protocol !== 'https:' && !local) return null
  return u.origin
}

export class SignedOut extends Error {}

/// A call to the extension API. A 401 means the key was revoked or expired:
/// it is forgotten, and the caller shows the sign-in again.
export async function api(path, { method = 'GET', body } = {}) {
  const { server, token } = await getAuth()
  if (!token) throw new SignedOut('Sign in to Huntwell')
  const res = await fetch(server + '/ext/v1' + path, {
    method,
    headers: { Authorization: 'Bearer ' + token, ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  })
  let data = {}
  try {
    data = await res.json()
  } catch {}
  if (res.status === 401) {
    await clearToken()
    throw new SignedOut(data.error || 'Signed out — sign in to Huntwell again')
  }
  if (!res.ok) throw new Error(data.error || `Huntwell answered ${res.status}`)
  return data
}

/// Where a queued item opens: the person's LinkedIn profile (or a people
/// search when there is none), or Reddit's compose page addressed to them.
export function targetUrl(item) {
  if (item.channel === 'reddit') {
    const q = new URLSearchParams({ to: item.reddit, subject: item.subject || '' })
    return `https://www.reddit.com/message/compose/?${q}`
  }
  if (item.linkedin) return item.linkedin
  const words = [item.name, item.company].filter(Boolean).join(' ')
  return `https://www.linkedin.com/search/results/people/?keywords=${encodeURIComponent(words)}`
}

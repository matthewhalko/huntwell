// Huntwell for Chrome — the background worker.
//
// It keeps one thing: which queued message belongs to which tab ("jobs", in
// session storage so a sleeping worker does not lose them). The panel opens a
// tab for an item; the content script on that tab asks what to fill; when the
// person presses Send, the content script says so and this records it in
// Huntwell. Nothing here clicks Send, or ever sends anything on a site.

import { api, targetUrl } from './lib.js'

chrome.runtime.onInstalled.addListener(() => {
  chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {})
})
chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {})

async function jobs() {
  return (await chrome.storage.session.get('jobs')).jobs || {}
}

async function setJob(tabId, item) {
  const all = await jobs()
  if (item) all[tabId] = item
  else delete all[tabId]
  await chrome.storage.session.set({ jobs: all })
}

chrome.tabs.onRemoved.addListener((tabId) => {
  setJob(tabId, null)
})

/// Tell any open panel the queue changed.
function changed() {
  chrome.runtime.sendMessage({ type: 'queue-changed' }).catch(() => {})
}

async function open(item) {
  const url = targetUrl(item)
  const tab = await chrome.tabs.create({ url, active: true })
  await setJob(tab.id, item)
  return { ok: true }
}

async function handle(msg, sender) {
  const tabId = sender.tab?.id
  switch (msg.type) {
    case 'open':
      return open(msg.item)
    // A content script asking what, if anything, it should fill on its tab.
    case 'job':
      return { item: tabId != null ? (await jobs())[tabId] || null : null }
    case 'sent': {
      await api(`/outreach/${Number(msg.id)}/sent`, { method: 'POST', body: { via: msg.via } })
      if (tabId != null) await setJob(tabId, null)
      changed()
      return { ok: true }
    }
    case 'skip': {
      await api(`/outreach/${Number(msg.id)}/unqueue`, { method: 'POST' })
      if (tabId != null) await setJob(tabId, null)
      changed()
      return { ok: true }
    }
    // The banner was closed: leave the item queued, forget the tab.
    case 'done':
      if (tabId != null) await setJob(tabId, null)
      return { ok: true }
    default:
      return { error: 'unknown message' }
  }
}

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  // Only this extension's own pages and content scripts.
  if (sender.id !== chrome.runtime.id) return false
  handle(msg, sender)
    .then(reply)
    .catch((e) => reply({ error: e.message || String(e) }))
  return true
})

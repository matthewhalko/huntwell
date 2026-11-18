// Huntwell for Chrome — helpers shared by the LinkedIn and Reddit scripts.
//
// The rule every script follows: fill the message in, point at Send, and let
// the person press it. Nothing here clicks a Send button, submits a form, or
// presses Enter on anyone's behalf.

;(() => {
  if (window.HW) return

  const ask = (msg) =>
    new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(msg, (r) => resolve(chrome.runtime.lastError ? { error: chrome.runtime.lastError.message } : r || {}))
      } catch (e) {
        // The extension was reloaded under this page.
        resolve({ error: String(e) })
      }
    })

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

  /// Poll for something the page renders late (LinkedIn and Reddit are SPAs).
  async function waitFor(fn, timeout = 10000, every = 250) {
    const end = Date.now() + timeout
    while (Date.now() < end) {
      const v = fn()
      if (v) return v
      await sleep(every)
    }
    return null
  }

  const visible = (el) => !!el && !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length)

  /// The text a composer holds now, for "is it empty" and "did it fill".
  const textOf = (el) => (el.value !== undefined && el.tagName !== 'DIV' ? el.value : el.innerText || '').replace(/\s+/g, ' ').trim()

  /// Fill a <textarea>/<input> the way a framework notices: the native setter,
  /// then an input event.
  function fillField(el, text) {
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
    const set = Object.getOwnPropertyDescriptor(proto, 'value').set
    el.focus()
    set.call(el, text)
    el.dispatchEvent(new Event('input', { bubbles: true }))
    el.dispatchEvent(new Event('change', { bubbles: true }))
  }

  /// Fill a rich editor (contenteditable). A paste event first — editors take
  /// pasted text with its line breaks intact — then typing line by line.
  async function fillEditable(el, text) {
    el.focus()
    const dt = new DataTransfer()
    dt.setData('text/plain', text)
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
    await sleep(150)
    const want = text.replace(/\s+/g, ' ').trim()
    if (textOf(el).includes(want.slice(0, 40))) return true
    el.focus()
    const lines = text.split('\n')
    lines.forEach((line, i) => {
      if (line) document.execCommand('insertText', false, line)
      if (i < lines.length - 1) document.execCommand('insertLineBreak')
    })
    await sleep(100)
    return textOf(el).includes(want.slice(0, 40))
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      return false
    }
  }

  /// Point at the button the person should press.
  function pointAt(el) {
    if (!el) return
    // Huntwell's primary green (--accent).
    el.style.outline = '3px solid #007a5a'
    el.style.outlineOffset = '2px'
    el.style.borderRadius = el.style.borderRadius || '16px'
    el.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }

  // ---- the banner ------------------------------------------------------------------
  // In a shadow root so the site's CSS cannot restyle it and ours cannot leak.
  // Huntwell's look: the aubergine header with the ✦ wordmark, Lato, and the
  // app's tokens (UI/web/src/styles.css). A @font-face inside a shadow root is
  // ignored, so the bundled Lato is declared on the page once, under a name
  // no site uses.

  function loadFont() {
    if (document.getElementById('huntwell-font')) return
    const url = (w) => chrome.runtime.getURL(`fonts/lato-${w}.woff2`)
    const style = document.createElement('style')
    style.id = 'huntwell-font'
    style.textContent = [400, 700, 900]
      .map((w) => `@font-face{font-family:'Huntwell Lato';font-weight:${w};font-display:swap;src:url(${url(w)}) format('woff2')}`)
      .join('')
    document.head?.append(style)
  }

  let host = null
  function banner({ title, text, actions = [], tone = 'info' }) {
    if (!host) {
      try {
        loadFont()
      } catch {
        // The extension was reloaded under this page; the system font will do.
      }
      host = document.createElement('div')
      host.id = 'huntwell-banner'
      host.style.cssText = 'position:fixed;top:72px;right:16px;z-index:2147483647;'
      host.attachShadow({ mode: 'open' })
      document.documentElement.append(host)
    }
    const root = host.shadowRoot
    root.innerHTML = `<style>
      :host{--chrome:#3f0e40;--accent:#007a5a;--accent-deep:#00604a;--surface:#fff;--border:#e0e0e0;--border-strong:#c5c5c5;--text:#1d1c1d;--text-2:#616061;
        --selected:#1264a3;--warn:#e8a317;--grad:linear-gradient(120deg,#36c5f0,#2eb67d 38%,#ecb22e 66%,#e01e5a);
        --shadow:0 1px 2px rgba(29,28,29,.06),0 12px 32px -10px rgba(29,28,29,.35)}
      @media (prefers-color-scheme:dark){:host{--chrome:#0b0b0d;--surface:#1a1a1d;--border:#2c2c30;--border-strong:#45454b;--text:#d1d2d3;--text-2:#ababad;
        --selected:#1d9bd1;--shadow:0 1px 2px rgba(0,0,0,.4),0 12px 32px -10px rgba(0,0,0,.8)}}
      .b{font:14px/1.45 'Huntwell Lato',-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;-webkit-font-smoothing:antialiased;width:310px;
        background:var(--surface);color:var(--text);border:1px solid var(--border);border-radius:10px;box-shadow:var(--shadow);overflow:hidden}
      .top{display:flex;align-items:center;gap:6px;background:var(--chrome);color:#fff;padding:7px 8px 7px 12px;border-bottom:3px solid var(--tone)}
      .brand{display:flex;align-items:center;gap:.25rem;font-weight:900;font-size:15px;letter-spacing:-.02em;user-select:none}
      .s{background:var(--grad);-webkit-background-clip:text;background-clip:text;color:transparent;font-size:1.2em;line-height:1}
      .x{margin-left:auto;background:none;border:0;font:inherit;font-size:18px;line-height:1;cursor:pointer;color:#fff;opacity:.7;padding:2px 6px;border-radius:6px}
      .x:hover{opacity:1;background:rgba(255,255,255,.12)}
      .body{padding:11px 14px 13px}
      .ti{font-weight:900;font-size:15px;letter-spacing:-.01em;margin-bottom:3px}
      .t{font-size:13px;color:var(--text-2);white-space:pre-line}
      .a{display:flex;gap:6px;flex-wrap:wrap;margin-top:10px}
      .a:empty{display:none}
      button.k{font:inherit;font-size:12.5px;font-weight:700;border-radius:8px;padding:5px 11px;cursor:pointer;border:1px solid var(--border-strong);background:transparent;color:inherit}
      button.k:hover{border-color:var(--text-2)}
      button.k:focus-visible,.x:focus-visible{outline:2px solid var(--selected);outline-offset:1px}
      button.k.p{background:var(--accent);border-color:var(--accent);color:#fff}
      button.k.p:hover{background:var(--accent-deep)}
    </style>
    <div class="b" role="status" style="--tone:${tone === 'ok' ? 'var(--accent)' : tone === 'warn' ? 'var(--warn)' : 'var(--selected)'}">
      <div class="top"><span class="brand"><span class="s" aria-hidden="true">✦</span><span>huntwell</span></span><button class="x" title="Close" aria-label="Close">×</button></div>
      <div class="body"><div class="ti"></div><div class="t"></div><div class="a"></div></div>
    </div>`
    root.querySelector('.ti').textContent = title
    root.querySelector('.t').textContent = text
    const row = root.querySelector('.a')
    for (const a of actions) {
      const b = document.createElement('button')
      b.className = 'k' + (a.primary ? ' p' : '')
      b.textContent = a.label
      b.addEventListener('click', a.onClick)
      row.append(b)
    }
    root.querySelector('.x').addEventListener('click', () => {
      close()
      ask({ type: 'done' })
    })
  }
  function close() {
    host?.remove()
    host = null
  }

  /// The actions every banner can offer, whatever went wrong on the page.
  function fallbackActions(item) {
    return [
      {
        label: 'Copy message',
        onClick: async () => banner({ title: (await copy(item.message)) ? 'Copied' : 'Copy failed', text: 'Paste it into the message box, then press Send.', actions: fallbackActions(item) }),
      },
      { label: 'Mark sent', onClick: () => reportSent(item) },
      { label: 'Skip', onClick: async () => (await ask({ type: 'skip', id: item.id }), close()) },
    ]
  }

  async function reportSent(item) {
    const r = await ask({ type: 'sent', id: item.id, via: item.channel })
    if (r.error) banner({ title: 'Not recorded', text: r.error, tone: 'warn', actions: fallbackActions(item) })
    else {
      banner({ title: 'Sent — recorded in Huntwell', text: `${item.name || 'Message'} is marked sent.`, tone: 'ok' })
      setTimeout(close, 4000)
    }
  }

  /// After the person presses Send (or Enter): it went if the box is empty
  /// a moment later. Called on their click — never instead of it.
  function watchSend(item, composer, sendBtn) {
    let done = false
    const check = async () => {
      if (done) return
      await sleep(1500)
      if (!document.contains(composer) || textOf(composer) === '') {
        done = true
        reportSent(item)
      }
    }
    sendBtn?.addEventListener('click', check, true)
    composer.addEventListener('keydown', (e) => e.key === 'Enter' && !e.shiftKey && check(), true)
  }

  window.HW = { ask, sleep, waitFor, visible, textOf, fillField, fillEditable, copy, pointAt, banner, close, fallbackActions, reportSent, watchSend }
})()

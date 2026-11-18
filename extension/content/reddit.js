// Huntwell for Chrome — Reddit.
//
// The panel opens Reddit's compose page addressed to the person
// (/message/compose/?to=…&subject=…). Here: fill the message in (and the
// recipient and subject if Reddit left them empty), point at Send, and record
// it once the person has sent it. If Reddit sends them to chat instead, fill
// the chat box the same way. Never submits anything itself.

;(() => {
  const HW = window.HW
  const PENDING = 'huntwell-pending-sent'
  let handled = ''

  function composeForm() {
    const message = [...document.querySelectorAll('textarea[name="message"], textarea[name="text"], form textarea')].find(HW.visible)
    if (!message) return null
    const form = message.closest('form')
    const to = form?.querySelector('input[name="to"], input[name="recipient"]')
    const subject = form?.querySelector('input[name="subject"], input[name="title"]')
    const send =
      form?.querySelector('button[type="submit"], input[type="submit"]') ||
      [...(form?.querySelectorAll('button') || [])].find((b) => /^send$/i.test((b.textContent || '').trim()))
    return { form, to, subject, message, send }
  }

  /// Reddit chat's message box, when compose redirects there.
  function chatBox() {
    const box = [...document.querySelectorAll('textarea[placeholder*="essage" i], [contenteditable="true"][role="textbox"]')].filter(HW.visible).pop()
    if (!box) return null
    const scope = box.closest('form') || box.parentElement?.parentElement || document
    const send = [...scope.querySelectorAll('button')].find((b) => /send/i.test(b.getAttribute('aria-label') || b.textContent || ''))
    return { box, send }
  }

  async function fillCompose(item, f) {
    if (f.to && !f.to.value.trim()) HW.fillField(f.to, item.reddit)
    if (f.subject && !f.subject.value.trim()) HW.fillField(f.subject, (item.subject || '').slice(0, 100))
    if (!f.message.value.trim()) HW.fillField(f.message, item.message)
    HW.pointAt(f.send)
    // The page goes away when it sends, so the "it went" check happens on the
    // page after (see tick), from this note.
    const arm = () => sessionStorage.setItem(PENDING, String(item.id))
    f.send?.addEventListener('click', arm, true)
    f.form?.addEventListener('submit', arm, true)
    HW.banner({
      title: 'Ready — read it, then press Send',
      text: `Huntwell filled in your message to u/${item.reddit}. Nothing is sent until you press Send.`,
      tone: 'ok',
      actions: [{ label: 'Mark sent', onClick: () => HW.reportSent(item) }, ...HW.fallbackActions(item).filter((a) => a.label === 'Skip')],
    })
  }

  async function fillChat(item, c) {
    const ok = HW.textOf(c.box) ? true : c.box.tagName === 'TEXTAREA' ? (HW.fillField(c.box, item.message), true) : await HW.fillEditable(c.box, item.message)
    if (!ok) {
      HW.banner({ title: "Couldn't fill it in", text: 'Use Copy message and paste it in, then press Send.', tone: 'warn', actions: HW.fallbackActions(item) })
      return
    }
    HW.pointAt(c.send)
    HW.watchSend(item, c.box, c.send)
    HW.banner({
      title: 'Ready — read it, then press Send',
      text: `Huntwell filled in your message to u/${item.reddit}. Nothing is sent until you press Send.`,
      tone: 'ok',
      actions: [{ label: 'Mark sent', onClick: () => HW.reportSent(item) }],
    })
  }

  async function tick() {
    const { item } = await HW.ask({ type: 'job' })
    if (!item || item.channel !== 'reddit') return
    const key = item.id + ':' + location.pathname
    if (handled === key) return
    handled = key

    // Pressed Send on the compose page, and Reddit moved on: it went.
    const pending = sessionStorage.getItem(PENDING)
    if (pending === String(item.id) && !/\/message\/compose/.test(location.pathname)) {
      sessionStorage.removeItem(PENDING)
      return HW.reportSent(item)
    }

    HW.banner({ title: 'Getting the message ready…', text: `For u/${item.reddit}.` })
    const found = await HW.waitFor(() => {
      const f = composeForm()
      if (f) return { kind: 'compose', f }
      const c = chatBox()
      if (c) return { kind: 'chat', c }
      return null
    }, 10000)
    if (!found) {
      HW.banner({
        title: "Couldn't find Reddit's message box",
        text: `Open a chat with u/${item.reddit}, paste the message, then press Send.`,
        tone: 'warn',
        actions: HW.fallbackActions(item),
      })
      return
    }
    if (found.kind === 'compose') fillCompose(item, found.f)
    else fillChat(item, found.c)
  }

  let last = location.href
  setInterval(() => {
    if (location.href !== last) {
      last = location.href
      tick()
    }
  }, 700)
  tick()
})()

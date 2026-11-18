// Huntwell for Chrome — LinkedIn.
//
// On a tab the panel opened for a queued message: on the person's profile,
// open LinkedIn's own message window, fill the message in, and point at Send.
// The person reads it and presses Send; we see the box empty and record it.
// On a people search (no profile link was known), say so and wait for them to
// open the right profile.
//
// LinkedIn's markup changes often, so every lookup tries a few shapes and
// anything that is not found ends in the banner's Copy / Mark sent / Skip.

;(() => {
  const HW = window.HW
  let handled = ''

  /// The profile's own Message button (not a "Message" in the nav or a feed).
  function messageButton() {
    const scope = document.querySelector('main') || document
    const candidates = [...scope.querySelectorAll('button, a')].filter((b) => {
      const label = (b.getAttribute('aria-label') || b.textContent || '').trim()
      return /^message\b/i.test(label) && HW.visible(b)
    })
    return candidates[0] || null
  }

  /// The newest open message window's editor and its Send button.
  function composer() {
    const boxes = [
      ...document.querySelectorAll(
        '.msg-form__contenteditable[contenteditable="true"], .msg-form [role="textbox"][contenteditable="true"], [data-test-msg-form] [contenteditable="true"]',
      ),
    ].filter(HW.visible)
    const box = boxes[boxes.length - 1]
    if (!box) return null
    const form = box.closest('form') || box.closest('.msg-form') || box.parentElement
    const send =
      form?.querySelector('button.msg-form__send-button, button[type="submit"]') ||
      [...(form?.querySelectorAll('button') || [])].find((b) => /^send$/i.test((b.textContent || '').trim())) ||
      null
    return { box, send }
  }

  async function fill(item) {
    HW.banner({ title: 'Opening the message window…', text: `For ${item.name || 'this person'}.` })
    const btn = await HW.waitFor(messageButton, 8000)
    if (!btn) {
      HW.banner({
        title: 'No Message button here',
        text: "LinkedIn only lets you message people you're connected to (or with InMail). Connect first — you can paste the message as the invitation note — or send it another way.",
        tone: 'warn',
        actions: HW.fallbackActions(item),
      })
      return
    }
    // Opening LinkedIn's message window is the one click made for the person,
    // and it sends nothing.
    btn.click()
    const c = await HW.waitFor(composer, 8000)
    if (!c) {
      HW.banner({ title: "Couldn't find the message box", text: 'Open the message window, paste the message, then press Send.', tone: 'warn', actions: HW.fallbackActions(item) })
      return
    }
    const already = HW.textOf(c.box)
    const want = item.message.replace(/\s+/g, ' ').trim()
    if (already && !already.includes(want.slice(0, 40))) {
      HW.banner({ title: 'The message box already has text', text: "It's left as it is. Clear it and use Copy message, or send what's there.", tone: 'warn', actions: HW.fallbackActions(item) })
      HW.watchSend(item, c.box, c.send)
      return
    }
    const ok = already ? true : await HW.fillEditable(c.box, item.message)
    if (!ok) {
      HW.banner({ title: "Couldn't fill it in", text: 'Use Copy message and paste it in, then press Send.', tone: 'warn', actions: HW.fallbackActions(item) })
      HW.watchSend(item, c.box, c.send)
      return
    }
    HW.pointAt(c.send)
    HW.watchSend(item, c.box, c.send)
    HW.banner({
      title: 'Ready — read it, then press Send',
      text: `Huntwell filled in your message to ${item.name || 'them'}. Edit anything you like. Nothing is sent until you press Send.`,
      tone: 'ok',
      actions: [{ label: 'Mark sent', onClick: () => HW.reportSent(item) }, ...HW.fallbackActions(item).filter((a) => a.label === 'Skip')],
    })
  }

  async function tick() {
    const { item } = await HW.ask({ type: 'job' })
    if (!item || item.channel !== 'linkedin') return
    const onProfile = /^\/in\/[^/]+/.test(location.pathname)
    const key = item.id + ':' + location.pathname
    if (handled === key) return
    handled = key
    if (!onProfile) {
      HW.banner({
        title: `Find ${item.name || 'them'}`,
        text: "Huntwell didn't have their profile link, so this is a search. Open the right person's profile and Huntwell will offer to fill the message in.",
        actions: HW.fallbackActions(item),
      })
      return
    }
    if (item.linkedin) return fill(item)
    // Found by search: make sure it is the right person before filling.
    HW.banner({
      title: `Is this ${item.name || 'the right person'}?`,
      text: [item.title, item.company].filter(Boolean).join(' · ') || 'Check before filling the message in.',
      actions: [{ label: 'Fill message', primary: true, onClick: () => fill(item) }, ...HW.fallbackActions(item)],
    })
  }

  // LinkedIn is a single-page app: a new profile is a new URL, not a new page.
  let last = location.href
  setInterval(() => {
    if (location.href !== last) {
      last = location.href
      tick()
    }
  }, 700)
  tick()
})()

// The page, as the accessibility tree the agent reads.
//
// Runs in the page through CDP `Runtime.evaluate` and returns the YAML text
// `thrift::trim` trims and the model reads. The format is Playwright's, because
// the trimmer, the trail, the guard's host scan and the scrape prompts were all
// written against it:
//
//   - list [ref=e12]:
//     - listitem:
//       - link "2022 Subaru Crosstrek" [ref=e13]:
//         - /url: https://cars.test/v/1
//       - text: $27,995
//
// A ref is put on the element as `data-hw-ref`, so clicking and typing are a
// `querySelector` away and no node handles have to be kept between calls.
//
// Deliberately not a full ARIA implementation: roles and names are computed the
// way they are *used* on listing pages, which `direct::tools` tests against
// Playwright's own output on real pages.
(() => {
  const MAX_NODES = 6000;      // a page that big is a bug or a trap
  const MAX_TEXT = 400;        // one run of text; the rest is not a row
  const MAX_NAME = 200;

  let refs = 0;
  let nodes = 0;
  const out = [];

  // Elements never worth describing.
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'LINK', 'META', 'HEAD', 'SVG', 'PATH', 'BR', 'HR']);

  const INPUT_ROLE = {
    button: 'button', submit: 'button', reset: 'button', image: 'button',
    checkbox: 'checkbox', radio: 'radio', range: 'slider',
    search: 'searchbox', number: 'spinbutton',
    text: 'textbox', email: 'textbox', tel: 'textbox', url: 'textbox', password: 'textbox',
    date: 'textbox', 'datetime-local': 'textbox', month: 'textbox', week: 'textbox', time: 'textbox',
    hidden: null, file: 'button', color: 'textbox',
  };

  const TAG_ROLE = {
    A: 'link', BUTTON: 'button', SELECT: 'combobox', OPTION: 'option', TEXTAREA: 'textbox',
    IMG: 'img', NAV: 'navigation', MAIN: 'main', FORM: 'form', TABLE: 'table',
    THEAD: 'rowgroup', TBODY: 'rowgroup', TFOOT: 'rowgroup', TR: 'row', TD: 'cell', TH: 'columnheader',
    UL: 'list', OL: 'list', LI: 'listitem', DL: 'list', DT: 'term', DD: 'definition',
    P: 'paragraph', BLOCKQUOTE: 'blockquote', ARTICLE: 'article', ASIDE: 'complementary',
    SECTION: 'region', FIGURE: 'figure', FIGCAPTION: 'caption', CAPTION: 'caption',
    DIALOG: 'dialog', PROGRESS: 'progressbar', METER: 'meter', SUMMARY: 'button',
    STRONG: 'strong', EM: 'emphasis', B: 'strong', I: 'emphasis', CODE: 'code',
    SUP: 'superscript', SUB: 'subscript', TIME: 'time', LABEL: 'label',
    H1: 'heading', H2: 'heading', H3: 'heading', H4: 'heading', H5: 'heading', H6: 'heading',
    IFRAME: 'iframe', VIDEO: 'video', AUDIO: 'audio', CANVAS: 'canvas',
  };

  // A landmark's role depends on where it sits: a <header> inside an article
  // is not the page's banner.
  function scopedRole(el) {
    const scoped = el.closest('article, section, aside, main, nav, [role="article"]');
    if (el.tagName === 'HEADER') return scoped && scoped !== el ? 'generic' : 'banner';
    if (el.tagName === 'FOOTER') return scoped && scoped !== el ? 'generic' : 'contentinfo';
    return null;
  }

  function roleOf(el) {
    const explicit = (el.getAttribute('role') || '').trim().split(/\s+/)[0];
    if (explicit) return explicit.toLowerCase();
    if (el.tagName === 'HEADER' || el.tagName === 'FOOTER') return scopedRole(el);
    if (el.tagName === 'INPUT') {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (type in INPUT_ROLE) return INPUT_ROLE[type];
      return 'textbox';
    }
    if (el.tagName === 'A') return el.hasAttribute('href') ? 'link' : 'generic';
    if (el.tagName === 'IMG') return el.getAttribute('alt') === '' ? null : 'img';
    return TAG_ROLE[el.tagName] || 'generic';
  }

  function hidden(el) {
    if (el.getAttribute('aria-hidden') === 'true') return true;
    if (el.hasAttribute('hidden')) return true;
    if (el.tagName === 'INPUT' && (el.getAttribute('type') || '').toLowerCase() === 'hidden') return true;
    const style = el.ownerDocument.defaultView.getComputedStyle(el);
    if (!style) return false;
    if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') return true;
    // Zero-size containers still hold content in some layouts, so only an
    // explicitly zeroed opacity counts.
    if (style.opacity === '0') return true;
    return false;
  }

  function clean(s) {
    return (s || '').replace(/\s+/g, ' ').trim();
  }

  function cut(s, max) {
    s = clean(s);
    return s.length > max ? s.slice(0, max) + '…' : s;
  }

  /** The accessible name, by the rules that actually matter on a page of listings. */
  function nameOf(el, role) {
    const aria = clean(el.getAttribute('aria-label'));
    if (aria) return cut(aria, MAX_NAME);

    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const text = by.split(/\s+/)
        .map((id) => el.ownerDocument.getElementById(id))
        .filter(Boolean)
        .map((n) => clean(n.textContent))
        .join(' ');
      if (clean(text)) return cut(text, MAX_NAME);
    }

    if (el.tagName === 'IMG') return cut(el.getAttribute('alt') || '', MAX_NAME);
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') {
      // A field is named by its label, then its placeholder, then its title.
      const id = el.getAttribute('id');
      const label = id && el.ownerDocument.querySelector(`label[for="${CSS.escape(id)}"]`);
      const wrapping = el.closest('label');
      const text = (label && clean(label.textContent)) || (wrapping && clean(wrapping.textContent));
      if (text) return cut(text, MAX_NAME);
      const ph = clean(el.getAttribute('placeholder')) || clean(el.getAttribute('title'));
      if (ph) return cut(ph, MAX_NAME);
      if (el.tagName === 'INPUT' && (el.getAttribute('type') || '') === 'submit') return cut(el.value || 'Submit', MAX_NAME);
      return '';
    }

    // Roles whose name comes from what is inside them.
    const FROM_CONTENT = new Set([
      'link', 'button', 'heading', 'option', 'listitem', 'cell', 'columnheader', 'rowheader',
      'menuitem', 'tab', 'checkbox', 'radio', 'switch', 'treeitem', 'term', 'caption', 'summary',
    ]);
    if (FROM_CONTENT.has(role)) {
      const text = clean(el.textContent) || clean(el.getAttribute('title'));
      return cut(text, MAX_NAME);
    }
    return '';
  }

  function attributes(el, role) {
    const bits = [];
    if (role === 'heading') {
      const level = Number(el.getAttribute('aria-level')) || Number((el.tagName.match(/^H(\d)$/) || [])[1]);
      if (level) bits.push(`[level=${level}]`);
    }
    if (el === el.ownerDocument.activeElement && el !== el.ownerDocument.body) bits.push('[active]');
    const checked = el.getAttribute('aria-checked') || (('checked' in el && typeof el.checked === 'boolean') ? String(el.checked) : null);
    if (checked === 'true' || checked === true) bits.push('[checked]');
    if (el.selected === true || el.getAttribute('aria-selected') === 'true') bits.push('[selected]');
    const expanded = el.getAttribute('aria-expanded');
    if (expanded === 'true') bits.push('[expanded]');
    if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') bits.push('[disabled]');
    return bits;
  }

  /** Whether the page styles this as something to click. */
  function pointer(el) {
    try {
      return el.ownerDocument.defaultView.getComputedStyle(el).cursor === 'pointer';
    } catch { return false; }
  }

  const CLICKABLE = new Set(['link', 'button', 'checkbox', 'radio', 'tab', 'menuitem', 'option', 'combobox', 'textbox', 'searchbox', 'switch', 'slider', 'spinbutton']);

  /** Elements the model may act on get a ref; plain structure does not need one. */
  function wantsRef(el, role) {
    if (CLICKABLE.has(role)) return true;
    if (el.tagName === 'SELECT' || el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') return true;
    return pointer(el);
  }

  function line(depth, text) {
    out.push('  '.repeat(depth) + '- ' + text);
  }

  /** The direct text of an element, ignoring what its element children hold. */
  function ownText(el) {
    let s = '';
    for (const n of el.childNodes) {
      if (n.nodeType === 3) s += n.nodeValue;
    }
    return clean(s);
  }

  function hasElementChildren(el) {
    for (const n of el.children) {
      if (!SKIP.has(n.tagName)) return true;
    }
    return false;
  }

  function walk(el, depth) {
    if (nodes++ > MAX_NODES) return;
    if (SKIP.has(el.tagName) || hidden(el)) return;

    const role = roleOf(el);
    if (role === null) return;  // a decorative image says nothing

    const name = nameOf(el, role);
    const bits = attributes(el, role);
    if (wantsRef(el, role)) {
      const ref = 'e' + (++refs);
      el.setAttribute('data-hw-ref', ref);
      bits.push(`[ref=${ref}]`);
    }
    if (pointer(el) && CLICKABLE.has(role)) bits.push('[cursor=pointer]');

    let head = role;
    if (name) head += ` "${name.replace(/"/g, "'")}"`;
    if (bits.length) head += ' ' + bits.join(' ');

    // What hangs under this node: an href for a link, then text of its own,
    // then its children.
    const href = el.tagName === 'A' ? el.getAttribute('href') : null;
    const own = ownText(el);
    const children = hasElementChildren(el);
    const inlineText = own && !children && own !== name;

    if (!href && !children && !inlineText) {
      line(depth, head);
      return;
    }
    if (inlineText && !href && !children) {
      line(depth, `${head}: ${cut(own, MAX_TEXT)}`);
      return;
    }

    line(depth, head + ':');
    if (href) {
      let absolute = href;
      try { absolute = new URL(href, el.ownerDocument.baseURI).href; } catch { /* keep as written */ }
      line(depth + 1, `/url: ${absolute}`);
    }
    if (own && own !== name) line(depth + 1, `text: ${cut(own, MAX_TEXT)}`);
    for (const child of el.children) {
      if (nodes > MAX_NODES) break;
      walk(child, depth + 1);
      // Text sitting between elements is content too.
      const after = child.nextSibling;
      if (after && after.nodeType === 3) {
        const t = clean(after.nodeValue);
        if (t) line(depth + 1, `text: ${cut(t, MAX_TEXT)}`);
      }
    }
  }

  // Refs are per snapshot: an old one must not resolve to a new element.
  for (const old of document.querySelectorAll('[data-hw-ref]')) old.removeAttribute('data-hw-ref');

  const root = document.body || document.documentElement;
  if (root) {
    for (const child of root.children) walk(child, 0);
  }
  return out.join('\n');
})()

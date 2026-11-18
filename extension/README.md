# Huntwell for Chrome

Sends queued Huntwell outreach on LinkedIn and Reddit. It opens the person's
page, fills the message in and points at Send. **The person presses Send** —
the extension never clicks it, submits a form, or presses Enter for them.
That is the product's rule, not a missing feature: LinkedIn's terms forbid
automated messaging, Reddit treats automated DMs as spam, and the Chrome Web
Store rejects extensions that facilitate spam.

## Try it (unpacked)

1. `chrome://extensions` → turn on **Developer mode** → **Load unpacked** → this folder.
   The `key` in `manifest.json` pins the id to `cicgcidophbcmijifdjjmgphkhggaddi`,
   which the server allows by default (`web/extension.rs` `BUILT_IN_ID`).
2. Click the Huntwell icon → the side panel opens → enter your Huntwell
   (`http://localhost:5611` for `./dev.sh`) → **Sign in to Huntwell** → Allow.
3. In Huntwell, open a draft under Outreach → **Queue for LinkedIn** / **Queue for Reddit**.
4. In the panel, **Open & fill**. Read it, press Send. It is marked sent in
   Huntwell (and logged on the contact, with the CRM module).

## Files

| | |
|---|---|
| `manifest.json` | MV3; side panel, `identity` (sign-in), `storage`, `tabs`; content scripts on linkedin.com and reddit.com |
| `panel.*` | sign-in and the queue |
| `background.js` | which queued message belongs to which tab; records "sent" |
| `lib.js` | the server address, the key, calls to `/ext/v1` |
| `content/common.js` | filling text the way the sites' editors notice, the on-page banner, "did it send" |
| `content/linkedin.js`, `content/reddit.js` | each site's message box |
| `icons/` | the app's ✦ mark (`UI/web/public/icon-512.png`) at 16/32/48/128 |
| `fonts/` | Lato 400/700/900 (latin, OFL — `OFL.txt`), bundled: an extension should not load fonts from Google |

**Branding** follows the app: the tokens in `panel.css` (and the banner's in
`content/common.js`) are copies of `UI/web/src/styles.css` — aubergine header,
green primary, blue focus, the four-stop rainbow on the ✦ — so change them
there and here together. The banner lives in a shadow root, where `@font-face`
is ignored, so `common.js` declares the bundled Lato on the page as
`Huntwell Lato` (the fonts are `web_accessible_resources` for those two sites).

LinkedIn and Reddit change their markup often. Every lookup tries a few shapes,
and anything not found ends in the banner's **Copy message / Mark sent / Skip**,
so a site change degrades to copy-and-paste rather than breaking.

## Publishing

Zip this folder and upload it to the Chrome Web Store. The store assigns its own
id: add it to the server's `HUNTWELL_EXTENSION_IDS` (comma-separated; list the
built-in id too to keep unpacked builds working), or sign-in is refused outside
dev. The store listing's permission justification: `identity` signs in to the
user's own Huntwell; host access to LinkedIn/Reddit fills messages the user
then sends themselves.

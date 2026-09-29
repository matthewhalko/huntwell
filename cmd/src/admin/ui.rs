//! The whole admin dashboard: one embedded page, no build step, 5s poll.
//! Framed like the product's Slack-style UI — aubergine chrome with the
//! workspace floating as a rounded pane, Lato, green primary, blue selection.
//! The routing log renders in AG Grid (CDN) so it is sortable/filterable.

pub const HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Huntwell Admin</title>
<link rel="icon" type="image/png" href="/favicon.png">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Lato:wght@400;700;900&display=swap" rel="stylesheet">
<link href="https://cdn.jsdelivr.net/npm/ag-grid-community@31.3.2/styles/ag-grid.css" rel="stylesheet">
<link href="https://cdn.jsdelivr.net/npm/ag-grid-community@31.3.2/styles/ag-theme-quartz.css" rel="stylesheet">
<script src="https://cdn.jsdelivr.net/npm/ag-grid-community@31.3.2/dist/ag-grid-community.min.js"></script>
<script>
// Before first paint, or the page flashes light then repaints dark. Same key
// the product app uses, so a browser that has both open agrees with itself.
(function(){try{var t=localStorage.getItem('huntwell.theme');
  if(t!=='light'&&t!=='dark')t=matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light';
  document.documentElement.dataset.theme=t}catch(e){}})();
</script>
<style>
/* AG Grid, in the admin's own colours. Same idea as the app's `.hw-grid`:
   only the theme variables are set, so a grid follows the light/dark switch
   with every other surface. */
.hw-grid{
  overflow:hidden;border:1px solid var(--border);border-radius:var(--radius);
}
.hw-grid.card{border-color:var(--border);box-shadow:none}
.hw-grid .ag-root-wrapper,
.hw-grid .ag-root-wrapper-body,
.hw-grid .ag-root{border-radius:inherit;overflow:hidden;border:none}
.hw-grid .ag-header,
.hw-grid .ag-header-row,
.hw-grid .ag-header-cell,
.hw-grid .ag-header-group-cell{
  background:var(--surface);
}
.hw-grid .ag-header{
  border-bottom:1px solid var(--border);
  border-top-left-radius:inherit;border-top-right-radius:inherit;
}
.hw-grid.ag-theme-quartz{
  --ag-background-color:var(--surface);--ag-foreground-color:var(--text);
  --ag-secondary-foreground-color:var(--text-2);
  --ag-header-background-color:var(--surface);--ag-header-foreground-color:var(--text-2);
  --ag-header-cell-hover-background-color:var(--surface);
  --ag-border-color:var(--border);--ag-row-border-color:var(--border);
  --ag-row-hover-color:var(--bg-2);--ag-odd-row-background-color:transparent;
  --ag-font-family:inherit;--ag-font-size:.86rem;--ag-borders:none;--ag-grid-size:5px;
  /* Without this the grid uses the row height as line-height, and a pill
     ("Routed", "succeeded") grows as tall as the row. */
  --ag-line-height:1.5em;
  --ag-wrapper-border-radius:0;
  --ag-input-focus-border-color:var(--accent);--ag-range-selection-border-color:var(--accent);
}
.hw-grid .ag-row.clickable{cursor:pointer}
.hw-grid .ag-cell{display:flex;align-items:center;line-height:1.5}
.hw-grid .badge{line-height:1.5;height:auto;align-self:center;flex:none}
.hw-grid .ag-right-aligned-cell,.hw-grid .ag-header-cell.ag-right-aligned-header{justify-content:flex-end}
.hw-grid .ag-row-pinned{background:var(--bg-2);font-weight:600}

/* ---------------------------------------------------------------------------
   The admin wears the product's clothes. Tokens, spacing and component shapes
   are copied from UI/web/src/styles.css by name, so the two look like one
   system and a change there can be carried across by hand without translation.
   --------------------------------------------------------------------------- */
:root{
  --chrome:#3f0e40; --chrome-2:#350d36;
  --selected:#1264a3; --accent:#007a5a; --accent-deep:#00604a; --accent-soft:#e6f3ee; --accent-text:#fff;
  --brand-sky:#36c5f0; --brand-green:#2eb67d; --brand-yellow:#ecb22e; --brand-pink:#e01e5a;
  --grad:linear-gradient(120deg,var(--brand-sky),var(--brand-green) 38%,var(--brand-yellow) 66%,var(--brand-pink));
  --ease-spring:cubic-bezier(.34,1.35,.64,1);
  --bg:#fff; --bg-2:#f8f8f8; --surface:#fff; --surface-2:#f8f8f8;
  --border:#e0e0e0; --border-strong:#c5c5c5;
  --text:#1d1c1d; --text-2:#616061; --text-3:#9a999a;
  --cornflower:#1264a3; --link:#1264a3;
  --ok:#007a5a; --ok-bg:#e6f3ee; --warn:#a06d00; --warn-bg:#faf0d2;
  --bad:#e01e5a; --bad-bg:#fdecf2; --info-bg:#e8f2fa;
  --shadow:0 1px 2px rgba(29,28,29,.06),0 8px 24px -14px rgba(29,28,29,.2);
  --radius:10px; --radius-sm:4px;
  --font:'Lato',-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
  --mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  color-scheme:light;
}
:root[data-theme='dark']{
  --chrome:#0b0b0d; --chrome-2:#0b0b0d;
  --accent-soft:#14261f;
  --bg:#111113; --bg-2:#0b0b0d; --surface:#1a1a1d; --surface-2:#222226;
  --border:#2c2c30; --border-strong:#45454b;
  --text:#d1d2d3; --text-2:#ababad; --text-3:#77787c;
  --cornflower:#1d9bd1; --link:#1d9bd1;
  --ok:#2eb67d; --ok-bg:#17301f; --warn:#e8a317; --warn-bg:#33290f;
  --bad:#ec6390; --bad-bg:#38202a; --info-bg:#16344a;
  --shadow:0 1px 2px rgba(0,0,0,.4),0 10px 28px -14px rgba(0,0,0,.7);
  color-scheme:dark;
}

*{box-sizing:border-box}
body{margin:0;font-family:var(--font);font-size:14.5px;line-height:1.5;background:var(--chrome);
  color:var(--text);-webkit-font-smoothing:antialiased}
/* The app's shell: topbar across the top, a 68px rail down the left, the pane
   beside it. Copied from UI/web/src/styles.css (.shell/.rail/.rail-item) so the
   two consoles are one design; change them together. */
.shell{display:grid;grid-template-columns:68px 1fr;grid-template-rows:auto 1fr;height:100vh;background:var(--chrome)}
.shell.noauth{grid-template-columns:1fr}
.shell.noauth .rail{display:none}
.rail{grid-row:2;grid-column:1;display:flex;flex-direction:column;align-items:center;gap:.55rem;
  padding:.4rem 0 1rem;min-height:0;overflow-y:auto;overflow-x:hidden;user-select:none}
.rail-nav{display:flex;flex-direction:column;align-items:center;gap:.55rem;width:100%}
.rail-item{display:flex;flex-direction:column;align-items:center;gap:3px;background:none;border:0;padding:0;cursor:pointer;
  width:100%;color:rgba(255,255,255,.64);font:inherit;text-decoration:none;flex:none;box-shadow:none;border-radius:0}
.rail-item:hover{text-decoration:none;color:rgba(255,255,255,.95);box-shadow:none;border:0}
.rail-item:active{transform:none}
.rail-item .tile{width:36px;height:36px;border-radius:8px;display:grid;place-items:center;
  transition:background .12s ease,transform .16s var(--ease-spring)}
.rail-item .tile svg{display:block}
.rail-item:hover .tile{background:rgba(255,255,255,.1);transform:translateY(-1px)}
.rail-item.active{color:rgba(255,255,255,.95)}
.rail-item.active .tile{background:rgba(255,255,255,.22)}
.rail-item .rlbl{font-size:10px;font-weight:500;line-height:1.1;text-align:center}
.rail-item.active .rlbl{font-weight:700}
.rail .spacer{flex:1}
.page{display:flex;flex-direction:column;gap:1.1rem}
.page.hide{display:none}
.page .page-head{margin-bottom:0}
a{color:var(--link);text-decoration:none}
a:hover{text-decoration:underline}
h1,h2,h3{font-weight:900;letter-spacing:-.02em;margin:0}
h1{font-size:1.65rem}
h2{font-size:1.05rem;letter-spacing:-.01em}

/* ---------- chrome ---------- */
.topbar{grid-row:1;grid-column:1/-1;background:var(--chrome);color:#fff;display:flex;align-items:center;justify-content:space-between;
  gap:1rem;padding:.55rem 1.2rem .55rem 0;min-height:48px}
/* the whole logo lockup over the rail column, as in the app */
.brand{display:flex;align-items:center;gap:.3rem;padding-left:18px;color:#fff;font-weight:900;font-size:1.32rem;letter-spacing:-.02em;user-select:none}
.brand .spark{font-size:1.2em;line-height:1;display:inline-block;transition:transform .25s var(--ease-spring)}
.brand:hover .spark{transform:rotate(20deg) scale(1.1)}
.grad-text{background:var(--grad);-webkit-background-clip:text;background-clip:text;color:transparent}
/* "admin" reads as a label on the product, not as part of the wordmark */
.brand .tag{font-size:.72rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase;
  padding:.12rem .45rem;border-radius:var(--radius-sm);background:rgba(255,255,255,.16);margin-left:.45rem;align-self:center}
.who{display:flex;align-items:center;gap:.6rem;font-size:.9rem;color:rgba(255,255,255,.85)}
.who button{background:transparent;color:#fff;border-color:rgba(255,255,255,.35)}
.who button:hover{background:rgba(255,255,255,.12);border-color:rgba(255,255,255,.6);box-shadow:none}
.iconbtn{display:inline-grid;place-items:center;width:32px;height:32px;padding:0;border-radius:var(--radius-sm);
  border:1px solid rgba(255,255,255,.35);background:transparent;color:#fff;cursor:pointer}
.iconbtn:hover{background:rgba(255,255,255,.12)}

/* the workspace pane floating inside the chrome, exactly as in the app */
.pane{grid-row:2;grid-column:2;background:var(--bg);border-radius:8px;margin:0 6px 6px 6px;overflow-y:auto;min-height:0;min-width:0}
.shell.noauth .pane{grid-column:1}
:root[data-theme='dark'] .pane{border:1px solid var(--border)}
.content{padding:1.4rem 1.1rem 2.4rem;max-width:none;display:flex;flex-direction:column;gap:1.1rem}
@keyframes rise{from{opacity:0;transform:translateY(7px)}to{opacity:1;transform:none}}
.content>*{animation:rise .28s ease both}

/* the title row a page opens with */
.page-head{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;flex-wrap:wrap;margin-bottom:.3rem}
.page-head .sub{color:var(--text-2);margin-top:.2rem;font-size:.92rem}

/* ---------- cards ---------- */
.card{background:var(--surface);border:1px solid var(--border-strong);border-radius:var(--radius);padding:1.2rem 1.3rem;box-shadow:var(--shadow)}
.card.pad0{padding:0;overflow:hidden}
.card-head{display:flex;align-items:baseline;justify-content:space-between;gap:.8rem;flex-wrap:wrap;margin-bottom:.9rem}
.card-head .sub{color:var(--text-2);font-size:.84rem}

/* ---------- controls ---------- */
button,.btn{display:inline-flex;align-items:center;justify-content:center;gap:.45rem;font:inherit;font-weight:700;
  padding:.5rem .95rem;border-radius:var(--radius-sm);border:1px solid var(--border-strong);
  background:var(--surface);color:var(--text);cursor:pointer;white-space:nowrap;
  transition:transform .06s ease,background .15s ease,box-shadow .15s ease,border-color .15s ease}
button:hover{border-color:var(--text-3);box-shadow:var(--shadow)}
button:active{transform:translateY(1px)}
button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-text)}
button.primary:hover{background:var(--accent-deep);border-color:var(--accent-deep)}
button.danger{color:var(--bad)}
button.danger:hover{background:var(--bad-bg);border-color:var(--bad)}
button.sm{padding:.35rem .7rem;font-size:.85rem}
input:where(:not([type='checkbox'],[type='radio'])),select,textarea{
  font:inherit;color:var(--text);background:var(--surface);border:1px solid var(--border-strong);
  border-radius:var(--radius-sm);padding:.55rem .75rem;width:100%;outline:none;appearance:none;-webkit-appearance:none;
  transition:border-color .15s,box-shadow .15s}
input:focus,select:focus,textarea:focus{border-color:var(--selected);box-shadow:0 0 0 3px var(--info-bg)}
select option,select optgroup{background:var(--surface);color:var(--text)}
textarea{min-height:120px;resize:vertical;font-family:var(--mono);font-size:.82rem;line-height:1.45}
label{font-weight:700;font-size:.84rem;display:block;margin:.7rem 0 .25rem;color:var(--text)}
input[type='checkbox'],input[type='radio']{accent-color:var(--accent);width:auto;cursor:pointer}
.row{display:flex;gap:.6rem;flex-wrap:wrap;align-items:center}
.grid2{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:0 .8rem}

/* ---------- data ---------- */
table{width:100%;border-collapse:collapse;font-size:.88rem}
th,td{text-align:left;padding:.6rem .75rem;border-bottom:1px solid var(--border);vertical-align:top}
th{font-size:.76rem;text-transform:uppercase;letter-spacing:.06em;color:var(--text-3);font-weight:700;background:var(--surface)}
tbody tr:hover td{background:var(--surface-2)}
tbody tr:last-child td{border-bottom:none}
/* A row's actions are one group, not a paragraph: size the last column to its
   content and keep the buttons on one line, or Edit/Sync/Delete wrap and the
   row grows a second storey. */
td:last-child{width:1%;white-space:nowrap}
/* `.row` is display:flex, and a table cell that is also a flex container draws
   its border-bottom at content height — leaving a stub of rule under the
   buttons, above the row's real separator. Lay these out inline instead. */
td.row{display:table-cell;white-space:nowrap}
td.row>button{margin-right:.4rem}
td.row>button:last-child{margin-right:0}
#services td:last-child{width:auto;white-space:normal}
.stat{background:var(--surface);border:1px solid var(--border-strong);border-radius:var(--radius);padding:1rem 1.2rem;min-width:130px;flex:1;box-shadow:var(--shadow)}
.stat .n{font-size:1.9rem;font-weight:900;line-height:1.1;letter-spacing:-.02em}
.stat .l{color:var(--text-2);font-size:.85rem;margin-top:.15rem}
.badge{display:inline-flex;align-items:center;gap:.3rem;padding:.15rem .6rem;border-radius:999px;font-size:.76rem;
  font-weight:700;background:var(--surface-2);color:var(--text-2);border:1px solid var(--border)}
.badge a{color:inherit;opacity:.55;font-weight:900;text-decoration:none}
.badge a:hover{opacity:1;text-decoration:none}
.badge.ok{background:var(--ok-bg);color:var(--ok);border-color:transparent}
.badge.bad{background:var(--bad-bg);color:var(--bad);border-color:transparent}
/* A message that belongs to a form: a wrong password, a locked account, a
   server that could not be reached. Its own card, not a badge or a muted
   aside, so it reads as the answer to what was just tried. */
.notice{display:flex;gap:.6rem;align-items:flex-start;margin-top:1rem;padding:.75rem .95rem;border-radius:var(--radius);
  background:var(--bad-bg);color:var(--bad);font-size:.92rem;line-height:1.45;border:1px solid transparent}
.notice.info{background:var(--info-bg);color:var(--text)}
.notice[hidden]{display:none}
.notice .ico{flex:none;font-weight:700}
.notice b{font-weight:700}
.badge.warn{background:var(--warn-bg);color:var(--warn);border-color:transparent}
.badge.info{background:var(--info-bg);color:var(--cornflower);border-color:transparent}
.muted{color:var(--text-2)}
.mono{font-family:var(--mono);font-size:.82rem}
.hide{display:none}

/* Model picker: a dialog we draw, not the OS combo box. The list is long and
   priced, so it has to be filterable and it has to wear the theme. */
.model-pick{display:flex;align-items:center;justify-content:space-between;gap:.5rem;width:100%;
  text-align:left;font:inherit;font-weight:500;cursor:pointer;white-space:nowrap;min-width:0}
.model-pick .v{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.model-pick .caret{width:0;height:0;border:4px solid transparent;border-top-color:var(--text-3);margin-top:3px;flex:none}
.model-list{max-height:min(50vh,22rem);overflow-y:auto;margin-top:.55rem;display:flex;flex-direction:column}
.model-grp{font-family:var(--mono);font-size:.64rem;letter-spacing:.13em;text-transform:uppercase;
  color:var(--text-3);padding:.55rem .45rem .2rem}
.model-opt{display:flex;align-items:center;justify-content:space-between;gap:.6rem;width:100%;
  text-align:left;border:0;background:none;padding:.5rem .55rem;border-radius:var(--radius-sm);
  cursor:pointer;font:inherit;font-weight:500;color:var(--text)}
.model-opt:hover{background:var(--surface-2);box-shadow:none;border:0}
.model-opt:active{transform:none}
.model-opt.on{background:var(--surface-2);color:var(--link);font-weight:700}
.model-opt small{color:var(--text-3);font-weight:400;white-space:nowrap;flex:none}
.model-none{padding:.7rem .55rem}
#models td:nth-child(2){min-width:16rem}

/* ---------- sign in ---------- */
/* The two cards that stand alone on an otherwise empty page. Both, not just
   #login: the first-run card carries the h2 and .sub that the sign-in card
   used to, and scoping these to #login left it unstyled. */
#login,#firstrun{max-width:380px;margin:12vh auto;box-shadow:var(--shadow)}
#firstrun h2{margin-bottom:.2rem}
#firstrun .sub{color:var(--text-2);font-size:.9rem;margin-bottom:.4rem}
/* With the heading gone the first field would sit against the card's padding. */
#login label:first-of-type{margin-top:0}

dialog{border:1px solid var(--border);border-radius:var(--radius);max-width:560px;width:92%;
  background:var(--surface);color:var(--text);box-shadow:var(--shadow)}
dialog::backdrop{background:rgba(0,0,0,.45)}
dialog .card{border:none;box-shadow:none}
/* A row's "…" actions. The menu is fixed to the page, not inside the grid
   cell, which would clip it. */
button.more{padding:.2rem .55rem;font-size:1rem;line-height:1;letter-spacing:.08em}
.rowmenu{position:fixed;z-index:50;min-width:190px;padding:.3rem;background:var(--surface);color:var(--text);
  border:1px solid var(--border);border-radius:var(--radius);box-shadow:var(--shadow);display:flex;flex-direction:column}
.rowmenu button{display:block;width:100%;justify-content:flex-start;border:none;background:none;box-shadow:none;text-align:left;padding:.45rem .7rem;border-radius:var(--radius-sm);
  font-size:.88rem;font-weight:500;transform:none}
.rowmenu button:hover:not(:disabled),.rowmenu button:focus-visible{background:var(--surface-2);outline:none}
.rowmenu button.danger:hover:not(:disabled),.rowmenu button.danger:focus-visible{background:var(--bad-bg)}
.rowmenu button:disabled{color:var(--text-3);cursor:not-allowed}
.rowmenu hr{border:none;border-top:1px solid var(--border);margin:.3rem .2rem}
#deldlg .notice{margin:.4rem 0 1rem}

/* ---------- AG Grid, dressed in the house theme ---------- */
#route-grid{height:380px}
.ag-theme-quartz .ag-header-cell-label{text-transform:uppercase;letter-spacing:.05em;font-size:.7rem;font-weight:700}

@media (max-width:760px){
  .content{padding:1.1rem 1rem 2.5rem}
  h1{font-size:1.35rem}
  .topbar{padding:.5rem .8rem}
  .who span{display:none}
}
</style>
</head>
<body>
<div class="shell noauth" id="shell">
<div class="topbar">
  <span class="brand"><span class="grad-text spark" aria-hidden>&#10022;</span><span>huntwell</span><span class="tag">admin</span></span>
  <span class="who">
    <button class="iconbtn" id="theme" onclick="flipTheme()" title="Toggle theme" aria-label="Toggle theme"></button>
    <span id="who"></span>
  </span>
</div>
<!-- The rail: one tile per page, the same tiles the app draws. -->
<aside class="rail" id="rail">
  <nav class="rail-nav">
    <a class="rail-item" href="#/home" data-page="home" title="Home"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m3 10.5 9-7.5 9 7.5"/><path d="M5 9.5V21h14V9.5"/></svg></span><span class="rlbl">Home</span></a>
    <a class="rail-item" href="#/users" data-page="users" title="Users"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg></span><span class="rlbl">Users</span></a>
    <a class="rail-item" href="#/models" data-page="models" title="Models"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M15 2v2M9 2v2M15 20v2M9 20v2M2 15h2M2 9h2M20 15h2M20 9h2"/></svg></span><span class="rlbl">Models</span></a>
    <a class="rail-item" href="#/routing" data-page="routing" title="Routing"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="19" r="3"/><path d="M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15"/><circle cx="18" cy="5" r="3"/></svg></span><span class="rlbl">Routing</span></a>
    <a class="rail-item" href="#/logs" data-page="logs" title="Logs"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 21h12a2 2 0 0 0 2-2v-2H10v2a2 2 0 1 1-4 0V5a2 2 0 1 0-4 0v3h4"/><path d="M19 17V5a2 2 0 0 0-2-2H4"/><path d="M15 8h-5M15 12h-5"/></svg></span><span class="rlbl">Logs</span></a>
  </nav>
  <div class="spacer"></div>
  <button class="rail-item" onclick="logout()" title="Sign out"><span class="tile"><svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/></svg></span><span class="rlbl">Sign out</span></button>
</aside>
<div class="pane">

<div id="login" class="card">
  <h2>Sign in</h2>
  <div class="sub">The operator console for this Huntwell installation.</div>
  <label for="li-email">Email</label><input id="li-email" type="email" autocomplete="username" onkeydown="if(event.key==='Enter')login()">
  <label for="li-pass">Password</label><input id="li-pass" type="password" autocomplete="current-password" onkeydown="if(event.key==='Enter')login()">
  <div id="li-err" class="notice" role="alert" hidden></div>
  <div class="row" style="margin-top:1rem"><button class="primary" id="li-btn" onclick="login()">Sign in</button></div>
</div>

<!-- Shown instead of sign-in when the control plane has no operator yet. The
     key comes from the banner the process printed at boot, so whoever can read
     the server's output claims the install. -->
<div id="firstrun" class="card hide">
  <h2>Claim this control plane</h2>
  <div class="sub">There is no operator yet. The setup key was printed in this server&rsquo;s log when it started.</div>
  <label for="fr-key">Setup key</label>
  <input id="fr-key" autocomplete="off" spellcheck="false" placeholder="XXXXX-XXXXX-XXXXX-XXXXX"
         style="font-family:ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.06em;text-transform:uppercase">
  <label for="fr-email">Your email</label><input id="fr-email" type="email" autocomplete="username">
  <label for="fr-pass">Choose a password</label>
  <input id="fr-pass" type="password" autocomplete="new-password">
  <div class="sub" style="margin-top:.35rem">At least 12 characters.</div>
  <div class="row" style="margin-top:1rem"><button class="primary" onclick="claim()">Create operator</button>
  </div>
  <div id="fr-err" class="notice" role="alert" hidden></div>
</div>

<main id="app" class="content hide">
<section class="page" data-page="home">
  <div class="page-head">
    <div>
      <h1>Fleet</h1>
      <div class="sub">Service heartbeats, hosts, slots and where every search plan ran.</div>
    </div>
  </div>

  <div class="row" id="overview"></div>

  <div class="card">
    <div class="card-head">
      <div><h2>Services</h2>
        <div class="sub">Last heartbeat on the bus. A process that misses three is stale.</div></div>
      <span id="services-bus"></span>
    </div>
    <table><thead><tr><th>Service</th><th>Status</th><th>Last ping</th><th>Instances</th></tr></thead>
    <tbody id="services"></tbody></table>
  </div>

  <div class="card">
    <div class="card-head">
      <div><h2>Hosts</h2><div class="sub">Bare-metal Incus servers. Each carries a few VMs, so a compromise stops at one.</div></div>
      <button onclick="newHost()">&#65291; Add host</button>
    </div>
    <table><thead><tr><th>Host</th><th>Machine</th><th>VMs</th><th>Worker slots</th><th>Status</th><th></th></tr></thead>
    <tbody id="hosts"></tbody></table>
  </div>

  <div class="card">
    <div class="card-head">
      <div><h2>VMs</h2><div class="sub" id="vms-sub">The whole application: one app VM, and worker VMs that execute plans.</div></div>
      <div class="row">
        <button onclick="newVm('worker')">&#65291; Worker VM</button>
        <button onclick="newVm('app')">&#65291; App VM</button>
        <button class="primary" onclick="deployAll()">Deploy all</button>
      </div>
    </div>
    <table><thead><tr><th>VM</th><th>Role</th><th>Host</th><th>Size</th><th>Build</th><th>Status</th><th></th></tr></thead>
    <tbody id="vms"></tbody></table>
  </div>
</section>

<section class="page hide" data-page="users">
  <div class="page-head"><div><h1>Users</h1><div class="sub">Who may join, every account, what each may build, and connected logins.</div></div></div>
  <div class="card">
    <div class="card-head">
      <div><h2>Waitlist &amp; invites</h2>
        <div class="sub" id="wl-sub">Huntwell is invite-only. People who ask to join wait here; an invitation emails them a sign-up link for their address, good for 14 days.</div></div>
    </div>
    <div class="row" style="align-items:flex-end">
      <div style="flex:2;min-width:220px"><label for="wl-email">Invite someone</label>
        <input id="wl-email" type="email" placeholder="name@company.com" autocomplete="off" onkeydown="if(event.key==='Enter')inviteSomeone()"></div>
      <div style="flex:1;min-width:160px"><label for="wl-name">Name <span class="muted">(optional)</span></label>
        <input id="wl-name" autocomplete="off" maxlength="80" onkeydown="if(event.key==='Enter')inviteSomeone()"></div>
      <button class="primary" id="wl-btn" onclick="inviteSomeone()">Send invitation</button>
    </div>
    <div id="wl-note" class="notice" role="status" hidden style="margin-top:.8rem"></div>
    <div id="waitlist" class="card pad0 hw-grid ag-theme-quartz" style="height:420px;margin-top:1rem"></div>
  </div>
  <div class="card">
    <div class="card-head">
      <div><h2>What people can build</h2>
        <div class="sub">The default for every account. Reports and files are experimental.</div></div>
    </div>
    <div class="row" id="features"></div>
    <div class="row" style="margin-top:.8rem">
      <button class="primary sm" onclick="saveFeatures()">Save default</button>
      <span id="features-note" class="muted"></span>
    </div>
    <table style="margin-top:1.2rem"><thead><tr><th>Account</th><th>Plans</th><th>Can build</th><th>Connected logins</th><th>2FA</th><th title="What this workspace is charged per million billable tokens">Rate $/M tokens</th><th title="Spendable credit in this workspace's wallet">Credits</th><th></th></tr></thead>
    <tbody id="accounts"></tbody></table>
  </div>
</section>

<section class="page hide" data-page="models">
  <div class="page-head"><div><h1>Models</h1><div class="sub">Which model each stage of a run uses.</div></div></div>
  <div class="card">
    <div class="card-head">
      <div><h2>Models</h2>
        <div class="sub">Applies to plans drafted and executions started from now on.</div></div>
    </div>
    <table><thead><tr><th style="width:9rem">Stage</th><th>Model</th><th>What it does</th></tr></thead>
    <tbody id="models"></tbody></table>
    <div class="row" style="margin-top:.8rem">
      <button class="primary sm" onclick="saveModels()">Save models</button>
      <span id="models-note" class="muted"></span>
    </div>
  </div>
  <div class="card" style="margin-top:1rem">
    <div class="card-head"><div><h2>Model providers</h2>
      <div class="sub">Where a <code>provider:model</code> stage runs. The keys live in Secrets Manager; only whether each is set is shown here.</div></div></div>
    <table><thead><tr><th>Provider</th><th>Status</th><th>Setting</th><th></th></tr></thead>
    <tbody id="providers"></tbody></table>
  </div>
</section>


<section class="page hide" data-page="routing">
  <div class="page-head"><div><h1>Routing</h1><div class="sub">How queued executions are placed, and every decision made.</div></div></div>
  <div class="card">
    <div class="card-head">
      <div><h2>Strategy</h2><div class="sub">How a queued execution picks its slot.</div></div>
    </div>
    <div class="row">
      <label class="row" style="margin:0"><input type="radio" name="strat" value="round_robin"> Round-robin</label>
      <label class="row" style="margin:0"><input type="radio" name="strat" value="random"> Random</label>
      <label class="row" style="margin:0"><input type="radio" name="strat" value="pinned"> Pin to slot:</label>
      <select id="pin" style="width:auto;min-width:220px"></select>
      <button class="primary sm" onclick="saveRouting()">Save</button>
      <span id="routing-note" class="muted"></span>
    </div>
  </div>

  <div class="card">
    <div class="card-head">
      <div><h2>Routing log</h2>
        <div class="sub">Every placement, re-queue and reap. Sortable and filterable.</div></div>
    </div>
    <div id="route-grid" class="card pad0 hw-grid ag-theme-quartz"></div>
  </div>
</section>

<section class="page hide" data-page="logs">
  <div class="page-head"><div><h1>Logs</h1><div class="sub">Recent executions across every workspace.</div></div></div>
  <div class="card">
    <div class="card-head">
      <div><h2>Recent executions</h2><div class="sub">The last 25, newest first. Click a run to read everything it printed.</div></div>
    </div>
    <div id="executions" class="card pad0 hw-grid ag-theme-quartz" style="height:640px"></div>
  </div>
</section>
</main>
</div>

<dialog id="reg">
  <div class="card" style="border:none">
    <h2 id="reg-title">Add host</h2>
    <div class="sub" id="reg-help">On the server, as root, then paste the token below:
      <pre class="mono" style="margin:.5rem 0;white-space:pre-wrap">sys incus init --app huntwell --allow &lt;postgres-ip&gt;:5432
sys incus image --app huntwell
sys incus token</pre></div>
    <input type="hidden" id="h-id">
    <div class="grid2">
      <div><label>Name <span class="muted" style="font-weight:400">(its Incus remote)</span></label><input id="h-name" placeholder="yak-02"></div>
      <div><label>Incus API</label><input id="h-endpoint" placeholder="https://10.0.0.3:8443"></div>
    </div>
    <div id="h-token-box"><label>Trust token <span class="muted" style="font-weight:400">(from sys incus token — used once, never stored)</span></label>
      <input id="h-token" autocomplete="off" spellcheck="false"></div>
    <div class="grid2">
      <div><label>Status</label><select id="h-status"><option>Active</option><option>Draining</option><option>Offline</option></select></div>
      <div><label>Max VMs <span class="muted" style="font-weight:400">(0 = no limit)</span></label><input id="h-maxvms" type="number" min="0" value="3"></div>
      <div><label>Priority <span class="muted" style="font-weight:400">(lower first)</span></label><input id="h-priority" type="number" value="100"></div>
      <div><label>Image</label><input id="h-image" value="huntwell"></div>
    </div>
    <h3 style="margin:1rem 0 0">New VMs here get</h3>
    <div class="grid2">
      <div><label>vCPU</label><input id="h-cpu" type="number" min="1" value="8"></div>
      <div><label>Memory</label><input id="h-memory" value="16GiB"></div>
      <div><label>Disk</label><input id="h-disk" value="60GiB"></div>
      <div><label>Plan slots (workers)</label><input id="h-slots" type="number" min="1" max="50" value="10"></div>
    </div>
    <h3 style="margin:1rem 0 0">Edge <span class="muted" style="font-weight:400;font-size:.85rem">— only used on the host carrying the app VM</span></h3>
    <div class="grid2">
      <div><label>Domain</label><input id="h-domain" placeholder="app.yourdomain.com"></div>
      <div><label>Scheme</label><select id="h-edge"><option value="https">https — Caddy gets certificates</option>
        <option value="http">http — private network</option>
        <option value="cloudflare">cloudflare — tunnel, nothing published</option></select></div>
      <div><label>Port <span class="muted" style="font-weight:400">(0 = default)</span></label><input id="h-port" type="number" min="0" value="0"></div>
    </div>
    <label class="row" style="margin-top:.8rem"><input id="h-on" type="checkbox" checked> Enabled</label>
    <label>Notes</label><input id="h-notes">
    <div class="row" style="margin-top:1rem">
      <button class="primary" id="h-save" onclick="saveHost()">Add host</button>
      <button onclick="document.getElementById('reg').close()">Cancel</button>
      <span id="h-err" class="muted"></span>
    </div>
  </div>
</dialog>

<dialog id="logdlg" style="max-width:min(1100px,94vw);width:100%">
  <div class="card" style="border:none">
    <div class="card-head">
      <div><h2 id="log-title">Execution</h2><div class="sub" id="log-sub"></div></div>
      <div class="row">
        <label class="muted" style="font-size:.85rem"><input type="checkbox" id="log-errors-only" onchange="renderLog()"> problems only</label>
        <button class="sm" onclick="copyLog(false)" id="log-copy-all">Copy whole log</button>
        <button class="sm" onclick="copyLog(true)" id="log-copy-shown" hidden>Copy shown</button>
        <button class="sm" onclick="downloadLog()">Download</button>
        <span id="log-copied" class="muted" style="font-size:.85rem"></span>
        <button class="sm" onclick="document.getElementById('logdlg').close()">Close</button>
      </div>
    </div>
    <pre id="log-body" class="mono" style="max-height:62vh;overflow:auto;white-space:pre-wrap;word-break:break-word;font-size:.8rem;line-height:1.5;margin:0"></pre>
  </div>
</dialog>

<dialog id="modeldlg" style="max-width:min(640px,94vw)">
  <div class="card">
    <h2 id="model-dlg-title">Choose a model</h2>
    <div class="sub" id="model-dlg-help" style="margin:.2rem 0 .7rem">Filter by name, provider or id.</div>
    <input id="model-q" type="search" placeholder="Filter models…" autocomplete="off" oninput="renderModelList()"
      onkeydown="if(event.key==='Enter'){const b=$('model-list').querySelector('.model-opt');if(b){event.preventDefault();pickModel(b.dataset.id)}}">
    <div id="model-list" class="model-list" onclick="if(event.target.closest('.model-opt'))pickModel(event.target.closest('.model-opt').dataset.id)"></div>
    <div class="row" style="margin-top:1rem">
      <button onclick="document.getElementById('modeldlg').close()">Cancel</button>
    </div>
  </div>
</dialog>

<dialog id="creditdlg">
  <div class="card" style="border:none">
    <h2>Add free credit</h2>
    <div class="sub" id="cr-who"></div>
    <div class="grid2" style="margin-top:.8rem">
      <div><label for="cr-usd">Amount (USD)</label>
        <input id="cr-usd" type="number" min="0.01" max="10000" step="0.01" value="100" oninput="creditTyped()"></div>
      <div><label>&nbsp;</label><div class="row" style="gap:.35rem">
        <button class="sm" onclick="$('cr-usd').value=25;creditTyped()">$25</button>
        <button class="sm" onclick="$('cr-usd').value=50;creditTyped()">$50</button>
        <button class="sm" onclick="$('cr-usd').value=100;creditTyped()">$100</button></div></div>
    </div>
    <label for="cr-note">Note to them <span class="muted">(optional — goes in the email)</span></label>
    <textarea id="cr-note" rows="3" maxlength="500" style="font-family:var(--font)" placeholder="Your first $100 is on us — welcome to Huntwell."></textarea>
    <div class="muted" style="font-size:.85rem;margin-top:.5rem">Free — no card is charged. They get an email saying it was added and their new balance.</div>
    <div class="row" style="margin-top:1rem">
      <button class="primary" id="cr-btn" onclick="grantCredit()">Add credit</button>
      <button onclick="$('creditdlg').close()">Cancel</button>
      <span id="cr-err" class="muted"></span>
    </div>
  </div>
</dialog>

<dialog id="deldlg">
  <div class="card" style="border:none">
    <h2 id="del-title">Delete permanently?</h2>
    <div class="notice"><span class="ico">!</span><div id="del-warn"></div></div>
    <label for="del-confirm">Type <b id="del-email"></b> to confirm</label>
    <input id="del-confirm" autocomplete="off" spellcheck="false" oninput="delTyped()" onkeydown="if(event.key==='Enter'&&!$('del-btn').disabled)doDelete()">
    <div class="row" style="margin-top:1rem">
      <button class="danger" id="del-btn" disabled onclick="doDelete()">Delete forever</button>
      <button onclick="$('deldlg').close()">Cancel</button>
      <span id="del-err" class="muted"></span>
    </div>
  </div>
</dialog>

<dialog id="vmdlg">
  <div class="card" style="border:none">
    <h2 id="vm-title">New worker VM</h2>
    <div class="sub" id="vm-help"></div>
    <input type="hidden" id="vm-role">
    <div class="grid2">
      <div><label>Host</label><select id="vm-host"></select></div>
      <div id="vm-slots-box"><label>Plan slots</label><input id="vm-slots" type="number" min="1" max="50" placeholder="host default"></div>
      <div><label>vCPU</label><input id="vm-cpu" type="number" min="1" placeholder="host default"></div>
      <div><label>Memory</label><input id="vm-memory" placeholder="host default"></div>
      <div><label>Disk</label><input id="vm-disk" placeholder="host default"></div>
    </div>
    <div class="row" style="margin-top:1rem">
      <button class="primary" onclick="saveVm()">Provision</button>
      <button onclick="document.getElementById('vmdlg').close()">Cancel</button>
      <span id="vm-err" class="muted"></span>
    </div>
  </div>
</dialog>

<script>
const $=id=>document.getElementById(id);

// Theme, matching the product app: a stored choice wins, otherwise the OS.
// The icon shows what you would switch TO, which is the convention the app uses.
const SUN='<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>';
const MOON='<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>';
function paintTheme(){const dark=document.documentElement.dataset.theme==='dark';
  const b=$('theme');if(b){b.innerHTML=dark?SUN:MOON;
    b.title=dark?'Switch to light mode':'Switch to dark mode'}}
function flipTheme(){const next=document.documentElement.dataset.theme==='dark'?'light':'dark';
  document.documentElement.dataset.theme=next;
  try{localStorage.setItem('huntwell.theme',next)}catch(e){}
  paintTheme()}
paintTheme();
const api=async(m,u,b)=>{let r;try{r=await fetch(u,{method:m,headers:{'content-type':'application/json'},
  body:b?JSON.stringify(b):undefined})}catch(x){const e=new Error('network');e.status=0;throw e}
  if(!r.ok){const e=new Error((await r.text())||String(r.status));e.status=r.status;throw e}return r.json()};

// What a failed form submission says. The API answers in its own words
// ({"error":"bad credentials"}); a person at a sign-in form needs the reason
// and what to do about it, in a card, not a JSON fragment beside a button.
function explain(e,what){
  const raw=errText(e)||'';
  if(e.status===0)return {title:'Can\u2019t reach the control plane.',text:'Check that the admin service is running and that you are on its network or VPN.'};
  if(e.status===401)return {title:'That email and password don\u2019t match.',text:'Check both and try again. Operator accounts are separate from Huntwell sign-ins.'};
  if(e.status===429)return {title:'Too many attempts.',text:raw.replace(/^too many attempts\s*[—-]\s*/i,'')||'Wait a few minutes before trying again.'};
  if(e.status===409)return {title:'This control plane already has an operator.',text:'Sign in with that account instead.'};
  if(e.status>=500)return {title:'The control plane hit an error.',text:raw||'See its log for the reason.'};
  return {title:what||'That didn\u2019t work.',text:raw};
}
function showNotice(id,msg){const n=$(id);n.hidden=false;
  n.innerHTML=`<span class="ico">!</span><span><b>${esc(msg.title)}</b>${msg.text?' '+esc(msg.text):''}</span>`}
function hideNotice(id){const n=$(id);n.hidden=true;n.textContent=''}

async function login(){hideNotice('li-err');const b=$('li-btn');b.disabled=true;b.textContent='Signing in\u2026';
  try{await api('POST','/admin/api/login',{email:$('li-email').value.trim(),password:$('li-pass').value});boot()}
  catch(e){showNotice('li-err',explain(e));$('li-pass').focus();$('li-pass').select()}
  finally{b.disabled=false;b.textContent='Sign in'}}
async function logout(){await api('POST','/admin/api/logout');location.reload()}

// ---- grids -----------------------------------------------------------------
// Every table of data in here is an AG Grid, so sorting, filtering and column
// sizing work the same way wherever you are. One helper rather than a setup
// per grid: the options that make them look and behave alike live once.
const GRIDS={};
function mkGrid(id,columnDefs,opts){
  if(GRIDS[id])return GRIDS[id];
  GRIDS[id]=agGrid.createGrid($(id),Object.assign({
    columnDefs,
    defaultColDef:{sortable:true,resizable:true,minWidth:80},
    rowData:[],animateRows:false,suppressCellFocus:true,headerHeight:38,rowHeight:40,
  },opts||{}));
  return GRIDS[id];
}
function setRows(id,rows){if(GRIDS[id])GRIDS[id].setGridOption('rowData',rows)}

let routeApi=null;
function eventBadge(p){const cls=p.value==='routed'?'ok':p.value==='reaped'?'bad':'warn';
  return `<span class="badge ${cls}">${p.value}</span>`}
function initGrid(){
  routeApi=agGrid.createGrid($('route-grid'),{
    columnDefs:[
      {field:'created_at',headerName:'Time',width:170,sort:'desc',
        valueFormatter:p=>p.value?new Date(p.value).toLocaleString():''},
      {field:'event',headerName:'Event',width:110,cellRenderer:eventBadge,filter:true},
      {field:'execution_id',headerName:'Run',width:90,valueFormatter:p=>'#'+p.value},
      {field:'source',headerName:'Search plan',flex:1,minWidth:160,filter:true},
      {field:'account_id',headerName:'Acct',width:80},
      {field:'host_name',headerName:'Host',width:130,filter:true,
        valueGetter:p=>p.data.host_name??(p.data.host_id!=null?('host '+p.data.host_id):'—')},
      {field:'slot_name',headerName:'Slot',width:120,filter:true,valueFormatter:p=>p.value??'—',
        cellClass:'mono'},
      {field:'detail',headerName:'Detail',flex:1,minWidth:150},
    ],
    defaultColDef:{sortable:true,resizable:true},
    rowData:[],animateRows:false,suppressCellFocus:true,
    overlayNoRowsTemplate:'<span class="muted">No routing decisions yet — start a search with RUN_DISPATCH=pool.</span>',
  });
}

async function claim(){hideNotice('fr-err');
  try{
    await api('POST','/admin/api/claim',{
      setupKey:$('fr-key').value,email:$('fr-email').value,password:$('fr-pass').value});
    boot();
  }catch(e){showNotice('fr-err',e.status===401?{title:'That setup key is not right.',text:'Copy it from the box the admin printed when it started; a restart mints a new one.'}:explain(e,'Could not create the operator.'))}}

// ---- pages: one per rail tile, picked by the hash so a reload stays put ----
const PAGES=['home','users','models','routing','logs'];
function currentPage(){const p=(location.hash||'').replace(/^#\/?/,'');return PAGES.includes(p)?p:'home'}
let lastLog=[];
function showPage(name){
  document.querySelectorAll('.page').forEach(el=>el.classList.toggle('hide',el.dataset.page!==name));
  document.querySelectorAll('.rail-item[data-page]').forEach(el=>el.classList.toggle('active',el.dataset.page===name));
  // The grid measures itself when it is made, so it is made the first time
  // its page is on screen, not while hidden at zero width.
  if(name==='routing'&&!routeApi&&window.agGrid){initGrid();routeApi.setGridOption('rowData',lastLog)}
  if(name==='users'&&window.agGrid){initWaitlist();loadWaitlist()}
  $('app').parentElement.scrollTop=0;
}
window.addEventListener('hashchange',()=>{if(!$('app').classList.contains('hide'))showPage(currentPage())});

async function boot(){
  const s=await api('GET','/admin/api/session');
  if(!s.email){
    $('shell').classList.add('noauth');
    // An unclaimed control plane has nothing to sign in to, so offer the one
    // thing that can be done instead of a form that cannot succeed.
    const first=!!s.setupRequired;
    $('firstrun').classList.toggle('hide',!first);
    $('login').classList.toggle('hide',first);
    $('app').classList.add('hide');
    if(first)$('fr-key').focus();
    return;
  }
  $('firstrun').classList.add('hide');
  $('login').classList.add('hide');$('app').classList.remove('hide');
  $('shell').classList.remove('noauth');
  $('who').innerHTML=`<span>${esc(s.email)}</span>`;
  showPage(currentPage());
  loadModels();
  loadFeatures();
  refresh();clearInterval(window._t);window._t=setInterval(refresh,5000);
}

function esc(s){return String(s??'').replace(/[&<>"']/g,c=>({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]))}
function ago(iso){
  if(!iso)return 'never';
  const s=Math.max(0,Math.round((Date.now()-new Date(iso).getTime())/1000));
  if(s<5)return 'just now';
  if(s<60)return s+'s ago';
  if(s<3600)return Math.floor(s/60)+'m ago';
  return Math.floor(s/3600)+'h ago';
}
function paintServices(sv){
  $('services-bus').innerHTML=sv.bus&&sv.bus.connected
    ?'<span class="badge ok">bus connected</span>'
    :'<span class="badge bad">bus down</span>';
  $('services').innerHTML=sv.services.map(s=>{
    const cls=s.status==='ok'?'ok':s.status==='stale'?'warn':'bad';
    const newest=s.instances.slice().sort((a,b)=>a.age_seconds-b.age_seconds)[0];
    const inst=s.instances.map(i=>`<span class="badge ${i.status==='ok'?'ok':'warn'} mono" title="${esc(i.last_ping)}">${esc(i.id)}</span>`).join(' ')
      ||'<span class="muted">no ping yet</span>';
    return `<tr><td><b>${esc(s.name)}</b></td>
      <td><span class="badge ${cls}">${esc(s.status)}</span></td>
      <td class="muted">${newest?ago(newest.last_ping):'never'}</td>
      <td>${inst}</td></tr>`;
  }).join('')||'<tr><td colspan="4" class="muted">No services listed.</td></tr>';
}

async function refresh(){try{
  const [o,sv]=await Promise.all([
    api('GET','/admin/api/overview'),
    api('GET','/admin/api/services'),
  ]);
  const up=(sv.services||[]).filter(s=>s.status==='ok').length;
  const total=(sv.services||[]).length;
  $('overview').innerHTML=[['Services',`${up}/${total}`],['Hosts',`${o.hosts_healthy}/${o.hosts}`],['Slots ready',o.slots_ready],
    ['Busy',o.slots_busy],['Waiting',o.queued_unplaced]].map(([l,n])=>
    `<div class="stat"><div class="n">${n}</div><div class="l">${l}</div></div>`).join('');
  paintServices(sv);
  const hs=await api('GET','/admin/api/hosts');
  HOSTS=hs.hosts;
  const rows=await Promise.all(hs.hosts.map(async x=>{
    const h=x.host;const slots=(await api('GET',`/admin/api/hosts/${h.host_id}/slots`)).slots;
    // Slots, grouped by the VM that runs them. A VM with every slot down is the
    // thing worth seeing, and one flat list of forty badges hides it.
    const byVm={};slots.forEach(p=>{(byVm[p.node||'local']=byVm[p.node||'local']||[]).push(p)});
    const slotHtml=Object.entries(byVm).map(([vm,ps])=>{
      const up=ps.filter(p=>p.ready).length,busy=ps.filter(p=>p.execution_id).length;
      const detail=ps.map(p=>`<span class="badge ${p.ready?(p.execution_id?'warn':'ok'):'bad'}" title="${esc(p.name)}${p.ready?'':' · down'}">${esc(p.name.slice(vm.length+1)||p.name)}${p.execution_id?` · run ${p.execution_id}`:''}
        <a href="#" onclick="killSlot(${h.host_id},'${esc(p.name)}');return false" title="kill slot">✕</a></span>`).join(' ');
      return `<details><summary><b>${esc(vm)}</b> <span class="muted">${up}/${ps.length} up · ${busy} busy</span></summary>${detail}</details>`;
    }).join('')||'<span class="muted">no worker slots</span>';
    const vmHtml=(x.vms||[]).map(v=>`<span class="badge ${vmBadge(v.status)}" title="${esc(v.role)} · ${esc(v.status)}">${esc(v.name)}</span>`).join(' ')
      ||'<span class="muted">none</span>';
    const machine=x.local?'<span class="muted">this admin\'s process pool</span>'
      :(h.cpu_total?`${esc(h.arch)} · ${h.cpu_total} vCPU · ${Math.round(h.memory_total_mb/1024)} GB<br><span class="muted mono">incus ${esc(h.incus_version)}</span>`:'<span class="muted">not checked yet</span>');
    const cls=h.status==='Active'?'ok':h.status==='Draining'?'warn':'bad';
    const st=`<span class="badge ${h.enabled?cls:'warn'}">${h.enabled?esc(h.status):'disabled'}</span>`+
      (h.last_error?`<br><span class="muted" style="font-size:.75rem" title="${esc(h.last_error)}">${esc(h.last_error.slice(0,140))}</span>`:'');
    return `<tr><td><b>${esc(h.name)}</b><br><span class="muted mono">${x.local?'local':esc(h.endpoint)}</span></td>
      <td>${machine}</td><td>${vmHtml}${x.local?'':`<br><span class="muted" style="font-size:.75rem">${(x.vms||[]).length}/${h.max_vms||'∞'}</span>`}</td>
      <td style="min-width:14rem">${slotHtml}</td><td>${st}</td>
      <td class="row">${x.local?'':`<button class="sm" onclick="editHost(${h.host_id})">Edit</button>`}
      <button class="sm" onclick="sync(${h.host_id})">Check</button>
      <button class="sm danger" onclick="delHost(${h.host_id},'${esc(h.name)}')">Delete</button></td></tr>`}));
  $('hosts').innerHTML=rows.join('')||'<tr><td colspan="6" class="muted">No hosts yet. Set up a server with <span class="mono">sys incus init --app huntwell</span>, then Add host.</td></tr>';

  const vs=await api('GET','/admin/api/vms');
  $('vms-sub').innerHTML=`The whole application: one app VM, and worker VMs that execute plans. Deploys come from <span class="mono">${esc(vs.build_dir)}</span>.`;
  $('vms').innerHTML=vs.vms.map(({vm:v,host,busy})=>`<tr>
    <td><b>${esc(v.name)}</b>${v.address?`<br><span class="muted mono">${esc(v.address)}</span>`:''}</td>
    <td>${v.role==='app'?'<span class="badge ok">app</span>':`worker · ${v.slots} slots`}</td>
    <td>${esc(host)}</td><td class="muted">${v.cpu} vCPU · ${esc(v.memory)} · ${esc(v.disk)}</td>
    <td class="mono">${esc(v.version)||'—'}</td>
    <td><span class="badge ${busy?'warn':vmBadge(v.status)}">${busy?'working…':esc(v.status)}</span>
      ${v.last_error?`<br><span class="muted" style="font-size:.75rem" title="${esc(v.last_error)}">${esc(v.last_error.slice(0,160))}</span>`:''}</td>
    <td class="row">
      <button class="sm" ${busy?'disabled':''} onclick="vmAction(${v.vm_id},'deploy')">Deploy</button>
      ${v.role==='worker'?`<button class="sm" ${busy?'disabled':''} onclick="setSlots(${v.vm_id},${v.slots})">Slots</button>`:''}
      ${v.status==='Stopped'?`<button class="sm" ${busy?'disabled':''} onclick="vmAction(${v.vm_id},'start')">Start</button>`
        :`<button class="sm" ${busy?'disabled':''} onclick="vmAction(${v.vm_id},'stop')">Stop</button>`}
      ${v.status==='Failed'?`<button class="sm" ${busy?'disabled':''} onclick="reprovision(${v.vm_id},'${v.role}',${v.host_id})">Retry</button>`:''}
      <button class="sm danger" ${busy?'disabled':''} onclick="delVm(${v.vm_id},'${esc(v.name)}')">Delete</button></td></tr>`).join('')
    ||'<tr><td colspan="7" class="muted">No VMs yet. Add a host, then provision the app VM and a worker VM.</td></tr>';
  const rt=await api('GET','/admin/api/routing');
  document.querySelectorAll('[name=strat]').forEach(r=>r.checked=r.value===rt.strategy);
  const cur=rt.pinned_host_id&&rt.pinned_slot?`${rt.pinned_host_id}:${rt.pinned_slot}`:'';
  $('pin').innerHTML=rt.free_slots.map(p=>`<option value="${esc(p.host_id)}:${esc(p.slot)}" ${cur===`${p.host_id}:${p.slot}`?'selected':''}>host ${esc(p.host_id)} · ${esc(p.slot)}</option>`).join('')
    ||`<option value="">${cur?esc(cur)+' (busy/offline)':'no free slots'}</option>`;
  const lg=await api('GET','/admin/api/route-log?limit=300');
  lastLog=lg.log;if(routeApi)routeApi.setGridOption('rowData',lg.log);
  const usd=m=>(m<0?'−':'')+'$'+(Math.abs(m)/1e6).toFixed(Math.abs(m)<1e6?3:2);
  window._runs={};
  const profitCell=m=>m==null?'<span class="muted">—</span>':`<span class="badge ${m>=0?'ok':'bad'}">${usd(m)}</span>`;
  const rs=await api('GET','/admin/api/executions?limit=25');
  rs.executions.forEach(r=>{window._runs[r.execution_id]=r});

  // Money right-aligned and sorted as numbers, not as the "$9.09" a reader
  // sees — sorting a currency string puts $9 above $80.
  const money=(field)=>({field,type:'rightAligned',width:120,
    valueFormatter:p=>p.data&&p.data.total?`<b>${usd(p.value)}</b>`:usd(p.value)});
  mkGrid('executions',[
    {field:'execution_id',headerName:'Run',width:90,sort:'desc',
      valueFormatter:p=>p.data&&p.data.total?'':'#'+p.value},
    {field:'source',headerName:'Search plan',flex:1.4,minWidth:150,filter:true,
      valueGetter:p=>p.data&&p.data.total?`Total of the ${p.data.runs} run(s) with a known cost`:p.data.source,
      cellClass:p=>p.data&&p.data.total?'muted':''},
    {field:'account_id',headerName:'Acct',width:80},
    {field:'status',headerName:'Status',width:110,filter:true,
      cellRenderer:p=>p.value?`<span class="badge ${p.value==='succeeded'?'ok':p.value==='failed'?'bad':'warn'}">${esc(p.value)}</span>`:''},
    {field:'model_scrape',headerName:'Model',flex:1,minWidth:130,filter:true,cellClass:'mono',
      valueFormatter:p=>p.data&&p.data.total?'':(p.value||'auto')},
    {headerName:'Charged',field:'charged_usd_micros',type:'rightAligned',width:110,
      cellRenderer:p=>p.data.total?`<b>${usd(p.value)}</b>`:usd(p.value)},
    {headerName:'Our cost',field:'cost_usd_micros',type:'rightAligned',width:110,
      // An estimate is marked, because the margin beside it is only as good.
      cellRenderer:p=>p.value==null?'—':(p.data.total?`<b>${usd(p.value)}</b>`
        :(p.data.cost_basis==='estimated'?'~':'')+usd(p.value)),
      tooltipValueGetter:p=>p.data.cost_basis==='estimated'?'Estimated from the tokens and this model\u2019s rates — the provider reported no cost':'What the provider reported for this run'},
    {headerName:'Profit',field:'profit_usd_micros',type:'rightAligned',width:110,cellRenderer:p=>profitCell(p.value)},
    {field:'host_id',headerName:'Host',width:90,valueFormatter:p=>p.value??'—'},
    {field:'slot_name',headerName:'Slot',flex:1,minWidth:120,cellClass:'mono',valueFormatter:p=>p.value??'—'},
    {field:'started_at',headerName:'Started',width:180,
      valueFormatter:p=>p.value?new Date(p.value).toLocaleString():''},
  ],{
    onRowClicked:e=>{if(e.data&&!e.data.total)openLog(e.data.execution_id)},
    rowClass:'clickable',
    overlayNoRowsTemplate:'<span class="muted">No runs yet.</span>',
  });
  setRows('executions',rs.executions);

  // The totals as a pinned row, so they stay put while the grid is sorted.
  const known=rs.executions.filter(r=>r.profit_usd_micros!=null);
  const sum=k=>known.reduce((a,r)=>a+r[k],0);
  GRIDS['executions'].setGridOption('pinnedBottomRowData',known.length?[{
    total:true,runs:known.length,
    charged_usd_micros:sum('charged_usd_micros'),
    cost_usd_micros:sum('cost_usd_micros'),
    profit_usd_micros:sum('profit_usd_micros'),
  }]:[]);
}catch(e){console.warn(e)}}

// ---- one run's log -------------------------------------------------------
// The operator's view: everything the run printed, stderr included. A
// customer's run page hides the raw lines behind a friendly feed, which is
// exactly what you do not want when working out why something failed.
let _log=[];
// A line worth jumping to. `stderr` alone is too broad — the guard and the
// trail both narrate there — so the wording the pipeline actually uses when
// something went wrong is what counts.
const LOG_BAD=/^\s*(!|✖)|\berror\b|\bfailed\b|panicked|refused|\bcould not\b|\bunavailable\b|exhausted/i;

async function openLog(id){
  const dlg=$('logdlg');
  $('log-title').textContent='Execution #'+id;
  const r=window._runs[id];
  $('log-sub').textContent=r?`${r.source} · account ${r.account_id} · ${r.status}`:'';
  $('log-body').textContent='Loading…';
  _log=[];
  dlg.showModal();
  try{
    const res=await api('GET',`/admin/api/executions/${id}/log`);
    _log=res.lines||[];
    // Open on "problems only" when there are any: a failed run is why you
    // clicked, and its reason is usually one line in three hundred.
    $('log-errors-only').checked=_log.some(l=>LOG_BAD.test(l.line));
    renderLog();
  }catch(e){$('log-body').textContent='Could not load the log: '+(e.message||e)}
}

function logLines(){
  return $('log-errors-only').checked ? _log.filter(l=>LOG_BAD.test(l.line)) : _log;
}

function renderLog(){
  const lines=logLines();
  $('log-copy-shown').hidden=lines.length===_log.length;
  $('log-copy-all').textContent=`Copy whole log (${_log.length})`;
  if(!lines.length){
    $('log-body').innerHTML=`<span class="muted">${_log.length?'No problems in this run’s '+_log.length+' log line(s).':'This run printed nothing.'}</span>`;
    return;
  }
  $('log-body').innerHTML=lines.map(l=>{
    const bad=LOG_BAD.test(l.line);
    const colour=bad?'var(--bad)':l.stream==='stderr'?'var(--text-2)':'inherit';
    const t=new Date(l.ts);
    const at=isNaN(t)?'':t.toLocaleTimeString();
    return `<span style="color:${colour}"><span class="muted">${esc(at)}</span>  ${esc(l.line)}</span>`;
  }).join('\n');
}

// The whole log by default: when a run has failed the thing you want is all
// of it, to paste somewhere. "Copy shown" is there for when the filter is on
// and the five lines that matter are all you want.
function logText(shownOnly){
  return (shownOnly?logLines():_log).map(l=>{
    const t=new Date(l.ts); const at=isNaN(t)?'':t.toISOString().replace('T',' ').slice(0,19);
    // The stream as a fixed tag, the way psql prints it. Not a marker
    // character: the lines carry their own, and two `!` in a row reads badly.
    return `${at} ${l.stream==='stderr'?'err':'out'}  ${l.line}`;
  }).join('\n');
}

// The admin is reached over plain HTTP on a private address, where
// `navigator.clipboard` does not exist — it is a secure-context API. Without a
// fallback the button did nothing at all and said nothing about it.
function toClipboard(text){
  if(navigator.clipboard&&window.isSecureContext){
    return navigator.clipboard.writeText(text).then(()=>true,()=>legacyCopy(text));
  }
  return Promise.resolve(legacyCopy(text));
}

function legacyCopy(text){
  const ta=document.createElement('textarea');
  ta.value=text;
  // Off-screen but focusable, and readonly so a phone keyboard stays down.
  ta.setAttribute('readonly','');
  ta.style.cssText='position:fixed;top:-1000px;opacity:0';
  document.body.appendChild(ta);
  ta.select();
  ta.setSelectionRange(0,text.length);
  let ok=false;
  try{ok=document.execCommand('copy')}catch(e){ok=false}
  document.body.removeChild(ta);
  return ok;
}

async function copyLog(shownOnly){
  const text=logText(shownOnly);
  const n=(shownOnly?logLines():_log).length;
  const ok=await toClipboard(text);
  const note=$('log-copied');
  note.textContent=ok?`Copied ${n} line(s)`:'Could not copy — use Download';
  note.style.color=ok?'var(--ok)':'var(--bad)';
  setTimeout(()=>{note.textContent=''},4000);
}

// Always available, whatever the browser allows: a big log is often easier to
// keep as a file than to paste anyway.
function downloadLog(){
  const id=$('log-title').textContent.replace(/\D+/g,'')||'run';
  const blob=new Blob([logText(false)],{type:'text/plain'});
  const a=document.createElement('a');
  a.href=URL.createObjectURL(blob);
  a.download=`huntwell-execution-${id}.log`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(()=>URL.revokeObjectURL(a.href),10000);
}

let HOSTS=[];
// Server text reaches the page escaped: last_error is Incus's own output, and
// an operator console that renders command output as HTML is a console an
// attacker who controls that output can script.
function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
function errText(e){try{return JSON.parse(e.message).error||e.message}catch(_){return e.message}}
function vmBadge(s){return s==='Running'?'ok':s==='Failed'?'bad':'warn'}

function fillHost(h){
  const v=(id,x)=>{$(id).value=x};
  v('h-id',h?h.host_id:'');v('h-name',h?h.name:'');v('h-endpoint',h?h.endpoint:'');v('h-token','');
  v('h-status',h?h.status:'Active');v('h-maxvms',h?h.max_vms:3);v('h-priority',h?h.priority:100);
  v('h-image',h?h.base_image:'huntwell');v('h-cpu',h?h.vm_cpu:8);v('h-memory',h?h.vm_memory:'16GiB');
  v('h-disk',h?h.vm_disk:'60GiB');v('h-slots',h?h.vm_slots:10);v('h-domain',h?h.ingress_domain:'');
  v('h-edge',h?h.edge_scheme:'https');v('h-port',h?h.edge_port:0);v('h-notes',h?h.notes:'');
  $('h-on').checked=h?h.enabled:true;
  // The name is the Incus remote and the token is used once: neither changes on edit.
  $('h-name').disabled=!!h;$('h-token-box').hidden=!!h;$('reg-help').hidden=!!h;
  $('reg-title').textContent=h?`Edit ${h.name}`:'Add host';$('h-save').textContent=h?'Save':'Add host';
  $('h-err').textContent='';document.getElementById('reg').showModal()}
function newHost(){fillHost(null)}
function editHost(id){const x=(HOSTS||[]).find(x=>x.host.host_id===id);if(x)fillHost(x.host)}

async function saveHost(){$('h-err').textContent='';const b=$('h-save');
  const body={name:$('h-name').value.trim(),endpoint:$('h-endpoint').value.trim(),token:$('h-token').value.trim(),
    enabled:$('h-on').checked,status:$('h-status').value,base_image:$('h-image').value.trim(),
    priority:+$('h-priority').value,max_vms:+$('h-maxvms').value,vm_cpu:+$('h-cpu').value,
    vm_memory:$('h-memory').value.trim(),vm_disk:$('h-disk').value.trim(),vm_slots:+$('h-slots').value,
    ingress_domain:$('h-domain').value.trim(),edge_scheme:$('h-edge').value,edge_port:+$('h-port').value,notes:$('h-notes').value};
  const id=$('h-id').value;
  // Registering trusts the host and checks it, which takes a few seconds.
  b.disabled=true;b.textContent=id?'Saving…':'Contacting host…';
  try{if(id)await api('PUT',`/admin/api/hosts/${id}`,body);else await api('POST','/admin/api/hosts',body);
    document.getElementById('reg').close();refresh()}
  catch(e){$('h-err').textContent=' '+errText(e)}
  finally{b.disabled=false;b.textContent=id?'Save':'Add host'}}

async function delHost(id,name){if(!confirm(`Remove host ${name}? Its VMs must already be deleted; this forgets the trust.`))return;
  try{await api('DELETE',`/admin/api/hosts/${id}`);refresh()}catch(e){alert(errText(e))}}
async function sync(id){try{await api('POST',`/admin/api/hosts/${id}/sync`)}catch(e){alert(errText(e))}refresh()}
async function killSlot(id,slot){if(!confirm(`Kill slot ${slot}? Its current run will fail and the slot restarts.`))return;
  try{await api('POST',`/admin/api/hosts/${id}/slots/${encodeURIComponent(slot)}/kill`);refresh()}catch(e){alert(errText(e))}}

function newVm(role){
  const hosts=HOSTS.filter(x=>!x.local).map(x=>x.host);
  if(!hosts.length){alert('Add a host first.');return}
  $('vm-role').value=role;$('vm-err').textContent='';
  $('vm-title').textContent=role==='app'?'New app VM':'New worker VM';
  $('vm-help').textContent=role==='app'
    ?'Runs the website, planning, scheduling, notification and the bus. One per installation; its host\'s edge routes the domain to it.'
    :'Runs plan slots, each executing one plan at a time. It reaches only the database and the internet.';
  // A worker can let placement choose; the app VM needs a named host because
  // that host's edge is what the public reaches.
  $('vm-host').innerHTML=(role==='worker'?'<option value="">least-loaded host</option>':'')+
    hosts.map(h=>`<option value="${h.host_id}">${esc(h.name)} (${esc(h.status)})</option>`).join('');
  $('vm-slots-box').hidden=role!=='worker';
  ['vm-slots','vm-cpu','vm-memory','vm-disk'].forEach(i=>$(i).value='');
  document.getElementById('vmdlg').showModal()}

async function saveVm(){$('vm-err').textContent='';
  const n=id=>$(id).value.trim()===''?undefined:+$(id).value, t=id=>$(id).value.trim()||undefined;
  const body={role:$('vm-role').value,host_id:$('vm-host').value?+$('vm-host').value:undefined,
    slots:n('vm-slots'),cpu:n('vm-cpu'),memory:t('vm-memory'),disk:t('vm-disk')};
  try{const r=await api('POST','/admin/api/vms',body);document.getElementById('vmdlg').close();
    alert(`${r.name} is provisioning. A VM takes a few minutes to boot — its status updates here.`);refresh()}
  catch(e){$('vm-err').textContent=' '+errText(e)}}

async function vmAction(id,what){
  const warn={stop:'Stop this VM? A worker\'s running plans fail; stopping the app VM takes the product offline.',
              deploy:'Deploy the build folder\'s current executables to this VM? Worker slots roll as their plans finish.'};
  if(warn[what]&&!confirm(warn[what]))return;
  try{await api('POST',`/admin/api/vms/${id}/${what}`);refresh()}catch(e){alert(errText(e))}}

async function setSlots(id,cur){const v=prompt('Plan slots for this worker (1–50). Removed slots finish their current plan first.',cur);
  if(v===null)return;try{await api('PUT',`/admin/api/vms/${id}/slots`,{slots:+v});refresh()}catch(e){alert(errText(e))}}

async function delVm(id,name){if(!confirm(`Delete ${name}? The VM and its disk are destroyed. Refused while a run is on it.`))return;
  try{await api('DELETE',`/admin/api/vms/${id}`);refresh()}catch(e){alert(errText(e))}}

// A failed VM is retried as a fresh provision on the same host, same shape.
async function reprovision(id,role,host){if(!confirm('Delete this failed VM and provision it again?'))return;
  try{await api('DELETE',`/admin/api/vms/${id}`);await api('POST','/admin/api/vms',{role,host_id:host});refresh()}
  catch(e){alert(errText(e))}}

async function deployAll(){if(!confirm('Deploy the build folder\'s executables to every running VM? Worker slots roll as their plans finish.'))return;
  try{const r=await api('POST','/admin/api/vms/deploy');alert(r.deploying.length?`Deploying: ${r.deploying.join(', ')}`:'No running VMs to deploy.');refresh()}
  catch(e){alert(errText(e))}}

const KIND_LABEL={prospects:'People and companies',artifacts:'Tables',report:'Written reports',assets:'Files to keep'};
const EXPERIMENTAL=['report','assets'];

async function loadFeatures(){
  const f=await api('GET','/admin/api/features');
  window._allKinds=f.all;
  $('features').innerHTML=f.all.map(k=>`<label class="row" style="margin:0">
    <input type="checkbox" data-kind="${esc(k)}" style="width:auto" ${f.kinds.includes(k)?'checked':''}>
    ${esc(KIND_LABEL[k]||k)}${EXPERIMENTAL.includes(k)?' <span class="badge warn">experimental</span>':''}</label>`).join('');
  const acc=await api('GET','/admin/api/accounts?limit=200'),as=acc.accounts,defRate=acc.default_usd_per_mtoken;
  $('accounts').innerHTML=as.map(a=>{
    // Blank means the installation's rate, shown as the placeholder so the
    // number an account actually pays is always on screen.
    const ownRate=a.sell_usd_per_mtoken;
    const rate=`<div class="row" style="gap:.35rem;flex-wrap:nowrap">
        <input type="number" min="0.0001" max="1000" step="0.01" data-rate="${esc(a.account_id)}" value="${ownRate==null?'':esc(ownRate)}"
          placeholder="${esc(defRate)}" style="width:6.5rem" onkeydown="if(event.key==='Enter')saveRate(${Number(a.account_id)})">
        <button class="sm" onclick="saveRate(${Number(a.account_id)})">Set</button></div>
      <span class="muted" style="font-size:.78rem">${ownRate==null?`default · $${esc(defRate)}`:'own rate'}</span>`;
    const own=(a.kinds||'').split(',').filter(Boolean);
    // No list of their own means they follow the default, and should keep
    // following it when it changes — so that state is shown, not resolved away.
    const boxes=f.all.map(k=>`<label class="row" style="margin:0;display:inline-flex">
      <input type="checkbox" data-acct="${esc(a.account_id)}" data-kind="${esc(k)}" style="width:auto"
        ${own.length?(own.includes(k)?'checked':''):(f.kinds.includes(k)?'checked':'')}> ${esc(KIND_LABEL[k]||k)}</label>`).join(' ');
    // Its own column and its own switch, not one of the "can build" boxes:
    // those pick which kinds of plan exist, while this hands a browser a
    // customer's real credentials. Off for everyone until an operator says so.
    const cl=a.connected_logins
      ?`<span class="badge ok">on</span> <button class="sm" onclick="setConnectedLogins(${a.account_id},false)">Turn off</button>`
      :`<span class="muted">off</span> <button class="sm" onclick="setConnectedLogins(${a.account_id},true)">Turn on</button>`;
    return `<tr><td><b>${esc(a.email)}</b><br><span class="muted">#${esc(a.account_id)}${a.display_name?' · '+esc(a.display_name):''}</span></td>
      <td>${esc(a.plans)}</td>
      <td>${boxes}<br><span class="muted" style="font-size:.78rem">${own.length?'set for this account':'following the default'}</span></td>
      <td class="row">${cl}</td>
      <td>${a.mfa_enabled?`<span class="badge ok">on</span> <button class="sm" onclick="resetMfa(${a.account_id})" title="For someone who lost their device">Reset</button>`:'<span class="muted">off</span>'}</td>
      <td>${rate}</td>
      <td style="white-space:nowrap"><b>${fmtUsd(a.credits_usd)}</b>
        <button class="sm" onclick="openCredit(${Number(a.account_id)})" style="margin-left:.35rem">Add</button>
        ${Number(a.granted_usd)>0?`<br><span class="muted" style="font-size:.78rem">${fmtUsd(a.granted_usd)} added free</span>`:''}</td>
      <td class="row"><button class="sm" onclick="saveAccount(${a.account_id})">Save</button>
      <button class="sm" onclick="resetAccount(${a.account_id})" title="Follow the installation default again">Reset</button></td></tr>`}).join('')
    ||'<tr><td colspan="8" class="muted">No accounts yet.</td></tr>';
  ACCOUNTS=Object.fromEntries(as.map(a=>[Number(a.account_id),a]));
}

// ---- waitlist & invitations ----
const WL_BADGE={waiting:'info',invited:'warn',joined:'ok',declined:'bad'};
function initWaitlist(){
  mkGrid('waitlist',[
    {field:'email',headerName:'Email',flex:1.3,minWidth:200,filter:true,
      cellRenderer:p=>`<b>${esc(p.value)}</b>${p.data.name?` <span class="muted">· ${esc(p.data.name)}</span>`:''}`},
    {field:'status',headerName:'Status',width:120,filter:true,
      cellRenderer:p=>`<span class="badge ${WL_BADGE[p.value]||''}">${esc(p.value)}</span>`},
    {field:'note',headerName:'What for',flex:1.4,minWidth:180,tooltipField:'note',
      valueFormatter:p=>p.value||'—'},
    {field:'requested_at',headerName:'Asked',width:130,
      valueFormatter:p=>since(p.value),tooltipValueGetter:p=>p.value?new Date(p.value).toLocaleString():''},
    {field:'invited_at',headerName:'Invited',width:150,
      valueFormatter:p=>p.value?since(p.value)+(p.data.invited_by?' · '+p.data.invited_by.split('@')[0]:''):'—',
      tooltipValueGetter:p=>p.value?`${new Date(p.value).toLocaleString()} by ${p.data.invited_by}`
        +(p.data.invite_expires_at?` · link expires ${new Date(p.data.invite_expires_at).toLocaleString()}`:''):''},
    {field:'joined_at',headerName:'Joined',width:120,valueFormatter:p=>since(p.value)},
    {headerName:'',width:170,sortable:false,resizable:false,cellStyle:{display:'flex',justifyContent:'flex-end',alignItems:'center'},cellRenderer:p=>{
      const d=p.data,id=Number(d.waitlist_id);
      const acct=d.status==='joined'?`<span class="muted">account #${esc(d.account_id)}</span>`:'';
      return `<span class="row" style="gap:.5rem">${acct}<button class="sm more" aria-haspopup="menu" aria-expanded="false"
        title="Actions" aria-label="Actions for ${esc(d.email)}" onclick="wlMenu(this,${id})">&#8943;</button></span>`}},
  ],{tooltipShowDelay:300,overlayNoRowsTemplate:'<span class="muted">Nobody has asked to join yet.</span>'});
}
let WL_ROWS={};
async function loadWaitlist(){
  try{const r=await api('GET','/admin/api/waitlist');
    WL_ROWS=Object.fromEntries(r.rows.map(x=>[Number(x.waitlist_id),x]));
    setRows('waitlist',r.rows);
    $('wl-sub').textContent=r.open_signup
      ?'Sign-up is open right now (HUNTWELL_OPEN_SIGNUP=1), so nobody needs an invitation. Invitations still work.'
      :'Huntwell is invite-only. People who ask to join wait here; an invitation emails them a sign-up link for their address, good for 14 days.';
  }catch(e){showWlNote(errText(e),true)}}
// The link is only ever shown here, once: the database keeps its hash.
function showWlNote(html,bad){const n=$('wl-note');n.hidden=false;n.classList.toggle('info',!bad);
  n.innerHTML=`<div style="flex:1;min-width:0">${bad?esc(html):html}</div>`}
// Waitlist entries are days old, not hours, so this one counts in days.
function since(iso){if(!iso)return '—';const h=(Date.now()-new Date(iso).getTime())/36e5;
  return h<24?ago(iso):Math.floor(h/24)+'d ago'}
function invitedNote(r){
  const link=esc(r.link);
  return `${r.emailed?`Invitation emailed to <b>${esc(r.row.email)}</b>.`:`<b>The email could not be queued</b> — send ${esc(r.row.email)} this link yourself.`}
    <div class="row" style="margin-top:.5rem;gap:.4rem"><input class="mono" readonly value="${link}" style="flex:1;min-width:0" onclick="this.select()">
    <button class="sm" onclick="navigator.clipboard.writeText(this.previousElementSibling.value).then(()=>this.textContent='Copied')">Copy link</button></div>
    <div class="muted" style="font-size:.78rem;margin-top:.3rem">Works once, for that address only. Sending again replaces it.</div>`}
async function inviteSomeone(){
  const email=$('wl-email').value.trim(),name=$('wl-name').value.trim();
  if(!email){$('wl-email').focus();return}
  $('wl-btn').disabled=true;
  try{const r=await api('POST','/admin/api/waitlist/invite',{email,name});
    $('wl-email').value='';$('wl-name').value='';showWlNote(invitedNote(r));loadWaitlist()}
  catch(e){showWlNote(errText(e),true)}
  finally{$('wl-btn').disabled=false}}
async function approveWaitlist(id,resend){
  if(resend&&!confirm('Send a new invitation? The link they already have stops working.'))return;
  try{const r=await api('POST',`/admin/api/waitlist/${id}/approve`);showWlNote(invitedNote(r));loadWaitlist()}
  catch(e){showWlNote(errText(e),true)}}
// ---- free credit ----
let ACCOUNTS={},CREDIT_ID=null;
function fmtUsd(v){return '$'+Number(v||0).toLocaleString(undefined,{minimumFractionDigits:2,maximumFractionDigits:2})}
function openCredit(id){const a=ACCOUNTS[id];if(!a)return;CREDIT_ID=id;
  $('cr-who').innerHTML=`To <b>${esc(a.email)}</b> (account #${esc(a.account_id)}) · balance now ${esc(fmtUsd(a.credits_usd))}`;
  $('cr-usd').value=100;$('cr-note').value='';$('cr-err').textContent='';creditTyped();
  $('creditdlg').showModal();$('cr-usd').focus()}
function creditTyped(){const v=Number($('cr-usd').value);const ok=v>=0.01&&v<=10000;
  $('cr-btn').disabled=!ok;$('cr-btn').textContent=ok?`Add ${fmtUsd(v)} credit`:'Add credit'}
async function grantCredit(){const usd=Number($('cr-usd').value),a=ACCOUNTS[CREDIT_ID];if(!a)return;
  $('cr-btn').disabled=true;$('cr-err').textContent='Adding…';
  try{const r=await api('POST',`/admin/api/accounts/${CREDIT_ID}/credits`,{usd,note:$('cr-note').value});
    $('creditdlg').close();
    $('features-note').textContent=`Added ${fmtUsd(usd)} to ${a.email} — balance ${fmtUsd(r.credits_usd)}. They've been emailed.`;
    setTimeout(()=>$('features-note').textContent='',6000);loadFeatures()}
  catch(e){$('cr-err').textContent=errText(e);creditTyped()}}

// ---- a row's actions menu ----
let OPEN_MENU=null;
function closeMenu(){if(!OPEN_MENU)return;OPEN_MENU.m.remove();OPEN_MENU.btn.setAttribute('aria-expanded','false');OPEN_MENU=null}
document.addEventListener('mousedown',e=>{if(OPEN_MENU&&!OPEN_MENU.m.contains(e.target)&&!OPEN_MENU.btn.contains(e.target))closeMenu()});
document.addEventListener('keydown',e=>{if(e.key==='Escape')closeMenu()});
window.addEventListener('scroll',closeMenu,true);window.addEventListener('resize',closeMenu);
function wlMenu(btn,id){
  if(OPEN_MENU&&OPEN_MENU.btn===btn){closeMenu();return}
  closeMenu();const d=WL_ROWS[id];if(!d)return;
  const joined=d.status==='joined';
  const m=document.createElement('div');m.className='rowmenu';m.setAttribute('role','menu');
  const item=(label,fn,o={})=>{const b=document.createElement('button');b.type='button';b.textContent=label;
    b.setAttribute('role','menuitem');if(o.danger)b.className='danger';if(o.title)b.title=o.title;
    if(o.disabled)b.disabled=true;else b.onclick=()=>{closeMenu();fn()};m.appendChild(b)};
  item(d.status==='invited'?'Resend invitation':d.status==='declined'?'Invite anyway':'Approve',
    ()=>approveWaitlist(id,d.status==='invited'),
    {disabled:joined,title:joined?'Already has an account':d.status==='invited'?'A new link; the old one stops working':''});
  item('Decline',()=>declineWaitlist(id),
    {disabled:joined||d.status==='declined',title:joined?'Already has an account':d.status==='declined'?'Already declined':''});
  m.appendChild(document.createElement('hr'));
  item(joined?'Delete user and all data…':'Delete request…',()=>openDelete(id),{danger:true});
  document.body.appendChild(m);
  const r=btn.getBoundingClientRect(),h=m.offsetHeight,w=m.offsetWidth;
  const top=r.bottom+4+h>innerHeight-8?r.top-h-4:r.bottom+4;
  m.style.top=Math.max(8,top)+'px';m.style.left=Math.max(8,r.right-w)+'px';
  btn.setAttribute('aria-expanded','true');OPEN_MENU={m,btn};
  m.querySelector('button:not(:disabled)')?.focus()}

// ---- delete, behind a typed confirmation ----
let DEL_ID=null;
function openDelete(id){const d=WL_ROWS[id];if(!d)return;DEL_ID=id;
  const joined=d.status==='joined'&&d.account_id;
  $('del-title').textContent=joined?'Delete this user and all their data?':'Delete this request?';
  $('del-warn').innerHTML=joined
    ?`This permanently deletes <b>${esc(d.email)}</b> (account #${esc(d.account_id)}) and everything in it: every plan, result, run, file,
      API key and outreach draft, their credit balance and billing records, and their sign-in. Teammates lose access to the workspace.
      <b>This cannot be undone.</b>`
    :`This permanently deletes the request from <b>${esc(d.email)}</b>. They are not told, and any invitation link they have stops working.
      <b>This cannot be undone.</b>`;
  $('del-email').textContent=d.email;$('del-confirm').value='';$('del-err').textContent='';
  $('del-btn').disabled=true;$('del-btn').textContent=joined?'Delete user forever':'Delete forever';
  $('deldlg').showModal();$('del-confirm').focus()}
function delTyped(){const d=WL_ROWS[DEL_ID];
  $('del-btn').disabled=!d||$('del-confirm').value.trim().toLowerCase()!==d.email.trim().toLowerCase()}
async function doDelete(){const d=WL_ROWS[DEL_ID];if(!d)return;
  $('del-btn').disabled=true;$('del-err').textContent='Deleting…';
  try{const r=await api('DELETE',`/admin/api/waitlist/${DEL_ID}`,{confirm:$('del-confirm').value.trim()});
    $('deldlg').close();
    const left=[r.files_left?`${r.files_left} stored file(s) could not be removed`:'',r.account_deleted&&!r.identity_removed?'their sign-in could not be removed from the user pool':'']
      .filter(Boolean).join('; ');
    showWlNote(r.account_deleted
      ?`Deleted <b>${esc(d.email)}</b> and all their data (${Number(r.plans)} plan(s), ${Number(r.files)} file(s)).${left?` <b>Check the logs:</b> ${esc(left)}.`:''}`
      :`Deleted the request from <b>${esc(d.email)}</b>.`);
    loadWaitlist();loadFeatures()}
  catch(e){$('del-err').textContent=errText(e);delTyped()}}

async function declineWaitlist(id){
  if(!confirm('Decline this request? They are not told, and any invitation link they have stops working.'))return;
  try{await api('POST',`/admin/api/waitlist/${id}/decline`);loadWaitlist()}
  catch(e){showWlNote(errText(e),true)}}

async function saveFeatures(){
  const kinds=[...document.querySelectorAll('#features input[data-kind]')].filter(b=>b.checked).map(b=>b.dataset.kind);
  try{await api('PUT','/admin/api/features',{kinds});
    $('features-note').textContent='saved · applies to accounts following the default';
    setTimeout(()=>$('features-note').textContent='',2500);loadFeatures()}
  catch(e){$('features-note').textContent=e.message}}

async function saveAccount(id){
  const kinds=[...document.querySelectorAll(`#accounts input[data-acct="${id}"]`)].filter(b=>b.checked).map(b=>b.dataset.kind);
  try{await api('PUT',`/admin/api/accounts/${id}/kinds`,{kinds});loadFeatures()}catch(e){alert(e.message)}}

// An empty box returns the account to the installation's rate.
async function saveRate(id){
  const box=document.querySelector(`input[data-rate="${id}"]`),raw=box.value.trim();
  const usd_per_mtoken=raw===''?null:Number(raw);
  if(usd_per_mtoken!==null&&!(usd_per_mtoken>0)){alert('Enter a rate above $0, or leave it empty for the default.');return}
  if(!confirm(usd_per_mtoken===null?'Put this workspace back on the default rate? It applies to tokens billed from now on.'
    :`Charge this workspace $${usd_per_mtoken} per million tokens? It applies to tokens billed from now on.`))return;
  try{await api('PUT',`/admin/api/accounts/${id}/rate`,{usd_per_mtoken});loadFeatures()}catch(e){alert(errText(e))}}
async function resetMfa(id){if(!confirm('Turn two-factor authentication off for this account? They sign in with their password alone until they set up a device again.'))return;
  try{await api('DELETE',`/admin/api/accounts/${id}/mfa`);loadFeatures()}catch(e){alert(errText(e))}}
async function setConnectedLogins(id,on){
  // Confirmed on the way in, not out: this is the switch that lets a workspace
  // put real credentials into a remote browser.
  if(on&&!confirm('Let this workspace connect an authenticated session?\n\nThey will sign in to a site in a remote browser and runs will collect as that signed-in user.'))return;
  try{await api('PUT',`/admin/api/accounts/${id}/connected-logins`,{on});loadFeatures()}catch(e){alert(e.message)}}

async function resetAccount(id){
  try{await api('PUT',`/admin/api/accounts/${id}/kinds`,{kinds:[]});loadFeatures()}catch(e){alert(e.message)}}

// Each stage is a different job on a different scale, so each gets its own
// model. Empty means "whatever the CLI defaults to", which is a real choice
// and the one every stage starts on.
const STAGE_HELP={draft:'Writes the plan from the brief. On the critical path of "type a sentence, wait".',
  scrape:'Reads pages and pulls rows out. Long loops over big pages — this is where the bill is.',
  enrich:'Fills a row in from its own page. One short page per row.',
  planner:'Proposes the next search when learn mode is on. Small and occasional.',
  outreach:'Drafts cold outreach emails in the app and API, one short call each. Needs a direct provider (provider:model). Empty uses the newest Claude Sonnet, which needs the Anthropic key.'};

async function loadModels(){
  const chosen=await api('GET','/admin/api/models');
  let avail=[],provs=[];
  try{const r=await api('GET','/admin/api/models/available');avail=r.models||[];provs=r.providers||[]}catch(e){}
  window._avail=avail;
  // Which providers are set up, so a missing key is seen here rather than as a
  // failed run. Never the keys themselves — only whether each is present.
  $('providers').innerHTML=provs.map(p=>`<tr><td><b>${esc(p.label)}</b></td>
    <td><span class="badge ${p.configured?'ok':''}">${p.configured?'configured':'no key'}</span></td>
    <td class="mono muted">${esc(p.setting)}</td>
    <td class="muted">${p.configured?esc(p.models)+' model(s) offered':'set this to offer its models'}</td></tr>`).join('')
    ||'<tr><td colspan="4" class="muted">No providers compiled in.</td></tr>';
  $('models').innerHTML=Object.keys(STAGE_HELP).map(stage=>{
    const cur=chosen[stage]||'';
    const field=avail.length
      ? `<input type="hidden" id="m-${stage}" value="${esc(cur)}">
         <button type="button" class="model-pick" id="m-btn-${stage}" onclick="openModelPick('${stage}')">
           <span class="v">${esc(modelLabel(cur,stage))}</span><span class="caret" aria-hidden></span>
         </button>`
      : `<input id="m-${stage}" value="${esc(cur)}" placeholder="model id, e.g. gemini-3.8-flash-medium">`;
    return `<tr><td><b>${stage}</b></td><td>${field}</td>
      <td class="muted" style="font-size:.82rem">${STAGE_HELP[stage]}</td></tr>`}).join('');
  if(!avail.length)$('models-note').textContent='no provider key set and cursor-agent not reachable here — type an id';
  else $('models-note').textContent='A `provider:model` id runs in Huntwell\'s own agent loop; a bare id runs through the Cursor CLI.';
}

// Stages served by one call from the website, not a run: only a provider's own
// models can answer them, and their default is not the Cursor CLI's.
const DIRECT_ONLY={outreach:'Default (newest Claude Sonnet)'};
function defaultLabel(stage){return DIRECT_ONLY[stage]||'Default (let Cursor choose)'}
function modelLabel(id,stage){
  if(!id)return defaultLabel(stage);
  const m=(window._avail||[]).find(x=>x.id===id);
  if(!m)return id+' (not in this account\'s list)';
  const price=!m.direct?'':m.priced?` · $${m.input_per_m}/$${m.output_per_m} per M`:' · price unknown';
  return m.label+price;
}
function openModelPick(stage){
  window._modelStage=stage;
  $('model-dlg-title').textContent=stage.charAt(0).toUpperCase()+stage.slice(1)+' model';
  $('model-q').value='';
  renderModelList();
  $('modeldlg').showModal();
  $('model-q').focus();
}
function renderModelList(){
  const q=$('model-q').value.trim().toLowerCase();
  const cur=$('m-'+window._modelStage)?.value||'';
  const stage=window._modelStage,direct=stage in DIRECT_ONLY;
  const rows=[{id:'',label:defaultLabel(stage),group:'',detail:direct?'':'Cursor CLI default'}];
  (window._avail||[]).filter(m=>!direct||m.direct).forEach(m=>{
    const price=!m.direct?'':m.priced?`$${m.input_per_m}/$${m.output_per_m} per M`:'price unknown';
    rows.push({id:m.id,label:m.label,group:m.group||'',detail:price});
  });
  if(cur&&!rows.some(r=>r.id===cur)) rows.splice(1,0,{id:cur,label:cur,group:'',detail:'not in this account\'s list'});
  const shown=rows.filter(r=>!q||[r.label,r.id,r.group,r.detail].join(' ').toLowerCase().includes(q));
  let last='\0';
  $('model-list').innerHTML=shown.map(r=>{
    const head=r.group&&r.group!==last?(last=r.group,`<div class="model-grp">${esc(r.group)}</div>`):'';
    return head+`<button type="button" class="model-opt${r.id===cur?' on':''}" data-id="${esc(r.id)}">
      <span>${esc(r.label)}</span>${r.detail?`<small>${esc(r.detail)}</small>`:''}</button>`;
  }).join('')||'<div class="muted model-none">Nothing matches.</div>';
}
function pickModel(id){
  const stage=window._modelStage;
  const inp=$('m-'+stage);
  if(inp)inp.value=id;
  const btn=$('m-btn-'+stage);
  if(btn){const v=btn.querySelector('.v');if(v)v.textContent=modelLabel(id,stage)}
  $('modeldlg').close();
}

async function saveModels(){
  const models={};Object.keys(STAGE_HELP).forEach(s=>{models[s]=$('m-'+s).value.trim()});
  try{await api('PUT','/admin/api/models',{models});
    $('models-note').textContent='saved · applies to new runs';
    setTimeout(()=>$('models-note').textContent='',2500)}
  catch(e){$('models-note').textContent=e.message}}

async function saveRouting(){const strat=document.querySelector('[name=strat]:checked')?.value||'round_robin';
  const body={strategy:strat};const pin=$('pin').value;
  if(strat==='pinned'&&pin){const[h,p]=pin.split(':');body.pinned_host_id=+h;body.pinned_slot=p}
  await api('PUT','/admin/api/routing',body);$('routing-note').textContent='saved';
  setTimeout(()=>$('routing-note').textContent='',1500)}

boot();
</script>
</body>
</html>"##;

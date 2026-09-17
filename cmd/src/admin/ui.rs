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
  color:var(--text);min-height:100vh;display:flex;flex-direction:column;-webkit-font-smoothing:antialiased}
a{color:var(--link);text-decoration:none}
a:hover{text-decoration:underline}
h1,h2,h3{font-weight:900;letter-spacing:-.02em;margin:0}
h1{font-size:1.65rem}
h2{font-size:1.05rem;letter-spacing:-.01em}

/* ---------- chrome ---------- */
.topbar{background:var(--chrome);color:#fff;display:flex;align-items:center;justify-content:space-between;
  gap:1rem;padding:.55rem 1.2rem;min-height:48px}
.brand{display:flex;align-items:center;gap:.3rem;color:#fff;font-weight:900;font-size:1.32rem;letter-spacing:-.02em;user-select:none}
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
.pane{flex:1;background:var(--bg);border-radius:8px;margin:0 6px 6px 6px;overflow-y:auto;min-height:0}
:root[data-theme='dark'] .pane{border:1px solid var(--border)}
.content{padding:1.6rem 2rem 3rem;max-width:1150px;margin:0 auto;display:flex;flex-direction:column;gap:1.1rem}
@keyframes rise{from{opacity:0;transform:translateY(7px)}to{opacity:1;transform:none}}
.content>*{animation:rise .28s ease both}

/* the title row a page opens with */
.page-head{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;flex-wrap:wrap;margin-bottom:.3rem}
.page-head .sub{color:var(--text-2);margin-top:.2rem;font-size:.92rem}

/* ---------- cards ---------- */
.card{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);padding:1.2rem 1.3rem}
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
th{font-size:.76rem;text-transform:uppercase;letter-spacing:.06em;color:var(--text-3);font-weight:700;background:var(--surface-2)}
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
.stat{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);padding:1rem 1.2rem;min-width:130px;flex:1}
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

/* ---------- AG Grid, dressed in the house theme ---------- */
#route-grid{height:380px;overflow:hidden;border-radius:var(--radius)}
.ag-theme-quartz{--ag-font-family:var(--font);--ag-font-size:13px;--ag-accent-color:var(--selected);
  --ag-background-color:var(--surface);--ag-foreground-color:var(--text);
  --ag-border-color:var(--border);--ag-header-background-color:var(--surface-2);
  --ag-header-foreground-color:var(--text-3);--ag-row-hover-color:var(--surface-2);
  --ag-selected-row-background-color:var(--info-bg);--ag-odd-row-background-color:transparent;
  --ag-control-panel-background-color:var(--surface-2);--ag-input-border-color:var(--border-strong);
  --ag-wrapper-border-radius:var(--radius)}
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
<div class="topbar">
  <span class="brand"><span class="grad-text spark" aria-hidden>&#10022;</span><span>huntwell</span><span class="tag">admin</span></span>
  <span class="who">
    <button class="iconbtn" id="theme" onclick="flipTheme()" title="Toggle theme" aria-label="Toggle theme"></button>
    <span id="who"></span>
  </span>
</div>
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
    <table style="margin-top:1.2rem"><thead><tr><th>Account</th><th>Plans</th><th>Can build</th><th>Connected logins</th><th></th></tr></thead>
    <tbody id="accounts"></tbody></table>
  </div>

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

  <div class="card">
    <div class="card-head">
      <div><h2>Routing</h2><div class="sub">How a queued execution picks its slot.</div></div>
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
    <div id="route-grid" class="ag-theme-quartz"></div>
  </div>

  <div class="card">
    <div class="card-head">
      <div><h2>Recent executions</h2><div class="sub">The last 25, newest first.</div></div>
    </div>
    <table><thead><tr><th>Execution</th><th>Plan</th><th>Acct</th><th>Status</th><th>Host</th><th>Slot</th><th>Started</th></tr></thead>
    <tbody id="executions"></tbody></table>
  </div>
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

// ---- routing-log grid (AG Grid) ----
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

async function boot(){
  const s=await api('GET','/admin/api/session');
  if(!s.email){
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
  $('who').innerHTML=`<span>${esc(s.email)}</span> <button class="sm" onclick="logout()">Sign out</button>`;
  if(!routeApi)initGrid();
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
  if(routeApi)routeApi.setGridOption('rowData',lg.log);
  const rs=await api('GET','/admin/api/executions?limit=25');
  $('executions').innerHTML=rs.executions.map(r=>`<tr><td>#${esc(r.execution_id)}</td><td>${esc(r.source)}</td><td>${esc(r.account_id)}</td>
    <td><span class="badge ${r.status==='succeeded'?'ok':r.status==='failed'?'bad':'warn'}">${esc(r.status)}</span></td>
    <td>${esc(r.host_id??'—')}</td><td class="mono">${esc(r.slot_name??'—')}</td>
    <td class="muted">${new Date(r.started_at).toLocaleString()}</td></tr>`).join('')
    ||'<tr><td colspan="7" class="muted">No runs yet.</td></tr>';
}catch(e){console.warn(e)}}

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
  const as=(await api('GET','/admin/api/accounts?limit=200')).accounts;
  $('accounts').innerHTML=as.map(a=>{
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
      <td class="row"><button class="sm" onclick="saveAccount(${a.account_id})">Save</button>
      <button class="sm" onclick="resetAccount(${a.account_id})" title="Follow the installation default again">Reset</button></td></tr>`}).join('')
    ||'<tr><td colspan="5" class="muted">No accounts yet.</td></tr>';
}

async function saveFeatures(){
  const kinds=[...document.querySelectorAll('#features input[data-kind]')].filter(b=>b.checked).map(b=>b.dataset.kind);
  try{await api('PUT','/admin/api/features',{kinds});
    $('features-note').textContent='saved · applies to accounts following the default';
    setTimeout(()=>$('features-note').textContent='',2500);loadFeatures()}
  catch(e){$('features-note').textContent=e.message}}

async function saveAccount(id){
  const kinds=[...document.querySelectorAll(`#accounts input[data-acct="${id}"]`)].filter(b=>b.checked).map(b=>b.dataset.kind);
  try{await api('PUT',`/admin/api/accounts/${id}/kinds`,{kinds});loadFeatures()}catch(e){alert(e.message)}}

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
  planner:'Proposes the next search when learn mode is on. Small and occasional.'};

async function loadModels(){
  const chosen=await api('GET','/admin/api/models');
  let avail=[];try{avail=(await api('GET','/admin/api/models/available')).models||[]}catch(e){}
  window._avail=avail;
  $('models').innerHTML=Object.keys(STAGE_HELP).map(stage=>{
    const cur=chosen[stage]||'';
    // Whatever is stored stays selectable even if the account no longer lists
    // it, otherwise saving the form would silently change an unrelated stage.
    const known=avail.some(m=>m.id===cur);
    const opts=['<option value="">Default (let Cursor choose)</option>']
      .concat(!known&&cur?[`<option value="${cur}" selected>${cur} (not in this account's list)</option>`]:[])
      .concat(avail.map(m=>`<option value="${m.id}" ${m.id===cur?'selected':''}>${m.label} — ${m.id}</option>`)).join('');
    const field=avail.length
      ? `<select id="m-${stage}" style="width:100%">${opts}</select>`
      : `<input id="m-${stage}" value="${cur.replace(/"/g,'&quot;')}" placeholder="model id, e.g. gemini-3.8-flash-medium">`;
    return `<tr><td><b>${stage}</b></td><td>${field}</td>
      <td class="muted" style="font-size:.82rem">${STAGE_HELP[stage]}</td></tr>`}).join('');
  if(!avail.length)$('models-note').textContent='cursor-agent not reachable here — type an id';
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

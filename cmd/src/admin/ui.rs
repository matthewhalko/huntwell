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
  <label for="li-email">Email</label><input id="li-email" type="email" autocomplete="username">
  <label for="li-pass">Password</label><input id="li-pass" type="password" autocomplete="current-password">
  <div class="row" style="margin-top:1rem"><button class="primary" onclick="login()">Sign in</button>
  <span id="li-err" class="muted"></span></div>
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
  <span id="fr-err" class="muted"></span></div>
</div>

<main id="app" class="content hide">
  <div class="page-head">
    <div>
      <h1>Fleet</h1>
      <div class="sub">Service heartbeats, hosts, pods and where every search plan ran.</div>
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
      <div><h2>Hosts</h2><div class="sub">Each one is a cluster running a warm pool of workers.</div></div>
      <button onclick="svcBox();document.getElementById('reg').showModal()">&#65291; Register host</button>
    </div>
    <table><thead><tr><th>Host</th><th>Pool</th><th>Pods</th><th>Status</th><th></th></tr></thead>
    <tbody id="hosts"></tbody></table>
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
      <div><h2>Routing</h2><div class="sub">How a queued execution picks its pod.</div></div>
    </div>
    <div class="row">
      <label class="row" style="margin:0"><input type="radio" name="strat" value="round_robin"> Round-robin</label>
      <label class="row" style="margin:0"><input type="radio" name="strat" value="random"> Random</label>
      <label class="row" style="margin:0"><input type="radio" name="strat" value="pinned"> Pin to pod:</label>
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
    <table><thead><tr><th>Execution</th><th>Plan</th><th>Acct</th><th>Status</th><th>Host</th><th>Pod</th><th>Started</th></tr></thead>
    <tbody id="executions"></tbody></table>
  </div>
</main>
</div>

<dialog id="reg">
  <div class="card" style="border:none">
    <h2 id="reg-title">Register host</h2>
    <input type="hidden" id="h-id">
    <label>Name</label><input id="h-name" placeholder="hetzner-fsn-1">
    <label>Kubeconfig YAML <span class="muted" style="font-weight:400">(leave blank on edit to keep)</span></label>
    <textarea id="h-kc"></textarea>
    <div class="grid2">
      <div><label>Context</label><input id="h-ctx" placeholder="(default)"></div>
      <div><label>Pool size</label><input id="h-pool" type="number" min="0" max="50" value="2"></div>
      <div><label>Image</label><input id="h-img" value="huntwell-worker:dev"></div>
    </div>
    <div class="grid2">
      <div><label>CPU request</label><input id="h-cpur" value="250m"></div>
      <div><label>CPU limit</label><input id="h-cpul" value="1"></div>
      <div><label>Mem request</label><input id="h-memr" value="512Mi"></div>
      <div><label>Mem limit</label><input id="h-meml" value="1Gi"></div>
    </div>
    <label class="row" style="margin-top:.8rem"><input id="h-on" type="checkbox" checked> Enabled</label>
    <label class="row" style="margin-top:.4rem"><input id="h-svc" type="checkbox" onchange="svcBox()"> Run the application here
      <span class="muted" style="font-weight:400">— website, planning, scheduling, notification and the bus</span></label>
    <div class="grid2" id="h-svc-box" hidden>
      <div><label>Website replicas</label><input id="h-web" type="number" min="1" max="50" value="1"></div>
    </div>
    <div class="row" style="margin-top:1rem">
      <button class="primary" onclick="saveHost()">Save</button>
      <button onclick="document.getElementById('reg').close()">Cancel</button>
      <span id="h-err" class="muted"></span>
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
const api=async(m,u,b)=>{const r=await fetch(u,{method:m,headers:{'content-type':'application/json'},
  body:b?JSON.stringify(b):undefined});if(!r.ok)throw new Error((await r.text())||r.status);return r.json()};

async function login(){$('li-err').textContent='';try{
  await api('POST','/admin/api/login',{email:$('li-email').value,password:$('li-pass').value});boot()}
  catch(e){$('li-err').textContent=' '+e.message}}
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
      {field:'pod_name',headerName:'Pod',width:120,filter:true,valueFormatter:p=>p.value??'—',
        cellClass:'mono'},
      {field:'detail',headerName:'Detail',flex:1,minWidth:150},
    ],
    defaultColDef:{sortable:true,resizable:true},
    rowData:[],animateRows:false,suppressCellFocus:true,
    overlayNoRowsTemplate:'<span class="muted">No routing decisions yet — start a search with RUN_DISPATCH=pool.</span>',
  });
}

async function claim(){$('fr-err').textContent='';
  try{
    await api('POST','/admin/api/claim',{
      setupKey:$('fr-key').value,email:$('fr-email').value,password:$('fr-pass').value});
    boot();
  }catch(e){$('fr-err').textContent=e.message||'could not create the operator'}}

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
  $('who').innerHTML=`<span>${s.email}</span> <button class="sm" onclick="logout()">Sign out</button>`;
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
  $('overview').innerHTML=[['Services',`${up}/${total}`],['Hosts',`${o.hosts_healthy}/${o.hosts}`],['Pods ready',o.pods_ready],
    ['Busy',o.pods_busy],['Waiting',o.queued_unplaced]].map(([l,n])=>
    `<div class="stat"><div class="n">${n}</div><div class="l">${l}</div></div>`).join('');
  paintServices(sv);
  const hs=await api('GET','/admin/api/hosts');
  const rows=await Promise.all(hs.hosts.map(async x=>{
    const h=x.host;const pods=(await api('GET',`/admin/api/hosts/${h.host_id}/pods`)).pods;
    const podHtml=pods.map(p=>`<span class="badge ${p.ready?(p.execution_id?'warn':'ok'):'bad'}"
      title="${p.node?`on ${p.node}`:'not scheduled yet'}${p.ready?'':' · not ready'}">${p.name}${p.execution_id?` · run ${p.execution_id}`:''}
      <a href="#" onclick="killPod(${h.host_id},'${p.name}');return false" title="kill pod">✕</a></span>`).join(' ')||'<span class="muted">none seen</span>';
    // Which machines are actually carrying the pool. A node that joined the
    // cluster but holds no pod is the thing worth seeing: it is in `kubectl get
    // nodes` and doing nothing, which the pod list alone does not show.
    const byNode={};pods.forEach(p=>{const n=p.node||'(unscheduled)';byNode[n]=(byNode[n]||0)+1});
    const nodeHtml=Object.entries(byNode).map(([n,c])=>
      `<span class="badge" title="${c} pool pod${c===1?'':'s'}">${n} · ${c}</span>`).join(' ')
      ||'<span class="muted">no nodes carrying pods</span>';
    const st=h.last_error?`<span class="badge bad" title="${h.last_error.replace(/"/g,'&quot;')}">error</span>`
      :(h.enabled?'<span class="badge ok">healthy</span>':'<span class="badge warn">disabled</span>');
    return `<tr><td><b>${h.name}</b><br><span class="muted mono">${h.image}</span></td>
      <td>${h.pool_size} pods · ${h.cpu_request}/${h.cpu_limit} · ${h.mem_request}/${h.mem_limit}
        ${h.runs_services?`<br><span class="badge ok">app ×${h.web_replicas}</span>`:''}</td>
      <td>${podHtml}<br><span style="font-size:.75rem">${nodeHtml}</span></td><td>${st}${h.last_error?`<br><span class="muted" style="font-size:.75rem">${h.last_error.slice(0,120)}</span>`:''}</td>
      <td class="row"><button class="sm" onclick='editHost(${JSON.stringify(JSON.stringify(h))})'>Edit</button>
      <button class="sm" onclick="sync(${h.host_id})">Sync</button>
      <button class="sm danger" onclick="delHost(${h.host_id})">Delete</button></td></tr>`}));
  $('hosts').innerHTML=rows.join('')||'<tr><td colspan="5" class="muted">No hosts yet — register your first k3d host above.</td></tr>';
  const rt=await api('GET','/admin/api/routing');
  document.querySelectorAll('[name=strat]').forEach(r=>r.checked=r.value===rt.strategy);
  const cur=rt.pinned_host_id&&rt.pinned_pod?`${rt.pinned_host_id}:${rt.pinned_pod}`:'';
  $('pin').innerHTML=rt.free_pods.map(p=>`<option value="${p.host_id}:${p.pod}" ${cur===`${p.host_id}:${p.pod}`?'selected':''}>host ${p.host_id} · ${p.pod}</option>`).join('')
    ||`<option value="">${cur?cur+' (busy/offline)':'no free pods'}</option>`;
  const lg=await api('GET','/admin/api/route-log?limit=300');
  if(routeApi)routeApi.setGridOption('rowData',lg.log);
  const rs=await api('GET','/admin/api/executions?limit=25');
  $('executions').innerHTML=rs.executions.map(r=>`<tr><td>#${r.execution_id}</td><td>${r.source}</td><td>${r.account_id}</td>
    <td><span class="badge ${r.status==='succeeded'?'ok':r.status==='failed'?'bad':'warn'}">${r.status}</span></td>
    <td>${r.host_id??'—'}</td><td class="mono">${r.pod_name??'—'}</td>
    <td class="muted">${new Date(r.started_at).toLocaleString()}</td></tr>`).join('')
    ||'<tr><td colspan="7" class="muted">No runs yet.</td></tr>';
}catch(e){console.warn(e)}}

function svcBox(){$('h-svc-box').hidden=!$('h-svc').checked}

function editHost(js){const h=JSON.parse(js);$('reg-title').textContent='Edit host';$('h-id').value=h.host_id;
  $('h-name').value=h.name;$('h-kc').value='';$('h-ctx').value=h.kube_context||'';$('h-pool').value=h.pool_size;
  $('h-img').value=h.image;$('h-cpur').value=h.cpu_request;$('h-cpul').value=h.cpu_limit;
  $('h-memr').value=h.mem_request;$('h-meml').value=h.mem_limit;$('h-on').checked=h.enabled;
  $('h-svc').checked=!!h.runs_services;$('h-web').value=h.web_replicas||1;svcBox();
  document.getElementById('reg').showModal()}

async function saveHost(){$('h-err').textContent='';
  const body={name:$('h-name').value,kubeconfig_yaml:$('h-kc').value,kube_context:$('h-ctx').value||null,
    enabled:$('h-on').checked,pool_size:+$('h-pool').value,image:$('h-img').value,
    runs_services:$('h-svc').checked,web_replicas:+$('h-web').value,
    cpu_request:$('h-cpur').value,cpu_limit:$('h-cpul').value,mem_request:$('h-memr').value,mem_limit:$('h-meml').value};
  try{const id=$('h-id').value;
    if(id)await api('PUT',`/admin/api/hosts/${id}`,body);else await api('POST','/admin/api/hosts',body);
    document.getElementById('reg').close();$('h-id').value='';refresh()}
  catch(e){$('h-err').textContent=' '+e.message}}

async function delHost(id){if(!confirm('Remove this host? Its huntwell namespace will be deleted.'))return;
  try{await api('DELETE',`/admin/api/hosts/${id}`);refresh()}catch(e){alert(e.message)}}
async function sync(id){await api('POST',`/admin/api/hosts/${id}/sync`);refresh()}
async function killPod(id,pod){if(!confirm(`Kill ${pod}? Its current run will fail.`))return;
  try{await api('POST',`/admin/api/hosts/${id}/pods/${pod}/kill`);refresh()}catch(e){alert(e.message)}}

const KIND_LABEL={prospects:'People and companies',artifacts:'Tables',report:'Written reports',assets:'Files to keep'};
const EXPERIMENTAL=['report','assets'];

async function loadFeatures(){
  const f=await api('GET','/admin/api/features');
  window._allKinds=f.all;
  $('features').innerHTML=f.all.map(k=>`<label class="row" style="margin:0">
    <input type="checkbox" data-kind="${k}" style="width:auto" ${f.kinds.includes(k)?'checked':''}>
    ${KIND_LABEL[k]}${EXPERIMENTAL.includes(k)?' <span class="badge warn">experimental</span>':''}</label>`).join('');
  const as=(await api('GET','/admin/api/accounts?limit=200')).accounts;
  $('accounts').innerHTML=as.map(a=>{
    const own=(a.kinds||'').split(',').filter(Boolean);
    // No list of their own means they follow the default, and should keep
    // following it when it changes — so that state is shown, not resolved away.
    const boxes=f.all.map(k=>`<label class="row" style="margin:0;display:inline-flex">
      <input type="checkbox" data-acct="${a.account_id}" data-kind="${k}" style="width:auto"
        ${own.length?(own.includes(k)?'checked':''):(f.kinds.includes(k)?'checked':'')}> ${KIND_LABEL[k]}</label>`).join(' ');
    // Its own column and its own switch, not one of the "can build" boxes:
    // those pick which kinds of plan exist, while this hands a browser a
    // customer's real credentials. Off for everyone until an operator says so.
    const cl=a.connected_logins
      ?`<span class="badge ok">on</span> <button class="sm" onclick="setConnectedLogins(${a.account_id},false)">Turn off</button>`
      :`<span class="muted">off</span> <button class="sm" onclick="setConnectedLogins(${a.account_id},true)">Turn on</button>`;
    return `<tr><td><b>${a.email}</b><br><span class="muted">#${a.account_id}${a.display_name?' · '+a.display_name:''}</span></td>
      <td>${a.plans}</td>
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
  if(strat==='pinned'&&pin){const[h,p]=pin.split(':');body.pinned_host_id=+h;body.pinned_pod=p}
  await api('PUT','/admin/api/routing',body);$('routing-note').textContent='saved';
  setTimeout(()=>$('routing-note').textContent='',1500)}

boot();
</script>
</body>
</html>"##;

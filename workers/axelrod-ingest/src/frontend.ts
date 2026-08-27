export function renderFrontend(): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Axelrod — Private AI Data Platform</title>
<style>
:root {
  --bg:       #0d1117;
  --card:     #161b22;
  --border:   #30363d;
  --text:     #e6edf3;
  --muted:    #8b949e;
  --accent:   #58a6ff;
  --accent2:  #388bfd;
  --danger:   #f85149;
  --success:  #3fb950;
  --harvest:  #d29922;
  --radius:   8px;
  --nav-h:    56px;
}
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
html,body{height:100%;background:var(--bg);color:var(--text);font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;font-size:14px;line-height:1.5}
a{color:var(--accent);text-decoration:none}
a:hover{text-decoration:underline}
button{cursor:pointer;font-family:inherit;font-size:14px}
input{font-family:inherit;font-size:14px}

/* ── Nav ── */
nav{position:fixed;top:0;left:0;right:0;height:var(--nav-h);background:var(--card);border-bottom:1px solid var(--border);display:flex;align-items:center;padding:0 24px;gap:24px;z-index:100}
.nav-logo{font-size:18px;font-weight:700;color:var(--text);letter-spacing:-0.5px;margin-right:16px}
.nav-logo span{color:var(--accent)}
.nav-links{display:flex;gap:4px;flex:1}
.nav-link{padding:6px 12px;border-radius:6px;color:var(--muted);transition:color .15s,background .15s;border:none;background:transparent}
.nav-link:hover,.nav-link.active{color:var(--text);background:rgba(255,255,255,.07)}
.nav-right{display:flex;align-items:center;gap:10px}
.nav-auth{font-size:12px;color:var(--success);background:rgba(63,185,80,.1);border:1px solid rgba(63,185,80,.3);border-radius:12px;padding:3px 10px}
.btn{display:inline-flex;align-items:center;gap:6px;padding:6px 14px;border-radius:6px;border:none;font-weight:500;transition:opacity .15s}
.btn-primary{background:var(--accent);color:#0d1117}
.btn-primary:hover{opacity:.85}
.btn-ghost{background:transparent;color:var(--muted);border:1px solid var(--border)}
.btn-ghost:hover{color:var(--text);border-color:var(--muted)}
.btn-sm{padding:4px 10px;font-size:12px}
.btn-danger{background:rgba(248,81,73,.15);color:var(--danger);border:1px solid rgba(248,81,73,.3)}
.btn-danger:hover{background:rgba(248,81,73,.25)}

/* ── Layout ── */
#app{padding-top:var(--nav-h)}
.page{padding:32px 24px;max-width:1100px;margin:0 auto;display:none}
.page.active{display:block}

/* ── Cards ── */
.card{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:20px}
.card+.card{margin-top:12px}

/* ── Hero ── */
.hero{padding:80px 0 60px;text-align:center}
.hero h1{font-size:48px;font-weight:700;letter-spacing:-1.5px;color:var(--text);margin-bottom:16px}
.hero h1 span{color:var(--accent)}
.hero p{font-size:18px;color:var(--muted);max-width:520px;margin:0 auto 32px}
.feature-grid{display:grid;grid-template-columns:repeat(3,1fr);gap:16px;margin-top:48px}
@media(max-width:640px){.feature-grid{grid-template-columns:1fr}}
.feature-card{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:24px;text-align:left}
.feature-icon{font-size:24px;margin-bottom:12px}
.feature-card h3{font-size:15px;font-weight:600;margin-bottom:6px}
.feature-card p{color:var(--muted);font-size:13px}
.footer{margin-top:80px;padding-top:24px;border-top:1px solid var(--border);text-align:center;color:var(--muted);font-size:12px}

/* ── Page headers ── */
.page-header{display:flex;align-items:center;justify-content:space-between;margin-bottom:24px}
.page-header h2{font-size:22px;font-weight:600}
.badge{display:inline-flex;align-items:center;padding:2px 8px;border-radius:12px;font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.5px}
.badge-dataset{background:rgba(88,166,255,.15);color:var(--accent)}
.badge-harvest{background:rgba(210,153,34,.15);color:var(--harvest)}

/* ── Search ── */
.search-bar{display:flex;gap:10px;margin-bottom:20px}
.search-bar input{flex:1;background:var(--card);border:1px solid var(--border);border-radius:6px;padding:8px 12px;color:var(--text);outline:none}
.search-bar input:focus{border-color:var(--accent)}

/* ── Dataset grid ── */
.dataset-grid{display:grid;grid-template-columns:repeat(2,1fr);gap:12px}
@media(max-width:700px){.dataset-grid{grid-template-columns:1fr}}
.dataset-card{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:16px;cursor:pointer;transition:border-color .15s,box-shadow .15s}
.dataset-card:hover{border-color:var(--accent);box-shadow:0 0 0 1px rgba(88,166,255,.2)}
.dataset-card-top{display:flex;align-items:flex-start;justify-content:space-between;margin-bottom:10px}
.dataset-card h3{font-size:15px;font-weight:600;color:var(--text)}
.dataset-meta{display:flex;gap:16px;color:var(--muted);font-size:12px;margin-top:8px;flex-wrap:wrap}
.dataset-meta span{display:flex;align-items:center;gap:4px}

/* ── Detail ── */
.detail-header{margin-bottom:24px}
.detail-header h2{font-size:24px;font-weight:700;margin-bottom:8px}
.detail-meta{display:flex;gap:20px;flex-wrap:wrap;color:var(--muted);font-size:13px;margin-top:10px}
.detail-meta span{display:flex;align-items:center;gap:4px}
.tabs{display:flex;gap:0;border-bottom:1px solid var(--border);margin-bottom:24px}
.tab{padding:10px 18px;color:var(--muted);border-bottom:2px solid transparent;margin-bottom:-1px;cursor:pointer;transition:color .15s,border-color .15s;background:transparent;border-top:none;border-left:none;border-right:none;font-size:14px}
.tab:hover{color:var(--text)}
.tab.active{color:var(--accent);border-bottom-color:var(--accent)}
.tab-panel{display:none}
.tab-panel.active{display:block}

/* ── Tables ── */
.table-wrap{overflow-x:auto;border:1px solid var(--border);border-radius:var(--radius)}
table{width:100%;border-collapse:collapse;font-size:13px}
th{background:rgba(255,255,255,.04);padding:10px 14px;text-align:left;color:var(--muted);font-weight:500;border-bottom:1px solid var(--border)}
td{padding:9px 14px;border-bottom:1px solid rgba(48,54,61,.6);color:var(--text);max-width:220px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
tr:last-child td{border-bottom:none}

/* ── Stats bars ── */
.stat-grid{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-bottom:24px}
@media(max-width:600px){.stat-grid{grid-template-columns:1fr}}
.stat-card{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:16px}
.stat-card .label{font-size:12px;color:var(--muted);margin-bottom:4px}
.stat-card .value{font-size:24px;font-weight:600;color:var(--text)}
.bar-chart{display:flex;flex-direction:column;gap:10px;margin-top:8px}
.bar-row{display:flex;align-items:center;gap:10px}
.bar-label{width:140px;font-size:12px;color:var(--muted);text-align:right;flex-shrink:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.bar-track{flex:1;background:rgba(255,255,255,.06);border-radius:4px;height:18px;overflow:hidden}
.bar-fill{height:100%;border-radius:4px;background:var(--accent);display:flex;align-items:center;padding-left:6px;font-size:11px;color:#0d1117;font-weight:600;min-width:2px;transition:width .4s ease}
.bar-count{font-size:12px;color:var(--muted);width:60px;text-align:right}

/* ── Storage ── */
.storage-summary{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-bottom:24px}

/* ── Models ── */
.coming-soon{text-align:center;padding:60px 24px;color:var(--muted)}
.coming-soon h3{font-size:20px;color:var(--text);margin-bottom:8px}
.mock-model-card{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:16px;opacity:.55}
.mock-model-grid{display:grid;grid-template-columns:repeat(2,1fr);gap:12px;margin-top:24px;pointer-events:none}
@media(max-width:640px){.mock-model-grid{grid-template-columns:1fr}}

/* ── Modal ── */
.modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,.7);display:flex;align-items:center;justify-content:center;z-index:200;display:none}
.modal-overlay.open{display:flex}
.modal{background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:28px;width:100%;max-width:420px}
.modal h3{font-size:17px;font-weight:600;margin-bottom:6px}
.modal p{color:var(--muted);font-size:13px;margin-bottom:18px}
.modal input{width:100%;background:var(--bg);border:1px solid var(--border);border-radius:6px;padding:10px 12px;color:var(--text);outline:none;margin-bottom:14px;font-size:14px}
.modal input:focus{border-color:var(--accent)}
.modal-actions{display:flex;gap:8px;justify-content:flex-end}
.error-msg{color:var(--danger);font-size:12px;margin-bottom:10px;display:none}

/* ── Empty/Loading states ── */
.empty{text-align:center;padding:48px;color:var(--muted)}
.empty h3{color:var(--text);margin-bottom:6px}
.loading{text-align:center;padding:48px;color:var(--muted)}
.back-btn{display:inline-flex;align-items:center;gap:6px;color:var(--muted);font-size:13px;margin-bottom:20px;background:transparent;border:none;cursor:pointer;padding:0}
.back-btn:hover{color:var(--text)}
</style>
</head>
<body>

<nav>
  <span class="nav-logo">axel<span>rod</span></span>
  <div class="nav-links">
    <button class="nav-link" onclick="navigate('/')">Home</button>
    <button class="nav-link" onclick="navigate('/datasets')">Datasets</button>
    <button class="nav-link" onclick="navigate('/storage')">Storage</button>
    <button class="nav-link" onclick="navigate('/models')">Models</button>
  </div>
  <div class="nav-right" id="nav-auth">
    <button class="btn btn-ghost btn-sm" onclick="showAuthModal()">Log in</button>
  </div>
</nav>

<div id="app">

  <!-- Landing -->
  <div class="page" id="page-home">
    <div class="hero">
      <h1>Private AI<br><span>Data Platform</span></h1>
      <p>Collect, organize, and serve training data for AI systems — on your infrastructure, under your control.</p>
      <button class="btn btn-primary" style="font-size:16px;padding:10px 24px" onclick="showAuthModal()">Get Started</button>
      <div class="feature-grid">
        <div class="feature-card">
          <div class="feature-icon">&#x1F4BE;</div>
          <h3>Datasets</h3>
          <p>Upload and version structured JSONL datasets. Query, sample, and export with a simple REST API.</p>
        </div>
        <div class="feature-card">
          <div class="feature-icon">&#x1F4CA;</div>
          <h3>Storage</h3>
          <p>R2-backed object storage with automatic metadata. Monitor usage and growth across all your datasets.</p>
        </div>
        <div class="feature-card">
          <div class="feature-icon">&#x1F916;</div>
          <h3>Models</h3>
          <p>Track training runs, model versions, and performance benchmarks. (Coming soon.)</p>
        </div>
      </div>
    </div>
    <div class="footer">Built by AXE Technologies</div>
  </div>

  <!-- Datasets list -->
  <div class="page" id="page-datasets">
    <div class="page-header">
      <h2>Datasets <span id="ds-count" style="color:var(--muted);font-weight:400;font-size:16px"></span></h2>
    </div>
    <div class="search-bar">
      <input id="ds-search" type="text" placeholder="Filter datasets…" oninput="filterDatasets()">
    </div>
    <div id="ds-grid" class="dataset-grid"></div>
  </div>

  <!-- Dataset detail -->
  <div class="page" id="page-detail">
    <button class="back-btn" onclick="navigate('/datasets')">&#8592; Datasets</button>
    <div class="detail-header">
      <div style="display:flex;align-items:center;gap:10px;flex-wrap:wrap">
        <h2 id="detail-name"></h2>
        <span id="detail-badge" class="badge"></span>
      </div>
      <div class="detail-meta" id="detail-meta"></div>
    </div>
    <div class="tabs">
      <button class="tab active" onclick="switchTab('preview')">Preview</button>
      <button class="tab" onclick="switchTab('stats')">Stats</button>
      <button class="tab" onclick="switchTab('download')">Download</button>
    </div>
    <div class="tab-panel active" id="tab-preview">
      <div id="preview-content"><div class="loading">Loading sample…</div></div>
    </div>
    <div class="tab-panel" id="tab-stats">
      <div id="stats-content"><div class="loading">Loading stats…</div></div>
    </div>
    <div class="tab-panel" id="tab-download">
      <div class="card" style="max-width:480px">
        <h3 style="margin-bottom:8px">Download Dataset</h3>
        <p style="color:var(--muted);font-size:13px;margin-bottom:16px">Downloads as NDJSON (.jsonl). All chunks are concatenated into a single stream.</p>
        <button class="btn btn-primary" id="dl-btn" onclick="downloadDataset()">&#x2193; Download .jsonl</button>
      </div>
    </div>
  </div>

  <!-- Storage -->
  <div class="page" id="page-storage">
    <div class="page-header"><h2>Storage</h2></div>
    <div class="storage-summary">
      <div class="stat-card"><div class="label">Total Storage</div><div class="value" id="st-total"></div></div>
      <div class="stat-card"><div class="label">Datasets</div><div class="value" id="st-count"></div></div>
      <div class="stat-card"><div class="label">Total Rows</div><div class="value" id="st-rows"></div></div>
    </div>
    <div class="card">
      <h3 style="margin-bottom:16px;font-size:15px">Size by Dataset</h3>
      <div id="storage-chart" class="bar-chart"></div>
    </div>
  </div>

  <!-- Models -->
  <div class="page" id="page-models">
    <div class="page-header"><h2>Models</h2></div>
    <div class="coming-soon">
      <div style="font-size:40px;margin-bottom:16px">&#x1F916;</div>
      <h3>Coming Soon</h3>
      <p>Track model versions, training runs, and performance metrics alongside your datasets.</p>
    </div>
    <div class="mock-model-grid">
      ${[
        {name:'piggybank-v0.1',ver:'0.1.0',params:'7B',trained:'2026-08-01'},
        {name:'axelrod-instruct',ver:'0.2.1',params:'13B',trained:'2026-07-15'},
      ].map(m => `
      <div class="mock-model-card">
        <div style="display:flex;justify-content:space-between;align-items:flex-start;margin-bottom:8px">
          <strong>${m.name}</strong>
          <span class="badge" style="background:rgba(255,255,255,.08);color:var(--muted)">v${m.ver}</span>
        </div>
        <div style="color:var(--muted);font-size:12px;display:flex;gap:14px">
          <span>&#x2699; ${m.params} params</span>
          <span>&#x1F4C5; ${m.trained}</span>
        </div>
      </div>`).join('')}
    </div>
  </div>

</div>

<!-- Auth modal -->
<div class="modal-overlay" id="auth-modal">
  <div class="modal">
    <h3>Enter API Key</h3>
    <p>Your key is stored locally in this browser only. It is sent as a Bearer token with every request.</p>
    <input type="password" id="api-key-input" placeholder="axelrod_key_…" onkeydown="if(event.key==='Enter')saveKey()">
    <div class="error-msg" id="key-error">Invalid key — server returned 401.</div>
    <div class="modal-actions">
      <button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
      <button class="btn btn-primary" onclick="saveKey()">Save &amp; continue</button>
    </div>
  </div>
</div>

<script>
(function(){
"use strict";

// ── State ──────────────────────────────────────────────────────────────────
var KEY_LS = 'axelrod_api_key';
var _datasets = [];
var _currentDataset = null;

function getKey(){ return localStorage.getItem(KEY_LS) || ''; }
function setKey(k){ localStorage.setItem(KEY_LS, k); }
function clearKey(){ localStorage.removeItem(KEY_LS); }
function isAuthed(){ return !!getKey(); }

// ── Fetch wrapper ──────────────────────────────────────────────────────────
async function api(path){
  var k = getKey();
  var headers = k ? {Authorization:'Bearer '+k} : {};
  var res = await fetch(path, {headers});
  if(res.status===401){
    clearKey();
    renderNav();
    showAuthModal();
    throw new Error('401');
  }
  if(!res.ok) throw new Error('HTTP '+res.status);
  return res.json();
}

// ── Auth modal ─────────────────────────────────────────────────────────────
window.showAuthModal = function(){
  document.getElementById('auth-modal').classList.add('open');
  var inp = document.getElementById('api-key-input');
  inp.value = getKey();
  document.getElementById('key-error').style.display='none';
  setTimeout(function(){ inp.focus(); }, 50);
};
window.closeModal = function(){
  document.getElementById('auth-modal').classList.remove('open');
};
window.saveKey = async function(){
  var k = document.getElementById('api-key-input').value.trim();
  if(!k) return;
  setKey(k);
  // Quick validation: hit /datasets
  try {
    await api('/datasets');
    document.getElementById('key-error').style.display='none';
    closeModal();
    renderNav();
    // Re-navigate to current page to load data
    navigate(currentPath());
  } catch(e){
    if(e.message==='401'){
      document.getElementById('key-error').style.display='block';
    }
  }
};

function renderNav(){
  var el = document.getElementById('nav-auth');
  if(isAuthed()){
    el.innerHTML='<span class="nav-auth">&#x2713; Authenticated</span><button class="btn btn-ghost btn-sm" onclick="logout()">Log out</button>';
  } else {
    el.innerHTML='<button class="btn btn-ghost btn-sm" onclick="showAuthModal()">Log in</button>';
  }
}
window.logout = function(){
  clearKey();
  renderNav();
  navigate('/');
};

// ── Router ─────────────────────────────────────────────────────────────────
function currentPath(){ return window.location.pathname || '/'; }

window.navigate = function(path){
  history.pushState(null,'',path);
  dispatch(path);
};
window.addEventListener('popstate', function(){
  dispatch(currentPath());
});

function dispatch(path){
  document.querySelectorAll('.nav-link').forEach(function(b){ b.classList.remove('active'); });
  if(path==='/') document.querySelectorAll('.nav-link')[0].classList.add('active');
  else if(path.startsWith('/datasets')) document.querySelectorAll('.nav-link')[1].classList.add('active');
  else if(path.startsWith('/storage')) document.querySelectorAll('.nav-link')[2].classList.add('active');
  else if(path.startsWith('/models')) document.querySelectorAll('.nav-link')[3].classList.add('active');

  document.querySelectorAll('.page').forEach(function(p){ p.classList.remove('active'); });

  if(path==='/'){
    document.getElementById('page-home').classList.add('active');
  } else if(path==='/datasets'){
    document.getElementById('page-datasets').classList.add('active');
    if(isAuthed()) loadDatasets(); else showAuthModal();
  } else if(path.startsWith('/datasets/')){
    var name = path.slice('/datasets/'.length).split('/')[0];
    document.getElementById('page-detail').classList.add('active');
    if(isAuthed()) loadDetail(name); else showAuthModal();
  } else if(path==='/storage'){
    document.getElementById('page-storage').classList.add('active');
    if(isAuthed()) loadStorage(); else showAuthModal();
  } else if(path==='/models'){
    document.getElementById('page-models').classList.add('active');
  } else {
    document.getElementById('page-home').classList.add('active');
  }
}

// ── Helpers ────────────────────────────────────────────────────────────────
function fmtBytes(b){
  if(b==null||b===0) return '0 B';
  var units=['B','KB','MB','GB','TB'];
  var i=0; while(b>=1024&&i<units.length-1){b/=1024;i++;}
  return b.toFixed(i===0?0:1)+' '+units[i];
}
function fmtDate(s){
  if(!s) return '—';
  try{ return new Date(s).toLocaleDateString('en-US',{month:'short',day:'numeric',year:'numeric'}); }
  catch(e){ return s; }
}
function fmtNum(n){
  if(n==null) return '—';
  return Number(n).toLocaleString();
}
function escHtml(s){
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

// ── Datasets page ──────────────────────────────────────────────────────────
async function loadDatasets(){
  var grid = document.getElementById('ds-grid');
  grid.innerHTML='<div class="loading">Loading datasets…</div>';
  try {
    _datasets = await api('/datasets');
    renderDatasetGrid(_datasets);
    document.getElementById('ds-count').textContent='('+_datasets.length+')';
  } catch(e){
    if(e.message!=='401') grid.innerHTML='<div class="empty"><h3>Error</h3><p>Could not load datasets.</p></div>';
  }
}

window.filterDatasets = function(){
  var q = document.getElementById('ds-search').value.toLowerCase();
  var filtered = _datasets.filter(function(d){ return d.name.toLowerCase().includes(q); });
  renderDatasetGrid(filtered);
};

function renderDatasetGrid(list){
  var grid = document.getElementById('ds-grid');
  if(!list.length){
    grid.innerHTML='<div class="empty" style="grid-column:1/-1"><h3>No datasets</h3><p>Upload data via the API to see it here.</p></div>';
    return;
  }
  grid.innerHTML = list.map(function(d){
    var badgeClass = d.type==='harvest' ? 'badge-harvest' : 'badge-dataset';
    return '<div class="dataset-card" onclick="navigate(\'/datasets/'+escHtml(d.name)+'\')">' +
      '<div class="dataset-card-top">' +
        '<h3>'+escHtml(d.name)+'</h3>' +
        '<span class="badge '+badgeClass+'">'+escHtml(d.type||'dataset')+'</span>' +
      '</div>' +
      '<div class="dataset-meta">' +
        '<span>&#x1F4BE; '+fmtBytes(d.size_bytes)+'</span>' +
        (d.row_count!=null?'<span>&#x23; '+fmtNum(d.row_count)+' rows</span>':'') +
        '<span>&#x1F4C5; '+fmtDate(d.updated_at)+'</span>' +
      '</div>' +
    '</div>';
  }).join('');
}

// ── Dataset detail ─────────────────────────────────────────────────────────
async function loadDetail(name){
  _currentDataset = name;
  document.getElementById('detail-name').textContent = name;
  document.getElementById('detail-meta').innerHTML = '<span>Loading…</span>';
  document.getElementById('preview-content').innerHTML = '<div class="loading">Loading sample…</div>';
  document.getElementById('stats-content').innerHTML = '<div class="loading">Loading stats…</div>';

  // Reset tabs
  document.querySelectorAll('.tab').forEach(function(t,i){ t.classList.toggle('active',i===0); });
  document.querySelectorAll('.tab-panel').forEach(function(p,i){ p.classList.toggle('active',i===0); });

  try {
    var meta = await api('/datasets/'+encodeURIComponent(name));
    renderDetailMeta(meta);
  } catch(e){ }

  loadPreview(name);
}

function renderDetailMeta(meta){
  var badgeEl = document.getElementById('detail-badge');
  var t = meta.type||'dataset';
  badgeEl.className = 'badge '+(t==='harvest'?'badge-harvest':'badge-dataset');
  badgeEl.textContent = t;
  var parts = [];
  if(meta.size_bytes!=null) parts.push('<span>&#x1F4BE; '+fmtBytes(meta.size_bytes)+'</span>');
  if(meta.row_count!=null) parts.push('<span>&#x23; '+fmtNum(meta.row_count)+' rows</span>');
  if(meta.chunks!=null) parts.push('<span>&#x1F4E6; '+meta.chunks+' chunks</span>');
  if(meta.date_range&&meta.date_range.from) parts.push('<span>&#x1F4C5; '+meta.date_range.from+' &#x2192; '+(meta.date_range.to||'now')+'</span>');
  if(meta.updated_at) parts.push('<span>Updated '+fmtDate(meta.updated_at)+'</span>');
  document.getElementById('detail-meta').innerHTML = parts.join('');
}

async function loadPreview(name){
  var el = document.getElementById('preview-content');
  try {
    var rows = await api('/datasets/'+encodeURIComponent(name)+'/sample?n=20');
    if(!rows.length){ el.innerHTML='<div class="empty"><p>No rows found.</p></div>'; return; }
    var cols = Object.keys(rows[0]);
    var html = '<div class="table-wrap"><table><thead><tr>'+
      cols.map(function(c){ return '<th>'+escHtml(c)+'</th>'; }).join('')+
      '</tr></thead><tbody>'+
      rows.map(function(r){
        return '<tr>'+cols.map(function(c){
          var v = r[c];
          var s = v==null?'':typeof v==='object'?JSON.stringify(v):String(v);
          return '<td title="'+escHtml(s)+'">'+escHtml(s.length>80?s.slice(0,80)+'…':s)+'</td>';
        }).join('')+'</tr>';
      }).join('')+
      '</tbody></table></div>';
    el.innerHTML = html;
  } catch(e){
    if(e.message!=='401') el.innerHTML='<div class="empty"><p>Could not load sample.</p></div>';
  }
}

window.switchTab = function(tab){
  var panels = {preview:'tab-preview',stats:'tab-stats',download:'tab-download'};
  document.querySelectorAll('.tab').forEach(function(t,i){
    var names=['preview','stats','download'];
    t.classList.toggle('active', names[i]===tab);
  });
  Object.keys(panels).forEach(function(k){
    document.getElementById(panels[k]).classList.toggle('active', k===tab);
  });
  if(tab==='stats' && _currentDataset) loadStats(_currentDataset);
};

async function loadStats(name){
  var el = document.getElementById('stats-content');
  if(el.dataset.loaded===name) return;
  el.innerHTML='<div class="loading">Loading stats…</div>';
  try {
    var s = await api('/datasets/'+encodeURIComponent(name)+'/stats');
    el.dataset.loaded = name;
    var dist = s.event_distribution||{};
    var total = Object.values(dist).reduce(function(a,b){return a+b;},0)||1;
    var barRows = Object.entries(dist).sort(function(a,b){return b[1]-a[1];}).map(function(kv){
      var pct = Math.round(kv[1]/total*100);
      return '<div class="bar-row">' +
        '<span class="bar-label">'+escHtml(kv[0])+'</span>' +
        '<div class="bar-track"><div class="bar-fill" style="width:'+pct+'%">'+pct+'%</div></div>' +
        '<span class="bar-count">'+fmtNum(kv[1])+'</span>' +
      '</div>';
    }).join('');
    el.innerHTML =
      '<div class="stat-grid">' +
        '<div class="stat-card"><div class="label">Total Rows</div><div class="value">'+fmtNum(s.total_rows)+'</div></div>' +
        '<div class="stat-card"><div class="label">Sessions</div><div class="value">'+fmtNum(s.session_count)+'</div></div>' +
        '<div class="stat-card"><div class="label">Size</div><div class="value">'+fmtBytes(s.total_bytes)+'</div></div>' +
      '</div>' +
      (s.date_range&&s.date_range.from?'<p style="color:var(--muted);font-size:13px;margin-bottom:16px">Date range: '+escHtml(s.date_range.from)+' → '+escHtml(s.date_range.to||'now')+'</p>':'') +
      (barRows?'<h4 style="margin-bottom:12px;font-size:13px;color:var(--muted)">Event Distribution</h4><div class="bar-chart">'+barRows+'</div>':'');
  } catch(e){
    if(e.message!=='401') el.innerHTML='<div class="empty"><p>Could not load stats.</p></div>';
  }
}

window.downloadDataset = function(){
  var name = _currentDataset;
  if(!name) return;
  var k = getKey();
  // Build a hidden form or open fetch-blob approach
  fetch('/datasets/'+encodeURIComponent(name)+'/download', {
    headers: k ? {Authorization:'Bearer '+k} : {}
  }).then(function(res){
    if(res.status===401){ clearKey(); renderNav(); showAuthModal(); return; }
    return res.blob();
  }).then(function(blob){
    if(!blob) return;
    var a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = name+'.jsonl';
    a.click();
    URL.revokeObjectURL(a.href);
  });
};

// ── Storage page ───────────────────────────────────────────────────────────
async function loadStorage(){
  document.getElementById('st-total').textContent='…';
  document.getElementById('st-count').textContent='…';
  document.getElementById('st-rows').textContent='…';
  document.getElementById('storage-chart').innerHTML='<div class="loading">Loading…</div>';
  try {
    var list = await api('/datasets');
    var totalBytes = list.reduce(function(a,d){return a+(d.size_bytes||0);},0);
    var totalRows = list.reduce(function(a,d){return a+(d.row_count||0);},0);
    document.getElementById('st-total').textContent = fmtBytes(totalBytes);
    document.getElementById('st-count').textContent = list.length;
    document.getElementById('st-rows').textContent = fmtNum(totalRows);

    var maxSize = Math.max.apply(null, list.map(function(d){return d.size_bytes||0;})) || 1;
    var bars = list.sort(function(a,b){return (b.size_bytes||0)-(a.size_bytes||0);}).map(function(d){
      var pct = Math.round((d.size_bytes||0)/maxSize*100);
      return '<div class="bar-row">' +
        '<span class="bar-label">'+escHtml(d.name)+'</span>' +
        '<div class="bar-track"><div class="bar-fill" style="width:'+pct+'%"></div></div>' +
        '<span class="bar-count">'+fmtBytes(d.size_bytes||0)+'</span>' +
      '</div>';
    }).join('');
    document.getElementById('storage-chart').innerHTML = bars || '<div class="empty"><p>No datasets found.</p></div>';
  } catch(e){
    if(e.message!=='401') document.getElementById('storage-chart').innerHTML='<div class="empty"><p>Could not load storage data.</p></div>';
  }
}

// ── Boot ───────────────────────────────────────────────────────────────────
renderNav();
dispatch(currentPath());

})();
</script>
</body>
</html>`;
}

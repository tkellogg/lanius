// elanus web — agent explorer. Everything live arrived over plain MQTT
// (relayed via SSE); everything historical comes from the userland `history`
// package's HTTP endpoint (proxied by the server as GET /api/history).
// AUTHORITY: read-and-converse only — no approve/revoke/kill here.
'use strict';

const $ = (s) => document.querySelector(s);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};

// ---------- state ----------
let DEFAULT_AGENT = 'main';
let count = 0;
let paused = false;
let filter = 'signals'; // rail filter; signals view opens on the alarm lane
let sel = { kind: 'signals' }; // or { kind:'agent', agent, tab }
let historyOk = null; // null = unprobed, false = live-only, true = view running
const buffer = []; // replayable rail history (the server ring, client-side)
const BUFFER_CAP = 2000;
const agents = new Map(); // name -> { sessions:Set, live:bool }
const convFeeds = new Map(); // agent -> persistent DOM feed
const corrAgent = new Map(); // correlation -> agent (for routing in/human mail)
const seenAsks = new Map(); // corr -> ask element refs
const sentCorrs = new Set();
const agentSessions = new Map(); // agent -> stable web chat session id

// ---------- helpers ----------
const timeOf = (env) => {
  const d = new Date(env?.ts ?? Date.now());
  return isNaN(d) ? '--:--:--' : d.toTimeString().slice(0, 8);
};
const shortTs = (t) => (typeof t === 'string' ? t.replace('T', ' ').slice(0, 19) : '');
const summarize = (p, max = 110) => {
  if (p == null) return '';
  const s = typeof p === 'string' ? p : JSON.stringify(p);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
};
const arr = (v) => String(v ?? '').split(',').map((x) => x.trim()).filter(Boolean);
const atBottom = (box) => box.scrollHeight - box.scrollTop - box.clientHeight < 60;
const stick = (box, was) => { if (was) box.scrollTop = box.scrollHeight; };
const agentOf = (topic) => {
  const m = topic.match(/^(?:in|obs)\/agent\/([^/]+)/);
  return m ? m[1] : null;
};

// ---------- nav ----------
function touchAgent(name, { live = false, sessions = [], profile = null } = {}) {
  if (!name) return;
  let a = agents.get(name);
  if (!a) { a = { sessions: new Set(), live: false, profile: null }; agents.set(name, a); }
  if (live) a.live = true;
  if (profile) a.profile = profile;
  for (const s of sessions) a.sessions.add(s);
}

// Disk is an agent source too: every profile IS an agent identity, visible
// before it ever speaks. A blank root shows its default agent immediately.
let diskProfiles = [];
async function loadDiskAgents() {
  try {
    const r = await fetch('/api/admin/agents');
    const j = await r.json();
    if (!j.ok) return;
    diskProfiles = j.profiles ?? [];
    for (const p of diskProfiles) touchAgent(p.agent, { profile: p.profile });
    renderNav();
    if (sel.kind === 'welcome') renderWelcome();
  } catch { /* admin endpoints absent: live-only nav, same as before */ }
}
function profileOf(agentName) {
  return diskProfiles.find((p) => p.agent === agentName)
    ?? diskProfiles.find((p) => p.profile === agentName) ?? null;
}

function renderNav() {
  const box = $('#nav-agents');
  box.textContent = '';
  $('#nav-empty').hidden = agents.size > 0;
  for (const name of [...agents.keys()].sort()) {
    const a = agents.get(name);
    const btn = el('button', 'nav-item nav-agent', '');
    btn.dataset.sel = `agent:${name}`;
    btn.appendChild(el('span', 'nav-sigil', '⟁'));
    btn.append(` ${name}`);
    if (a.live) btn.appendChild(el('span', 'nav-live', '·live'));
    btn.classList.toggle('on', sel.kind === 'agent' && sel.agent === name);
    btn.onclick = () => selectAgent(name);
    box.appendChild(btn);
    const sess = [...a.sessions].sort().reverse();
    for (const s of sess.slice(0, 12)) {
      const sb = el('button', 'nav-item nav-session', s);
      sb.onclick = () => { selectAgent(name, 'sessions'); openTranscript(name, s); };
      box.appendChild(sb);
    }
    if (sess.length > 12) box.appendChild(el('div', 'nav-hint', `+${sess.length - 12} more in sessions`));
  }
  $('.nav-signals').classList.toggle('on', sel.kind === 'signals');
  $('.nav-setup').classList.toggle('on', sel.kind === 'setup');
  $('#mast-home').classList.toggle('on', sel.kind === 'welcome');
}

// arrow keys walk the nav like a real instrument panel
$('#nav-list').addEventListener('keydown', (e) => {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  e.preventDefault();
  const items = [...document.querySelectorAll('#nav-list .nav-item')];
  const i = items.indexOf(document.activeElement);
  const next = items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length];
  next?.focus();
});
$('.nav-signals').onclick = () => selectSignals();
$('.nav-setup').onclick = () => selectSetup();
$('#nav-new-agent').onclick = () => { selectSetup(); $('#na-name').focus(); };
$('#mast-home').onclick = () => selectWelcome();
$('#welcome-new').onclick = () => { selectSetup(); $('#na-name').focus(); };
$('#welcome-kits').onclick = () => selectSetup();
$('#welcome-signals').onclick = () => selectSignals();
$('#na-create').onclick = async () => {
  const name = $('#na-name').value.trim();
  const model = $('#na-model').value.trim();
  const note = $('#na-note');
  if (!name) { note.textContent = 'name it first'; return; }
  note.textContent = 'creating…';
  const r = await fetch('/api/admin/agents', {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, ...(model ? { model } : {}) }),
  }).then((x) => x.json()).catch(() => ({ ok: false, error: 'unreachable' }));
  if (!r.ok) { note.textContent = r.error ?? 'failed'; return; }
  note.textContent = '';
  $('#na-name').value = ''; $('#na-model').value = '';
  await loadDiskAgents();
  selectAgent(name, 'configure');
  // Land on configure with an explicit confirmation — the nav selection
  // changing is otherwise the only signal the create succeeded.
  $('#cfg-note').textContent = `created ${name} — set its identity below, then converse`;
};

// ---------- view switching ----------
function show(view) {
  $('#view-welcome').hidden = view !== 'welcome';
  $('#view-converse').hidden = view !== 'converse';
  $('#view-sessions').hidden = view !== 'sessions';
  $('#view-rail').hidden = view !== 'rail';
  $('#view-setup').hidden = view !== 'setup';
  $('#view-configure').hidden = view !== 'configure';
}

// ---------- welcome (the front door) ----------
// The primary agent is the default profile's, else the first on disk, else
// whatever the bus named. Welcome orients and ROUTES — converse if you want
// to talk, configure if you want to set it up (Tim: depending what you're
// vibing) — it is never a dead end.
function primaryAgent() {
  const def = diskProfiles.find((p) => p.profile === 'default');
  if (def) return def.agent;
  if (diskProfiles[0]) return diskProfiles[0].agent;
  return [...agents.keys()][0] ?? null;
}

function selectWelcome() {
  sel = { kind: 'welcome' };
  $('#stage-title').textContent = 'welcome';
  $('#stage-note').textContent = 'orient, then dive in';
  $('#agent-tabs').hidden = true;
  show('welcome');
  renderWelcome();
  renderNav();
}

function renderWelcome() {
  const box = $('#welcome-agent');
  box.textContent = '';
  const name = primaryAgent();
  if (!name) {
    box.appendChild(el('div', 'dim-note', 'no agents yet — create your first one.'));
  } else {
    box.appendChild(el('div', 'welcome-agent-label', 'your agent'));
    const row = el('div', 'welcome-agent-row');
    row.appendChild(el('span', 'welcome-agent-name', name));
    const conv = el('button', '', `converse with ${name}`);
    conv.onclick = () => selectAgent(name, 'converse');
    const cfg = el('button', 'ghost', 'configure');
    cfg.onclick = () => selectAgent(name, 'configure');
    row.append(conv, cfg);
    box.appendChild(row);
  }
  $('#welcome-hint').textContent = historyOk === false
    ? 'transcripts are unavailable until the history view is on.'
    : '';
}

function selectSignals() {
  sel = { kind: 'signals' };
  $('#stage-title').textContent = 'signals';
  $('#stage-note').textContent = 'a live view of everything happening — orange means something needs your attention';
  $('#agent-tabs').hidden = true;
  setFilter('signals');
  show('rail');
  renderRail();
  renderNav();
}

function selectSetup() {
  sel = { kind: 'setup' };
  $('#stage-title').textContent = 'add-ons';
  $('#stage-note').textContent = 'add useful behavior and adjust its settings';
  $('#agent-tabs').hidden = true;
  show('setup');
  renderNav();
  loadSetup();
}

// ---------- setup: add-ons, package settings, and agent requests ----------
async function adminGet(p) {
  try { const r = await fetch(`/api/admin/${p}`); return await r.json(); } catch { return { ok: false }; }
}

async function loadSetup(opts = {}) {
  // A status line that SURVIVES the re-render below. Staging/approving reloads
  // this whole pane, which otherwise wipes a button's transient 'staged ✓' the
  // instant it appears — the "flash, then nothing" Tim saw. The banner is the
  // durable confirmation; it lives outside #setup-kits/#setup-pending so the
  // reload never touches it.
  const statusBox = $('#setup-status');
  if (statusBox) {
    statusBox.textContent = opts.status ?? '';
    statusBox.className = `setup-status${opts.statusKind ? ' status-' + opts.statusKind : ''}`;
    statusBox.hidden = !opts.status;
  }
  const kitsBox = $('#setup-kits');
  const pendBox = $('#setup-pending');
  const configBox = $('#setup-configs');
  kitsBox.textContent = 'resolving…';
  pendBox.textContent = 'checking…';
  configBox.textContent = 'checking…';

  const [kits, pkgs, proposals] = await Promise.all([adminGet('kits'), adminGet('packages'), adminGet('proposals')]);
  await loadDiskAgents();

  // Which kits already touched this root? Provenance is the ledger's
  // answer (decided_by = kit:<name>), staged shows as pending requests.
  const provenance = new Set();
  for (const p of pkgs.packages ?? [])
    for (const g of p.grants ?? [])
      if (g.decided_by?.startsWith('kit:')) provenance.add(g.decided_by.slice(4));

  kitsBox.textContent = '';
  for (const k of kits.kits ?? []) {
    const row = el('div', 'setup-kit');
    const head = el('div', 'setup-kit-head');
    head.appendChild(el('span', 'setup-kit-name', k.name));
    head.appendChild(el('span', 'setup-kit-hook dim-note', k.hook || ''));
    if (provenance.has(k.name)) head.appendChild(el('span', 'badge', 'installed'));
    const readmeBtn = el('button', 'ghost', 'details');
    const stageBtn = el('button', '', provenance.has(k.name) ? 'add again' : 'add');
    head.appendChild(readmeBtn);
    head.appendChild(stageBtn);
    row.appendChild(head);
    const pre = el('pre', 'setup-readme');
    pre.hidden = true;
    row.appendChild(pre);
    readmeBtn.onclick = async () => {
      if (pre.hidden && !pre.textContent) {
        pre.textContent = 'fetching…';
        const r = await fetch(`/api/admin/kits/readme?kit=${encodeURIComponent(k.name)}`)
          .then((x) => x.json()).catch(() => ({ ok: false }));
        pre.textContent = r.ok ? r.readme : (r.error ?? 'no readme');
      }
      pre.hidden = !pre.hidden;
    };
    stageBtn.onclick = async () => {
      stageBtn.disabled = true; stageBtn.textContent = 'adding...';
      const r = await fetch('/api/admin/kits/add', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ kit: k.name }),
      }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
      loadSetup(r.ok
        ? { status: `added ${k.name}.`, statusKind: 'ok' }
        : { status: `✕ couldn't add ${k.name}: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
    };
    kitsBox.appendChild(row);
  }
  if (!(kits.kits ?? []).length) {
    kitsBox.appendChild(el('div', 'dim-note', kits.ok === false
      ? `add-ons could not load: ${kits.error ?? 'unknown - is the elanus binary on the server PATH current?'}`
      : 'no add-ons found'));
  }

  configBox.textContent = '';
  if (pkgs.ok === false) {
    configBox.appendChild(el('div', 'dim-note', `could not load installed add-ons: ${pkgs.error ?? 'unknown error'}`));
  }
  let pkgAny = false;
  for (const p of pkgs.ok === false ? [] : (pkgs.packages ?? [])) {
    const active = (p.grants ?? []).some((g) => g.state === 'approved');
    pkgAny = true;
    const card = el('div', 'setup-pending-pkg');
    const head = el('div', 'setup-kit-head');
    head.appendChild(el('span', 'setup-kit-name', p.name));
    head.appendChild(el('span', active ? 'badge' : 'badge badge-wait', active ? 'on' : 'off'));
    card.appendChild(head);

    const form = el('div', 'setup-row');
    const key = el('input');
    key.placeholder = 'setting';
    key.spellcheck = false;
    const value = el('input');
    value.placeholder = 'value, using TOML for arrays or numbers';
    value.spellcheck = false;
    const save = el('button', '', 'save setting');
    const note = el('span', 'dim-note');
    save.onclick = async () => {
      if (!key.value.trim()) { note.textContent = 'name the setting first'; return; }
      save.disabled = true; save.textContent = 'saving...'; note.textContent = '';
      const r = await fetch('/api/admin/configs/set', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ package: p.name, key: key.value.trim(), value: value.value }),
      }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
      save.disabled = false; save.textContent = 'save setting';
      if (!r.ok) {
        note.textContent = r.error ?? 'save failed';
        return;
      }
      const loaded = await loadPackageConfig(p.name, raw);
      note.textContent = loaded && loaded.includes(key.value.trim()) ? 'saved' : 'saved, but could not verify the reload';
    };
    form.append(key, value, save, note);
    card.appendChild(form);
    const details = document.createElement('details');
    const summary = el('summary', 'dim-note', 'current settings');
    const raw = el('pre', 'setup-readme');
    raw.textContent = 'not loaded';
    details.append(summary, raw);
    details.addEventListener('toggle', () => { if (details.open) loadPackageConfig(p.name, raw); });
    card.appendChild(details);
    configBox.appendChild(card);
  }
  if (!pkgAny && pkgs.ok !== false) configBox.appendChild(el('div', 'dim-note', 'nothing added yet'));

  pendBox.textContent = '';
  if (proposals.ok === false) {
    pendBox.appendChild(el('div', 'dim-note', `could not load agent requests: ${proposals.error ?? 'unknown error'}`));
  }
  let requestAny = false;
  for (const p of proposals.ok === false ? [] : (proposals.proposals ?? [])) {
    requestAny = true;
    const card = el('div', 'setup-pending-pkg');
    const who = p.agent && typeof p.agent === 'string' ? p.agent : 'an agent';
    card.appendChild(el('div', 'setup-kit-name', `${who} wants to change settings`));
    card.appendChild(el('div', 'dim-note', (p.files ?? []).join(', ') || 'settings change'));
    const diff = el('pre', 'setup-readme');
    diff.hidden = true;
    const row = el('div', 'setup-row');
    const showBtn = el('button', 'ghost', 'show change');
    showBtn.onclick = async () => {
      if (diff.hidden && !diff.textContent) {
        diff.textContent = 'loading...';
        const r = await fetch(`/api/admin/proposals/show?id=${encodeURIComponent(p.proposal)}`)
          .then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
        diff.textContent = r.ok ? r.diff : (r.error ?? 'could not load the change');
      }
      diff.hidden = !diff.hidden;
    };
    const acceptBtn = el('button', '', 'accept');
    acceptBtn.onclick = async () => {
      acceptBtn.disabled = true; acceptBtn.textContent = 'accepting...';
      const r = await fetch('/api/admin/proposals/accept', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ id: p.proposal }),
      }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
      loadSetup(r.ok
        ? { status: 'accepted the change.', statusKind: 'ok' }
        : { status: `✕ couldn't accept it: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
    };
    const declineBtn = el('button', 'ghost', 'decline');
    declineBtn.onclick = async () => {
      declineBtn.disabled = true; declineBtn.textContent = 'declining...';
      const r = await fetch('/api/admin/proposals/decline', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ id: p.proposal }),
      }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
      loadSetup(r.ok
        ? { status: 'declined the change.', statusKind: 'ok' }
        : { status: `✕ couldn't decline it: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
    };
    row.append(showBtn, acceptBtn, declineBtn);
    card.appendChild(row);
    card.appendChild(diff);
    pendBox.appendChild(card);
  }
  if (!requestAny && proposals.ok !== false) pendBox.appendChild(el('div', 'dim-note', 'no agent requests'));
}

async function loadPackageConfig(pkg, raw) {
  raw.textContent = 'loading...';
  const r = await fetch(`/api/admin/configs?package=${encodeURIComponent(pkg)}`)
    .then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
  const text = r.ok ? (r.config?.toml || 'no settings yet') : null;
  raw.textContent = text ?? (r.error ?? 'could not load settings');
  return text;
}

function selectAgent(name, tab) {
  const prev = sel;
  sel = { kind: 'agent', agent: name, tab: tab ?? (prev.kind === 'agent' && prev.agent === name ? prev.tab : 'converse') };
  $('#stage-title').textContent = name;
  $('#agent-tabs').hidden = false;
  for (const b of document.querySelectorAll('#agent-tabs button')) b.classList.toggle('on', b.dataset.tab === sel.tab);
  renderNav();
  if (sel.tab === 'converse') {
    $('#stage-note').textContent = `in/agent/${name} ⇄ in/human — the mailbox view`;
    mountConvFeed(name);
    show('converse');
    $('#compose-input').focus();
  } else if (sel.tab === 'sessions') {
    $('#stage-note').textContent = 'your agent’s past conversations';
    show('sessions');
    loadSessions(name);
  } else if (sel.tab === 'configure') {
    $('#stage-note').textContent = 'who this agent is — model, mailbox, visibility';
    show('configure');
    loadConfigure(name);
  } else {
    $('#stage-note').textContent = `obs/agent/${name}/# — this agent's telemetry`;
    setFilter('all');
    show('rail');
    renderRail();
  }
}

// ---------- configure (per-agent identity) ----------
let cfgProfile = null; // the profile NAME backing the form
let cfgParsed = {};
let cfgPackages = [];
let cfgKits = [];
let cfgConfigPackages = new Set();
let cfgContextChain = [];
let cfgContextRemoved = new Set();
let cfgContextVarEdits = new Map();
const cfgKitDetails = new Map();
const PARENT_PATH = '$parent';
// The fields + both save buttons are LOCKED while the pane populates.
// loadConfigure does two round trips (profile fetch, then loadDiskAgents — ~1s)
// and only fills the fields at the very end; left editable, anything typed in
// that window is silently overwritten by the late resolve, i.e. invisible edit
// loss. Disabling is both the guard and the "still loading" affordance.
function setConfigureLoading(on) {
  document.querySelectorAll('#view-configure input, #view-configure textarea, #view-configure select, #view-configure button')
    .forEach((e) => { e.disabled = on; });
}

function csv(values) {
  return Array.isArray(values) ? values.join(', ') : '';
}

function iconButton(symbol, title, cls = 'icon-btn') {
  const b = el('button', cls, symbol);
  b.type = 'button';
  b.title = title;
  b.setAttribute('aria-label', title);
  return b;
}

function topicFilterMatches(filterText, value) {
  const f = String(filterText ?? '');
  if (f === '#') return true;
  const fp = f.split('/');
  const vp = String(value ?? '').split('/');
  for (let i = 0, j = 0; i < fp.length; i++, j++) {
    if (fp[i] === '#') return true;
    if (j >= vp.length) return false;
    if (fp[i] !== '+' && fp[i] !== vp[j]) return false;
  }
  return fp.length === vp.length;
}

function skillVisible(pkg) {
  const include = arr($('#cfg-include').value);
  const exclude = arr($('#cfg-exclude').value);
  const inc = include.length ? include : ['#'];
  return inc.some((p) => topicFilterMatches(p, pkg.name))
    && !exclude.some((p) => topicFilterMatches(p, pkg.name));
}

function skillIncluded(pkg) {
  const include = arr($('#cfg-include').value);
  const inc = include.length ? include : ['#'];
  return inc.some((p) => topicFilterMatches(p, pkg.name));
}

function skillExcluded(pkg) {
  return arr($('#cfg-exclude').value).some((p) => topicFilterMatches(p, pkg.name));
}

function setSkillExcluded(pkgName, excluded) {
  const next = arr($('#cfg-exclude').value).filter((p) => p !== pkgName);
  if (excluded) next.push(pkgName);
  $('#cfg-exclude').value = next.join(', ');
  renderPackageConfigTree();
  reconcileContextChainWithVisibility();
  renderContextChain();
}

function setKitPackagesExcluded(pkgs, excluded) {
  const names = new Set(pkgs.map((p) => p.name));
  const next = arr($('#cfg-exclude').value).filter((p) => !names.has(p));
  if (excluded) next.push(...[...names].sort());
  $('#cfg-exclude').value = next.join(', ');
  renderPackageConfigTree();
  reconcileContextChainWithVisibility();
  renderContextChain();
}

function currentLocalElanusPath() {
  const entries = arr($('#cfg-package-path').value);
  if ($('#cfg-path-inherit').checked) entries.push(PARENT_PATH);
  return entries;
}

async function saveAgentPath(entries) {
  const r = await fetch('/api/admin/agents/set', {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name: cfgProfile, set: prunedSet({ parent: $('#cfg-parent').value.trim(), elanus_path: entries }) }),
  }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
  return r;
}

function kitNameFor(pkg) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return parts[kits + 1];
  const source = packageSource(pkg);
  return source.kind === 'copied' ? 'instance' : source.label;
}

function packageSource(pkg) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return { kind: 'linked', label: parts[kits + 1], icon: '↗' };
  const packages = parts.lastIndexOf('packages');
  if (packages >= 0) return { kind: 'copied', label: 'instance', icon: '⬚' };
  return { kind: 'path', label: 'path entry', icon: '•' };
}

function packageBadges(pkg) {
  const badges = [];
  if (pkg.skill) badges.push({ cls: 'badge', text: 'skill' });
  const manifest = pkg.manifest ?? {};
  if (manifest.process?.mode) badges.push({ cls: 'badge badge-wait', text: manifest.process.mode === 'daemon' ? 'actor' : manifest.process.mode });
  if (manifest.process?.http) badges.push({ cls: 'badge badge-wait', text: 'http' });
  if (manifest.hooks) badges.push({ cls: 'badge badge-wait', text: 'hook' });
  if (manifest.cron) badges.push({ cls: 'badge badge-wait', text: 'cron' });
  if (manifest.providers) badges.push({ cls: 'badge badge-wait', text: 'provider' });
  if ((manifest.stages ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'stage' });
  if ((manifest.mcp ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'mcp' });
  return badges;
}

function packageDescription(pkg) {
  const manifest = pkg.manifest ?? {};
  if (manifest.description) return firstSentence(manifest.description);
  if (pkg.skill?.description) return pkg.skill.description;
  if (manifest.process?.mode === 'daemon') return 'resident actor on the bus';
  if (manifest.process?.mode === 'exec') return 'per-event script actor';
  if (manifest.hooks) return 'policy hook package';
  if ((manifest.stages ?? []).length) return 'context stage package';
  if ((manifest.mcp ?? []).length) return 'MCP tool server package';
  return 'package';
}

function firstSentence(text) {
  const compact = String(text ?? '').replace(/\s+/g, ' ').trim();
  if (!compact) return '';
  const sentence = compact.match(/^(.{20,220}?[.!?])\s/)?.[1] ?? compact;
  return sentence.length > 180 ? `${sentence.slice(0, 177)}...` : sentence;
}

function shortList(values, max = 2) {
  const list = (values ?? []).filter(Boolean);
  if (!list.length) return '';
  const shown = list.slice(0, max).join(', ');
  return list.length > max ? `${shown}, +${list.length - max}` : shown;
}

function actorDetail(pkg) {
  const manifest = pkg.manifest ?? {};
  const bits = [];
  const process = manifest.process;
  if (process?.mode === 'daemon') {
    bits.push(`runs ${process.run ?? 'its script'} as a resident actor`);
  } else if (process?.mode === 'exec') {
    bits.push(`runs ${process.run ?? 'its script'} for each matching event`);
  }
  const request = manifest.request ?? {};
  const subscribes = shortList(request.subscribe);
  if (subscribes) bits.push(`listens on ${subscribes}`);
  const publishes = shortList(request.publish);
  if (publishes) bits.push(`can emit ${publishes}`);
  const blocking = shortList(request.blocking);
  if (blocking) bits.push(`can block ${blocking}`);
  if (process?.http) bits.push('serves an approved local HTTP endpoint');
  if (manifest.hooks) bits.push(`declares ${manifest.hooks} hook${manifest.hooks === 1 ? '' : 's'}`);
  if ((manifest.stages ?? []).length) bits.push(`contributes ${manifest.stages.length} context stage${manifest.stages.length === 1 ? '' : 's'}`);
  if ((manifest.mcp ?? []).length) bits.push(`exposes ${manifest.mcp.length} MCP server${manifest.mcp.length === 1 ? '' : 's'}`);
  if (manifest.cron) bits.push(`schedules ${manifest.cron} recurring event${manifest.cron === 1 ? '' : 's'}`);
  const summary = bits.length ? `${bits.join('; ')}.` : packageDescription(pkg);
  const desc = manifest.description && firstSentence(manifest.description) !== manifest.description
    ? manifest.description
    : '';
  return desc ? `${summary} ${desc}` : summary;
}

function normalizedPreviewPackage(pkg) {
  const manifest = pkg.manifest ?? {};
  if (manifest.process || !manifest.mode) return pkg;
  return {
    ...pkg,
    manifest: {
      ...manifest,
      process: { mode: manifest.mode, run: manifest.run, http: manifest.http },
    },
  };
}

function tomlDisplayValue(value, type = 'string') {
  if (value === undefined || value === null) return '';
  if (type === 'array') return JSON.stringify(value);
  if (type === 'boolean') return value ? 'true' : 'false';
  if (type === 'number') return String(value);
  return String(value);
}

function declaredConfigParams(pkg) {
  const byKey = new Map();
  for (const key of pkg.manifest?.config?.agent_tunable ?? []) {
    if (!key) continue;
    byKey.set(key, {
      key,
      type: 'string',
      label: key,
      help: 'Package setting declared agent-tunable by the package manifest.',
      agent_tunable: true,
      source: 'package',
    });
  }
  for (const stage of pkg.manifest?.stages ?? []) {
    for (const param of stage.config ?? []) {
      if (!param.key) continue;
      byKey.set(param.key, {
        key: param.key,
        type: param.type ?? 'string',
        label: param.label || param.key,
        help: param.help || '',
        default: param.default,
        options: param.options ?? [],
        agent_tunable: param.agent_tunable === true,
        source: `context stage ${stage.name}`,
      });
    }
  }
  return [...byKey.values()].sort((a, b) => a.key.localeCompare(b.key));
}

function typedConfigInput(param, valueText) {
  if (param.type === 'boolean') {
    const label = el('label', 'cfg-check');
    const input = el('input');
    input.type = 'checkbox';
    input.checked = /^(true|1)$/i.test(String(valueText || tomlDisplayValue(param.default, param.type)));
    label.append(input, document.createTextNode(' enabled'));
    label.dataset.valueInput = '1';
    label.valueForSave = () => input.checked ? 'true' : 'false';
    return label;
  }
  if (param.type === 'enum' && (param.options ?? []).length) {
    const input = el('select');
    for (const option of param.options) input.appendChild(el('option', '', option));
    input.value = String(valueText || tomlDisplayValue(param.default, param.type) || param.options[0]);
    input.valueForSave = () => input.value;
    return input;
  }
  const input = el('input');
  input.spellcheck = false;
  if (param.type === 'number') input.type = 'number';
  input.value = valueText || tomlDisplayValue(param.default, param.type);
  input.placeholder = param.default === undefined || param.default === null
    ? 'value'
    : `default ${tomlDisplayValue(param.default, param.type)}`;
  input.valueForSave = () => input.value;
  return input;
}

function parseConfigRows(raw = '') {
  const rows = [];
  for (const line of String(raw).split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('[')) continue;
    const m = trimmed.match(/^([A-Za-z0-9_.-]+)\s*=\s*(.*)$/);
    if (!m) continue;
    rows.push({ key: m[1], value: m[2] });
  }
  return rows;
}

function kitIsInstalled(k, detail = null) {
  const kitDir = String(k?.dir ?? '');
  const packageDir = `${kitDir.replace(/[\\/]$/, '')}${kitDir ? '/' : ''}packages`;
  if (kitDir && arr($('#cfg-effective-path').value).some((p) => p === kitDir || p === packageDir)) return true;
  const names = new Set((detail?.packages ?? []).map((p) => p.name));
  return names.size > 0 && cfgPackages.some((p) => names.has(p.name));
}

async function loadKitDetail(k) {
  if (cfgKitDetails.has(k.name)) return cfgKitDetails.get(k.name);
  const r = await fetch(`/api/admin/kits/packages?kit=${encodeURIComponent(k.name)}`)
    .then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
  const detail = r.ok ? r.kit : { ...k, packages: [], error: r.error ?? 'could not load kit' };
  cfgKitDetails.set(k.name, detail);
  return detail;
}

function focusPackageConfigs(kitName) {
  const target = document.querySelector(`#cfg-package-configs details[data-kit="${CSS.escape(kitName)}"]`);
  if (target) target.open = true;
  location.hash = 'cfg-section-packages';
  document.querySelector('#cfg-section-packages')?.scrollIntoView({ block: 'start' });
}

async function installKitForAgent(k, mode, note, controls = []) {
  if (!cfgProfile) return;
  const buttons = Array.isArray(controls) ? controls : [controls];
  buttons.forEach((button) => { if (button) button.disabled = true; });
  note.textContent = mode === 'copy' ? 'copying...' : 'linking...';
  if (mode === 'copy') {
    const r = await fetch('/api/admin/kits/add', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ kit: k.name, copy: true }),
    }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
    note.textContent = r.ok ? `copied ${k.name}` : (r.error ?? 'copy failed');
    buttons.forEach((button) => { if (button) button.disabled = false; });
    if (r.ok) await loadConfigure(sel.agent ?? cfgParsed.agent ?? cfgProfile);
    return;
  }
  const entries = currentLocalElanusPath().filter((p) => p !== k.dir);
  const insertAt = entries.indexOf(PARENT_PATH);
  if (insertAt >= 0) entries.splice(insertAt, 0, k.dir);
  else entries.unshift(k.dir);
  const r = await saveAgentPath(entries);
  note.textContent = r.ok ? `linked ${k.name}` : (r.error ?? 'link failed');
  buttons.forEach((button) => { if (button) button.disabled = false; });
  if (r.ok) await loadConfigure(sel.agent ?? cfgParsed.agent ?? cfgProfile);
}

async function renderKitAddList() {
  const box = $('#cfg-kit-add-list');
  box.textContent = '';
  if (!cfgKits.length) {
    box.appendChild(el('div', 'dim-note', 'no kits found'));
    return;
  }
  for (const k of cfgKits) {
    const details = document.createElement('details');
    details.className = 'cfg-add-kit';
    const summary = el('summary', 'cfg-kit-summary cfg-kit-head');
    summary.appendChild(el('span', '', k.name));
    summary.appendChild(el('span', 'cfg-pkg-desc', k.hook || ''));
    const actions = el('span', 'cfg-kit-actions');
    const install = el('span', 'cfg-split-action');
    const primary = el('button', 'cfg-split-primary cfg-kit-add-btn', 'link');
    primary.type = 'button';
    primary.title = `link ${k.name}`;
    primary.setAttribute('aria-label', `link ${k.name}`);
    const menuToggle = iconButton('⌄', `more add actions for ${k.name}`, 'cfg-split-caret');
    const menu = el('div', 'cfg-action-menu');
    menu.hidden = true;
    for (const action of ['link', 'copy']) {
      const item = el('button', '', action);
      item.type = 'button';
      item.setAttribute('aria-label', `${action} ${k.name}`);
      item.onclick = async (e) => {
        e.preventDefault();
        e.stopPropagation();
        menu.hidden = true;
        await installKitForAgent(k, action, note, [primary, menuToggle, ...menu.querySelectorAll('button')]);
      };
      menu.appendChild(item);
    }
    install.append(primary, menuToggle, menu);
    const gear = iconButton('⚙', `configure ${k.name}`, 'cfg-icon-btn cfg-kit-gear');
    const badge = el('span', 'badge', 'installed');
    badge.hidden = true;
    gear.hidden = true;
    actions.append(install, badge, gear);
    summary.appendChild(actions);
    details.appendChild(summary);
    const skillsBox = el('div', 'cfg-skill-table');
    skillsBox.appendChild(el('div', 'dim-note', 'expand to load packages'));
    const note = el('div', 'dim-note');
    details.append(skillsBox, note);
    const refreshInstallState = (detail) => {
      const installed = kitIsInstalled(k, detail);
      badge.hidden = !installed;
      gear.hidden = !installed;
      install.hidden = installed;
    };
    refreshInstallState(null);
    details.addEventListener('toggle', async () => {
      if (!details.open || details.dataset.loaded) return;
      skillsBox.textContent = 'loading...';
      const detail = await loadKitDetail(k);
      details.dataset.loaded = '1';
      skillsBox.textContent = '';
      const packages = detail.packages ?? [];
      if (!packages.length) {
        skillsBox.appendChild(el('div', 'dim-note', detail.error ?? 'no packages in this kit'));
      } else {
        for (const p of packages) {
          const row = el('div', 'cfg-skill-row cfg-kit-preview-row');
          const previewPkg = normalizedPreviewPackage(p);
          const name = el('span', 'cfg-pkg-name', p.name);
          const badges = el('span', 'cfg-preview-badges');
          if (p.skill) badges.appendChild(el('span', 'badge', 'skill'));
          if (p.manifest?.actor) badges.appendChild(el('span', 'badge badge-wait', p.manifest.actor));
          name.appendChild(badges);
          row.appendChild(name);
          row.appendChild(el('span', 'cfg-pkg-desc', actorDetail(previewPkg)));
          row.appendChild(el('span', 'cfg-skill-actions', ''));
          skillsBox.appendChild(row);
        }
      }
      refreshInstallState(detail);
    });
    primary.onclick = async (e) => {
      e.preventDefault();
      e.stopPropagation();
      await installKitForAgent(k, 'link', note, [primary, menuToggle, ...menu.querySelectorAll('button')]);
    };
    menuToggle.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      menu.hidden = !menu.hidden;
    };
    gear.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      focusPackageConfigs(k.name);
    };
    box.appendChild(details);
  }
}

function groupByKit(packages) {
  const groups = new Map();
  for (const p of packages) {
    const kit = kitNameFor(p);
    if (!groups.has(kit)) groups.set(kit, []);
    groups.get(kit).push(p);
  }
  return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b));
}

function varRow(key = '', value = '') {
  const row = el('div', 'cfg-var-row');
  row.innerHTML = '<input class="cfg-var-key" placeholder="name" spellcheck="false" />'
    + '<input class="cfg-var-value" placeholder="value" spellcheck="false" />';
  row.querySelector('.cfg-var-key').value = key;
  row.querySelector('.cfg-var-value').value = value ?? '';
  return row;
}

function renderVars(vars = {}) {
  const box = $('#cfg-vars');
  box.textContent = '';
  for (const [k, v] of Object.entries(vars ?? {}).sort(([a], [b]) => a.localeCompare(b))) {
    box.appendChild(varRow(k, v));
  }
  if (!box.children.length) box.appendChild(varRow());
}

function throttleRow(name = '', t = {}) {
  const row = el('div', 'cfg-throttle-row');
  row.innerHTML = '<input class="cfg-throttle-name" placeholder="name" spellcheck="false" />'
    + '<input class="cfg-throttle-max" type="number" placeholder="max concurrent" />'
    + '<input class="cfg-throttle-rate" type="number" placeholder="rate/min" />'
    + '<input class="cfg-throttle-tokens" type="number" placeholder="tokens/hour" />'
    + '<label class="cfg-check"><input class="cfg-throttle-coalesce" type="checkbox" /> coalesce</label>';
  row.querySelector('.cfg-throttle-name').value = name;
  row.querySelector('.cfg-throttle-max').value = t?.max_concurrent ?? '';
  row.querySelector('.cfg-throttle-rate').value = t?.rate_per_min ?? '';
  row.querySelector('.cfg-throttle-tokens').value = t?.llm_tokens_per_hour ?? '';
  row.querySelector('.cfg-throttle-coalesce').checked = t?.coalesce === true;
  return row;
}

function renderThrottle(throttle = {}) {
  const box = $('#cfg-throttle');
  box.textContent = '';
  for (const [k, v] of Object.entries(throttle ?? {}).sort(([a], [b]) => a.localeCompare(b))) {
    box.appendChild(throttleRow(k, v));
  }
  if (!box.children.length) box.appendChild(throttleRow());
}

function contextStageKey(s) {
  return `${s.package}/${s.name}`;
}

function contextStageDefs() {
  const defs = [];
  for (const p of cfgPackages.filter(skillVisible)) {
    for (const stage of p.manifest?.stages ?? []) {
      defs.push({
        package: p.name,
        name: stage.name,
        mode: stage.mode ?? 'exec',
        order: Number(stage.order ?? 50),
        timeout_ms: stage.mode === 'resident' ? 15000 : 10000,
        config: stage.config ?? [],
      });
    }
  }
  return defs.sort((a, b) => (a.order - b.order)
    || a.package.localeCompare(b.package)
    || a.name.localeCompare(b.name));
}

function contextStageConfigParams(stage) {
  return (stage.config ?? [])
    .filter((param) => param.key)
    .map((param) => ({
      key: param.key,
      type: param.type ?? 'string',
      label: param.label || param.key,
      help: param.help || '',
      default: param.default,
      options: param.options ?? [],
      agent_tunable: param.agent_tunable === true,
      source: `context stage ${stage.name}`,
    }));
}

function resetContextChain(context = {}) {
  const defs = contextStageDefs();
  const overrides = new Map((context.stages ?? []).map((s) => [contextStageKey(s), s]));
  cfgContextRemoved = new Set((context.stages ?? [])
    .filter((s) => s.enabled === false)
    .map(contextStageKey));
  cfgContextVarEdits = new Map();
  cfgContextChain = [];
  for (const def of defs) {
    const ov = overrides.get(contextStageKey(def));
    if (ov?.enabled === false) continue;
    cfgContextChain.push({
      ...def,
      enabled: true,
      order: Number(ov?.order ?? def.order),
      timeout_ms: Number(ov?.timeout_ms ?? def.timeout_ms),
    });
  }
  cfgContextChain.sort((a, b) => (a.order - b.order)
    || a.package.localeCompare(b.package)
    || a.name.localeCompare(b.name));
}

function reconcileContextChainWithVisibility() {
  const defs = contextStageDefs();
  const defsByKey = new Map(defs.map((s) => [contextStageKey(s), s]));
  const disabledFromProfile = new Set((cfgParsed.context?.stages ?? [])
    .filter((s) => s.enabled === false)
    .map(contextStageKey));
  const seen = new Set();
  cfgContextChain = cfgContextChain
    .filter((stage) => defsByKey.has(contextStageKey(stage)))
    .map((stage) => {
      const def = defsByKey.get(contextStageKey(stage));
      seen.add(contextStageKey(stage));
      return { ...def, ...stage, enabled: true };
    });
  for (const def of defs) {
    const key = contextStageKey(def);
    if (!seen.has(key) && !disabledFromProfile.has(key) && !cfgContextRemoved.has(key)) {
      cfgContextChain.push({ ...def, enabled: true });
    }
  }
  cfgContextChain.sort((a, b) => (Number(a.order ?? 50) - Number(b.order ?? 50))
    || a.package.localeCompare(b.package)
    || a.name.localeCompare(b.name));
}

function renumberContextChain() {
  cfgContextChain.forEach((s, i) => { s.order = (i + 1) * 10; });
}

function moveContextStage(index, dir) {
  const next = index + dir;
  if (next < 0 || next >= cfgContextChain.length) return;
  const tmp = cfgContextChain[index];
  cfgContextChain[index] = cfgContextChain[next];
  cfgContextChain[next] = tmp;
  renumberContextChain();
  renderContextChain();
}

function removeContextStage(index) {
  const [removed] = cfgContextChain.splice(index, 1);
  if (removed) cfgContextRemoved.add(contextStageKey(removed));
  renumberContextChain();
  renderContextChain();
}

function addContextStage(key) {
  const def = contextStageDefs().find((s) => contextStageKey(s) === key);
  if (!def || cfgContextChain.some((s) => contextStageKey(s) === key)) return;
  cfgContextRemoved.delete(key);
  cfgContextChain.push({ ...def, enabled: true, order: (cfgContextChain.length + 1) * 10 });
  renderContextChain();
}

function renderContextChain() {
  const box = $('#cfg-context-chain');
  const addSelect = $('#cfg-context-add-stage');
  if (!box || !addSelect) return;
  const defs = contextStageDefs();
  const inChain = new Set(cfgContextChain.map(contextStageKey));
  const available = defs.filter((s) => !inChain.has(contextStageKey(s)));
  addSelect.textContent = '';
  for (const s of available) {
    const opt = el('option', '', `${s.package}/${s.name}`);
    opt.value = contextStageKey(s);
    addSelect.appendChild(opt);
  }
  addSelect.disabled = !available.length;
  $('#cfg-context-add').disabled = !available.length;

  box.textContent = '';
  if (!defs.length) {
    box.appendChild(el('div', 'dim-note', 'no visible package context stages'));
    return;
  }
  if (!cfgContextChain.length) {
    box.appendChild(el('div', 'dim-note', 'all visible context stages are removed for this agent'));
  }
  cfgContextChain.forEach((stage, index) => {
    const tile = el('div', 'cfg-context-stage');
    tile.dataset.stage = contextStageKey(stage);
    const head = el('div', 'cfg-context-stage-head');
    const title = el('div', 'cfg-context-stage-title');
    title.appendChild(el('strong', '', `${stage.package}/${stage.name}`));
    title.appendChild(el('span', 'cfg-config-help', `mode: ${stage.mode} · ${stage.config.length} declared setting${stage.config.length === 1 ? '' : 's'}`));
    const actions = el('div', 'cfg-context-stage-actions');
    const up = iconButton('↑', `move ${stage.package}/${stage.name} up`, 'cfg-icon-btn');
    const down = iconButton('↓', `move ${stage.package}/${stage.name} down`, 'cfg-icon-btn');
    const remove = iconButton('×', `remove ${stage.package}/${stage.name}`, 'cfg-icon-btn');
    up.disabled = index === 0;
    down.disabled = index === cfgContextChain.length - 1;
    up.onclick = () => moveContextStage(index, -1);
    down.onclick = () => moveContextStage(index, 1);
    remove.onclick = () => removeContextStage(index);
    actions.append(up, down, remove);
    head.append(title, actions);
    const controls = el('div', 'cfg-context-stage-grid');
    const order = el('label', '', 'order');
    const orderInput = el('input');
    orderInput.type = 'number';
    orderInput.min = '1';
    orderInput.dataset.contextField = 'order';
    orderInput.value = stage.order;
    orderInput.oninput = () => { stage.order = Number(orderInput.value || stage.order); };
    order.appendChild(orderInput);
    const timeout = el('label', '', 'timeout ms');
    const timeoutInput = el('input');
    timeoutInput.type = 'number';
    timeoutInput.min = '1';
    timeoutInput.dataset.contextField = 'timeout_ms';
    timeoutInput.value = stage.timeout_ms;
    timeoutInput.oninput = () => { stage.timeout_ms = Number(timeoutInput.value || stage.timeout_ms); };
    timeout.appendChild(timeoutInput);
    controls.append(order, timeout);
    tile.append(head, controls);
    const params = contextStageConfigParams(stage);
    if (params.length) {
      const settings = el('div', 'cfg-context-stage-config');
      settings.appendChild(el('div', 'cfg-context-stage-subhead', 'settings'));
      for (const param of params) {
        const keyName = param.key;
        const valueText = cfgParsed.vars?.[keyName] ?? '';
        const row = el('div', 'cfg-config-row');
        const label = el('label', '', param.label || keyName);
        label.title = keyName;
        const help = el('span', 'cfg-config-help');
        help.textContent = [
          param.source,
          param.type ? `type: ${param.type}` : '',
          param.agent_tunable ? 'agent-tunable' : '',
          param.help,
        ].filter(Boolean).join(' · ');
        label.appendChild(help);
        const input = typedConfigInput(param, cfgContextVarEdits.get(keyName) ?? valueText);
        input.dataset.contextVar = keyName;
        input.dataset.contextStage = contextStageKey(stage);
        const markDirty = () => {
          input.dataset.dirty = '1';
          const value = typeof input.valueForSave === 'function' ? input.valueForSave() : input.value;
          cfgContextVarEdits.set(keyName, value ?? '');
        };
        input.addEventListener('input', markDirty);
        input.addEventListener('change', markDirty);
        row.append(label, input, el('span'), el('span'));
        settings.appendChild(row);
      }
      tile.appendChild(settings);
    }
    box.appendChild(tile);
  });
}

function syncContextChainFromDom() {
  const byKey = new Map(cfgContextChain.map((s) => [contextStageKey(s), s]));
  for (const tile of document.querySelectorAll('#cfg-context-chain .cfg-context-stage')) {
    const stage = byKey.get(tile.dataset.stage);
    if (!stage) continue;
    const order = Number(tile.querySelector('[data-context-field="order"]')?.value);
    const timeout = Number(tile.querySelector('[data-context-field="timeout_ms"]')?.value);
    if (Number.isFinite(order) && order > 0) stage.order = order;
    if (Number.isFinite(timeout) && timeout > 0) stage.timeout_ms = timeout;
  }
}

function contextStageOverridesForSave() {
  syncContextChainFromDom();
  const chain = new Map(cfgContextChain.map((s) => [contextStageKey(s), s]));
  const rows = [];
  for (const current of cfgContextChain) {
    rows.push({
      package: current.package,
      name: current.name,
      enabled: true,
      order: Number(current.order || 50),
      timeout_ms: Number(current.timeout_ms || (current.mode === 'resident' ? 15000 : 10000)),
    });
  }
  for (const def of contextStageDefs()) {
    if (!chain.has(contextStageKey(def))) rows.push({ package: def.package, name: def.name, enabled: false });
  }
  return rows;
}

function contextStageVarsForSave() {
  const out = new Map(cfgContextVarEdits);
  for (const input of document.querySelectorAll('#cfg-context-chain [data-context-var][data-dirty="1"]')) {
    const key = input.dataset.contextVar;
    if (!key) continue;
    const value = typeof input.valueForSave === 'function' ? input.valueForSave() : input.value;
    out.set(key, value ?? '');
  }
  return out;
}

function packageConfigControls(p) {
  const card = document.createElement('details');
  card.className = 'cfg-package-card';
  card.dataset.package = p.name;
  const disabled = skillExcluded(p);
  card.classList.toggle('is-disabled', disabled);
  const head = el('summary', 'cfg-package-head');
  head.appendChild(el('span', 'cfg-disclosure', '▸'));
  const source = packageSource(p);
  const sourceIcon = el('span', `cfg-source-icon source-${source.kind}`, source.icon);
  sourceIcon.title = `${source.kind}: ${source.label}`;
  head.appendChild(sourceIcon);
  const title = el('span', 'cfg-package-title');
  title.appendChild(el('span', 'setup-kit-name', p.name));
  title.appendChild(el('span', 'cfg-pkg-desc', packageDescription(p)));
  head.appendChild(title);
  card.appendChild(head);

  const body = el('div', 'cfg-package-body');
  body.appendChild(el('div', 'cfg-package-detail', actorDetail(p)));
  const meta = el('div', 'cfg-package-meta');
  for (const b of packageBadges(p)) meta.appendChild(el('span', b.cls, b.text));
  if (meta.children.length) body.appendChild(meta);
  const declared = declaredConfigParams(p);
  const canConfigure = declared.length > 0 || cfgConfigPackages.has(p.name);
  const controls = el('div', 'cfg-package-controls');
  const visibility = el('span', 'dim-note', disabled ? 'disabled for this agent' : 'enabled for this agent');
  const toggle = el('button', disabled ? 'ghost cfg-package-disable' : 'cfg-package-disable', disabled ? 'enable' : 'disable');
  toggle.title = disabled ? `remove ${p.name} from skills.exclude` : `add ${p.name} to skills.exclude`;
  const configure = el('button', 'ghost cfg-package-config-toggle', 'settings');
  configure.hidden = !canConfigure;
  controls.append(visibility, toggle, configure);
  body.appendChild(controls);
  const panel = el('div', 'cfg-package-config-panel');
  panel.hidden = true;
  panel.textContent = 'loading...';
  body.appendChild(panel);
  card.appendChild(body);
  const toggleConfigPanel = async () => {
    card.open = true;
    panel.hidden = !panel.hidden;
    if (panel.hidden || panel.dataset.loaded) return;
    const r = await fetch(`/api/admin/configs?package=${encodeURIComponent(p.name)}`)
      .then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
    const raw = r.ok ? (r.config?.toml || '') : '';
    const current = parseConfigRows(raw);
    const byKey = new Map(current.map((row) => [row.key, row.value]));
    const params = new Map(declared.map((param) => [param.key, param]));
    for (const param of declared) if (!byKey.has(param.key)) byKey.set(param.key, '');
    for (const key of byKey.keys()) {
      if (!params.has(key)) params.set(key, {
        key,
        type: 'string',
        label: key,
        help: 'Existing package setting not declared in the manifest.',
        source: 'current settings',
      });
    }
    panel.textContent = '';
    if (!byKey.size) {
      panel.appendChild(el('div', 'dim-note', r.ok ? 'no configurable settings declared' : (r.error ?? 'could not load settings')));
      panel.dataset.loaded = '1';
      return;
    }
    for (const param of [...params.values()].sort((a, b) => a.key.localeCompare(b.key))) {
      const keyName = param.key;
      const valueText = byKey.get(keyName) ?? '';
      const row = el('div', 'cfg-config-row');
      const label = el('label', '', param.label || keyName);
      label.title = keyName;
      const help = el('span', 'cfg-config-help');
      help.textContent = [
        param.source,
        param.type ? `type: ${param.type}` : '',
        param.agent_tunable ? 'agent-tunable' : '',
        param.help,
      ].filter(Boolean).join(' · ');
      label.appendChild(help);
      const input = typedConfigInput(param, valueText);
      const save = iconButton('✓', `save ${p.name}.${keyName}`, 'cfg-icon-btn');
      const note = el('span', 'dim-note');
      save.onclick = async () => {
        save.disabled = true;
        note.textContent = 'saving...';
        const value = typeof input.valueForSave === 'function' ? input.valueForSave() : input.value;
        const write = await fetch('/api/admin/configs/set', {
          method: 'POST', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ package: p.name, key: keyName, value }),
        }).then((x) => x.json()).catch(() => ({ ok: false, error: 'server unreachable' }));
        save.disabled = false;
        note.textContent = write.ok ? 'saved' : (write.error ?? 'save failed');
        if (write.ok) cfgConfigPackages.add(p.name);
      };
      row.append(label, input, save, note);
      panel.appendChild(row);
    }
    panel.dataset.loaded = '1';
  };
  configure.onclick = async (e) => {
    e.preventDefault();
    e.stopPropagation();
    await toggleConfigPanel();
  };
  toggle.onclick = (e) => {
    e.preventDefault();
    e.stopPropagation();
    setSkillExcluded(p.name, !disabled);
  };
  return card;
}

function renderPackageConfigTree() {
  const box = $('#cfg-package-configs');
  box.textContent = '';
  const packages = cfgPackages.filter(skillIncluded);
  if (!packages.length) {
    box.appendChild(el('div', 'dim-note', 'no packages found'));
    return;
  }
  for (const [kit, pkgs] of groupByKit(packages)) {
    const details = document.createElement('details');
    details.className = 'cfg-package-group';
    details.dataset.kit = kit;
    details.open = true;
    const disabledCount = pkgs.filter((p) => skillExcluded(p)).length;
    const kitHead = el('summary', 'cfg-kit-summary cfg-kit-head');
    kitHead.appendChild(el('span', 'cfg-disclosure', '▸'));
    kitHead.appendChild(el('span', 'cfg-kit-name', kit));
    kitHead.appendChild(el('span', 'cfg-pkg-desc', `${pkgs.length} package${pkgs.length === 1 ? '' : 's'}`));
    const kitToggle = iconButton(
      disabledCount === pkgs.length ? '✓' : '⊘',
      disabledCount === pkgs.length
        ? `enable all ${kit} packages`
        : `disable all ${kit} packages`,
      disabledCount === pkgs.length ? 'ghost cfg-icon-btn cfg-kit-toggle' : 'cfg-icon-btn cfg-kit-toggle',
    );
    kitToggle.title = disabledCount === pkgs.length
      ? `remove all ${kit} packages from skills.exclude`
      : `add all ${kit} packages to skills.exclude`;
    kitToggle.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      setKitPackagesExcluded(pkgs, disabledCount !== pkgs.length);
    };
    kitHead.appendChild(kitToggle);
    details.appendChild(kitHead);
    const table = el('div', 'cfg-package-table');
    for (const p of [...pkgs].sort((a, b) => a.name.localeCompare(b.name))) {
      table.appendChild(packageConfigControls(p));
    }
    details.appendChild(table);
    box.appendChild(details);
  }
}

async function loadConfigure(agentName) {
  const p = profileOf(agentName);
  cfgProfile = p?.profile ?? agentName;
  cfgParsed = {};
  $('#cfg-note').textContent = 'loading…';
  $('#cfg-file').textContent = `${cfgProfile} settings`;
  setConfigureLoading(true);
  const [r, pkgs, kits, configs] = await Promise.all([
    fetch(`/api/admin/profile?name=${encodeURIComponent(cfgProfile)}`)
      .then((x) => x.json()).catch(() => ({ ok: false })),
    fetch(`/api/admin/packages?profile=${encodeURIComponent(cfgProfile)}`)
      .then((x) => x.json()).catch(() => ({ ok: false })),
    adminGet('kits'),
    adminGet('configs'),
  ]);
  cfgPackages = pkgs.ok === false ? [] : (pkgs.packages ?? []);
  cfgKits = kits.ok === false ? [] : (kits.kits ?? []);
  cfgConfigPackages = new Set((configs.ok === false ? [] : (configs.configs ?? [])).map((c) => c.package).filter(Boolean));
  $('#cfg-toml').value = r.ok ? r.toml : '';
  // The parsed view comes from profile list (same loader the kernel uses).
  await loadDiskAgents();
  const d = r.profile ?? profileOf(agentName) ?? {};
  cfgParsed = d;
  $('#cfg-agent').value = d.agent ?? agentName;
  $('#cfg-owner').value = d.owner ?? 'owner';
  $('#cfg-parent').value = d.parent ?? '';
  $('#cfg-autonomy').value = d.autonomy ?? 'off';
  const localPath = d.local_elanus_path ?? null;
  const localEntries = Array.isArray(localPath) ? localPath.filter((x) => x !== PARENT_PATH) : [];
  $('#cfg-package-path').value = csv(localEntries);
  $('#cfg-path-inherit').checked = !Array.isArray(localPath) || localPath.includes(PARENT_PATH);
  $('#cfg-effective-path').value = csv(d.elanus_path ?? d.package_path ?? ['packages']);
  $('#cfg-model').value = d.model ?? '';
  $('#cfg-turns').value = d.max_turns ?? '';
  $('#cfg-base-url').value = d.base_url ?? '';
  $('#cfg-api-key-env').value = d.api_key_env ?? '';
  $('#cfg-context-program').value = d.context?.program ?? 'default';
  $('#cfg-context-max-ms').value = d.context?.max_total_ms ?? 30000;
  resetContextChain(d.context ?? {});
  $('#cfg-workdir').value = d.workdir ?? '';
  $('#cfg-fs-write').value = csv(d.fs_write ?? []);
  $('#cfg-capture-exclude').value = csv(d.capture_exclude ?? []);
  $('#cfg-include').value = csv(d.skills?.include ?? ['#']);
  $('#cfg-exclude').value = csv(d.skills?.exclude ?? []);
  renderVars(d.vars ?? {});
  renderThrottle(d.throttle ?? {});
  renderKitAddList();
  renderContextChain();
  renderPackageConfigTree();
  setConfigureLoading(false);
  // Clear only our own 'loading…' — never stomp a message a caller set while we
  // were awaiting (e.g. na-create's "created <name> — set its identity below").
  if ($('#cfg-note').textContent === 'loading…') {
    $('#cfg-note').textContent = r.ok ? '' : `no settings file for ${cfgProfile} — this agent only exists as traffic; create an agent here to configure it`;
  }
}

$('#cfg-save').onclick = async () => {
  if (!cfgProfile) return;
  const note = $('#cfg-note');
  note.textContent = 'saving…';
  const set = {};
  const newAgent = $('#cfg-agent').value.trim();
  if (newAgent) set['agent'] = newAgent;
  if ($('#cfg-owner').value.trim()) set['owner'] = $('#cfg-owner').value.trim();
  set['parent'] = $('#cfg-parent').value.trim();
  set['autonomy'] = $('#cfg-autonomy').value;
  const localPath = arr($('#cfg-package-path').value);
  if ($('#cfg-path-inherit').checked) localPath.push(PARENT_PATH);
  set['elanus_path'] = localPath;
  if ($('#cfg-model').value.trim()) set['model.model'] = $('#cfg-model').value.trim();
  if ($('#cfg-turns').value) set['model.max_turns'] = Number($('#cfg-turns').value);
  if ($('#cfg-base-url').value.trim()) set['model.base_url'] = $('#cfg-base-url').value.trim();
  if ($('#cfg-api-key-env').value.trim()) set['model.api_key_env'] = $('#cfg-api-key-env').value.trim();
  if ($('#cfg-context-program').value.trim()) set['context.program'] = $('#cfg-context-program').value.trim();
  if ($('#cfg-context-max-ms').value) set['context.max_total_ms'] = Number($('#cfg-context-max-ms').value);
  set['context.stage'] = contextStageOverridesForSave();
  set['sandbox.workdir'] = $('#cfg-workdir').value.trim();
  set['sandbox.fs_write'] = arr($('#cfg-fs-write').value);
  set['sandbox.capture_exclude'] = arr($('#cfg-capture-exclude').value);
  // Send as a real JS array — server's tomlValue sees Array.isArray and
  // encodes it as a TOML array. Sending a JSON-stringified string here was
  // the regression: it arrived as the string '["#"]' and the kernel refused.
  set['skills.include'] = arr($('#cfg-include').value).length ? arr($('#cfg-include').value) : ['#'];
  // Always sent: an EMPTY exclude list is a meaningful save (clearing it),
  // and an omitted key would silently keep the old value.
  set['skills.exclude'] = arr($('#cfg-exclude').value);
  for (const row of document.querySelectorAll('#cfg-vars .cfg-var-row')) {
    const k = row.querySelector('.cfg-var-key')?.value.trim();
    if (!k) continue;
    set[`vars.${k}`] = row.querySelector('.cfg-var-value')?.value ?? '';
  }
  for (const [k, v] of contextStageVarsForSave()) {
    set[`vars.${k}`] = v;
  }
  for (const row of document.querySelectorAll('#cfg-throttle .cfg-throttle-row')) {
    const name = row.querySelector('.cfg-throttle-name')?.value.trim();
    if (!name) continue;
    const max = row.querySelector('.cfg-throttle-max')?.value;
    const rate = row.querySelector('.cfg-throttle-rate')?.value;
    const tokens = row.querySelector('.cfg-throttle-tokens')?.value;
    const coalesce = row.querySelector('.cfg-throttle-coalesce')?.checked === true;
    if (max) set[`throttle.${name}.max_concurrent`] = Number(max);
    if (rate) set[`throttle.${name}.rate_per_min`] = Number(rate);
    if (tokens) set[`throttle.${name}.llm_tokens_per_hour`] = Number(tokens);
    if (coalesce || cfgParsed.throttle?.[name]?.coalesce != null) set[`throttle.${name}.coalesce`] = coalesce;
  }
  const r = await fetch('/api/admin/agents/set', {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name: cfgProfile, set: prunedSet(set) }),
  }).then((x) => x.json()).catch(() => ({ ok: false, error: 'unreachable' }));
  if (!r.ok) { note.textContent = r.error ?? 'save failed'; return; }
  note.textContent = 'saved — applies on the next run';
  const renamed = newAgent && sel.kind === 'agent' && newAgent !== sel.agent;
  await loadDiskAgents();
  if (renamed) selectAgent(newAgent, 'configure');
};
// Drop empty-string entries except workdir-clearing, and send arrays as TOML text.
function prunedSet(set) {
  const out = {};
  for (const [k, v] of Object.entries(set)) {
    if (v === '' && k !== 'sandbox.workdir' && k !== 'parent') continue;
    out[k] = v;
  }
  return out;
}

$('#cfg-toml-save').onclick = async () => {
  if (!cfgProfile) return;
  const note = $('#cfg-toml-note');
  note.textContent = 'saving…';
  const r = await fetch(`/api/admin/profile?name=${encodeURIComponent(cfgProfile)}`, {
    method: 'PUT', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ toml: $('#cfg-toml').value }),
  }).then((x) => x.json()).catch(() => ({ ok: false }));
  note.textContent = r.ok ? 'saved' : 'save failed';
  if (r.ok) loadConfigure(sel.agent);
};

for (const b of document.querySelectorAll('#agent-tabs button')) {
  b.onclick = () => { if (sel.kind === 'agent') selectAgent(sel.agent, b.dataset.tab); };
}

$('#cfg-include').addEventListener('input', () => { renderPackageConfigTree(); reconcileContextChainWithVisibility(); renderContextChain(); });
$('#cfg-exclude').addEventListener('input', () => { renderPackageConfigTree(); reconcileContextChainWithVisibility(); renderContextChain(); });
$('#cfg-context-add').onclick = () => addContextStage($('#cfg-context-add-stage').value);
$('#cfg-kit-add-toggle').onclick = () => {
  renderKitAddList();
  const modal = $('#cfg-kit-add-modal');
  if (modal?.showModal) modal.showModal();
};
$('#cfg-kit-add-close').onclick = () => $('#cfg-kit-add-modal')?.close();
$('#cfg-kit-add-modal').addEventListener('click', (e) => {
  if (e.target === $('#cfg-kit-add-modal')) $('#cfg-kit-add-modal').close();
});
$('#cfg-var-add').onclick = () => $('#cfg-vars').appendChild(varRow());
$('#cfg-throttle-add').onclick = () => $('#cfg-throttle').appendChild(throttleRow());

// ---------- rail (telemetry & signals) ----------
const teleFeed = $('#tele-feed');
const FILTERS = {
  all: () => true,
  work: (t) => t.startsWith('in/'),
  tools: (t) => /^obs\/[^/]+\/[^/]+\/[^/]+\/tool\//.test(t),
  signals: (t) => t.startsWith('signal/'),
};
const inScope = (t) =>
  sel.kind === 'agent'
    ? t.startsWith(`obs/agent/${sel.agent}/`) || t.startsWith(`in/agent/${sel.agent}`)
    : true;

function setFilter(f) {
  filter = f;
  document.querySelectorAll('.tele-filters button[data-f]').forEach((x) => x.classList.toggle('on', x.dataset.f === f));
}
function verbClass(topic) {
  if (topic.startsWith('signal/')) return 'v-signal';
  if (topic.startsWith('in/')) return 'v-in';
  if (FILTERS.tools(topic)) return 'v-tool';
  return 'v-obs';
}
function railRow({ topic, env }) {
  const row = el('div', `row ${verbClass(topic)}`);
  row.appendChild(el('span', 't', timeOf(env)));
  const body = el('span');
  const toolM = topic.match(/^obs\/[^/]+\/[^/]+\/([^/]+)\/tool\/([^/]+)\/(call|result)$/);
  if (toolM) {
    body.appendChild(el('span', 'badge', toolM[3] === 'call' ? '⚙ call' : '⚙ result'));
    body.appendChild(el('span', 'topic', `${toolM[2]} `));
  } else {
    body.appendChild(el('span', 'topic', `${topic} `));
  }
  body.appendChild(el('span', 'pay', summarize(env?.payload)));
  row.appendChild(body);
  return row;
}
function appendRail(msg) {
  if (paused || !FILTERS[filter](msg.topic) || !inScope(msg.topic)) return;
  const was = atBottom(teleFeed);
  teleFeed.appendChild(railRow(msg));
  while (teleFeed.children.length > 600) teleFeed.firstChild.remove();
  stick(teleFeed, was);
}
function renderRail() {
  teleFeed.textContent = '';
  for (const m of buffer) {
    if (FILTERS[filter](m.topic) && inScope(m.topic)) teleFeed.appendChild(railRow(m));
  }
  while (teleFeed.children.length > 600) teleFeed.firstChild.remove();
  teleFeed.scrollTop = teleFeed.scrollHeight;
}
for (const b of document.querySelectorAll('.tele-filters button[data-f]')) {
  b.onclick = () => { setFilter(b.dataset.f); renderRail(); };
}
$('#tele-pause').onclick = () => {
  paused = !paused;
  $('#tele-pause').textContent = paused ? '▶' : '⏸';
  if (!paused) renderRail();
};

// ---------- conversation (per agent, persistent feeds) ----------
function convFeedFor(agent) {
  let f = convFeeds.get(agent);
  if (!f) {
    f = el('div', 'conv-feed');
    const empty = el('div', 'conv-empty');
    empty.appendChild(el('p', 'conv-empty-mark', '⟁'));
    const p = el('p');
    p.innerHTML = 'nothing yet — say something below.<br/>asks and replies thread here by correlation.';
    empty.appendChild(p);
    f.appendChild(empty);
    convFeeds.set(agent, f);
  }
  return f;
}
function mountConvFeed(agent) {
  const holder = $('#conv-holder');
  holder.textContent = '';
  const f = convFeedFor(agent);
  holder.appendChild(f);
  f.scrollTop = f.scrollHeight;
}
function convMsg(agent, who, cls, text, corr) {
  const feed = convFeedFor(agent);
  feed.querySelector('.conv-empty')?.remove();
  const was = atBottom(feed);
  const m = el('div', `msg ${cls}`);
  // Threading still keys on correlation internally; the id is debug detail,
  // so it rides the element title (hover) instead of cluttering every bubble.
  if (corr) m.title = `correlation ${corr}`;
  const head = el('div', 'msg-meta');
  head.appendChild(el('span', 'msg-who', who));
  m.appendChild(head);
  const body = el('div', 'msg-body', text);
  m.appendChild(body);
  feed.appendChild(m);
  stick(feed, was);
  return { m, body };
}

// A labeled failure (payload.failed) from the harness: the message was
// delivered, the agent broke. Render it explicitly in the thread, deduped
// by correlation so a replayed run can't stack identical failures.
const seenFailures = new Set();
function convFailure(agent, env) {
  const corr = env.correlation_id;
  if (corr && seenFailures.has(corr)) return;
  if (corr) seenFailures.add(corr);
  const p = env?.payload ?? {};
  const { body } = convMsg(agent, 'agent failed', 'failed', '', corr);
  body.textContent = '';
  body.appendChild(el('div', 'fail-reason', p.error || 'the agent failed with no detail.'));
  body.appendChild(el('div', 'fail-hint', 'check the agent: a model set, the background service running, and the add-on turned on.'));
}

function routeHumanMail(env) {
  // in/human mail is owner-addressed, not agent-addressed; route by the
  // correlation we've seen on the agent side, else the selected agent.
  const corr = env.correlation_id;
  return corrAgent.get(corr) ?? (sel.kind === 'agent' ? sel.agent : null) ?? [...agents.keys()][0] ?? DEFAULT_AGENT;
}

function renderAsk(agent, env) {
  const corr = env.correlation_id;
  const p = env.payload ?? {};
  if (corr && seenAsks.has(corr)) return;
  const feed = convFeedFor(agent);
  feed.querySelector('.conv-empty')?.remove();
  const was = atBottom(feed);

  const m = el('div', 'msg agent ask');
  const head = el('div', 'msg-meta');
  head.appendChild(el('span', 'msg-who', 'agent asks'));
  if (corr) head.appendChild(el('span', 'msg-corr', corr.slice(0, 18)));
  m.appendChild(head);

  const body = el('div', 'msg-body');
  body.appendChild(el('div', 'ask-q', p.question ?? summarize(p)));

  const answer = (text) => {
    publish(`in/agent/${agent}`, { answer: text }, corr).then((ok) => {
      row.remove(); opts?.remove();
      const done = el('div', 'ask-done');
      done.append('answered: ');
      done.appendChild(el('b', '', text));
      if (!ok) done.append('  (send failed)');
      body.appendChild(done);
    });
  };

  let opts = null;
  if (Array.isArray(p.options) && p.options.length) {
    opts = el('div', 'ask-options');
    for (const o of p.options) {
      const b = el('button', '', String(o));
      b.onclick = () => answer(String(o));
      opts.appendChild(b);
    }
    body.appendChild(opts);
  }
  const row = el('div', 'ask-row');
  const input = el('input');
  input.placeholder = 'answer…';
  const send = el('button', '', 'answer');
  send.onclick = (e) => { e.preventDefault(); if (input.value.trim()) answer(input.value.trim()); };
  input.onkeydown = (e) => { if (e.key === 'Enter' && input.value.trim()) answer(input.value.trim()); };
  row.append(input, send);
  body.appendChild(row);

  m.appendChild(body);
  feed.appendChild(m);
  stick(feed, was);
  if (corr) seenAsks.set(corr, { body, row, opts });
}

function closeAskFromOutside(corr, ans) {
  const a = corr && seenAsks.get(corr);
  if (!a || a.body.querySelector('.ask-done')) return;
  a.row.remove(); a.opts?.remove();
  const done = el('div', 'ask-done');
  done.append('answered elsewhere: ');
  done.appendChild(el('b', '', String(ans ?? '✓')));
  a.body.appendChild(done);
}

// ---------- live message intake ----------
function onMessage(msg) {
  const { topic, env } = msg;
  count++;
  $('#stat-count').textContent = `${count} event${count === 1 ? '' : 's'}`;
  buffer.push(msg);
  if (buffer.length > BUFFER_CAP) buffer.shift();

  // discovery: agents announce themselves by existing on the bus
  const noun = agentOf(topic);
  if (noun) {
    const known = agents.has(noun);
    touchAgent(noun, { live: true });
    const m = topic.match(/^obs\/agent\/[^/]+\/([^/]+)\//);
    if (m) agents.get(noun).sessions.add(m[1]);
    if (!known || m) renderNav();
  }

  if (!$('#view-rail').hidden) appendRail(msg);

  const p = env?.payload && typeof env.payload === 'object' ? env.payload : {};
  if (topic.startsWith('signal/')) {
    const lamp = $('#signal-lamp');
    lamp.classList.add('lit');
    $('#signal-label').textContent = topic;
    return;
  }
  if (topic.startsWith('in/human/')) {
    const agent = routeHumanMail(env);
    if (p.failed) convFailure(agent, env);
    else if (p.question != null) renderAsk(agent, env);
    else if (typeof p.text === 'string') convMsg(agent, 'agent', 'agent', p.text, env.correlation_id);
    return;
  }
  if (noun && topic.startsWith('in/agent/')) {
    if (env.correlation_id) corrAgent.set(env.correlation_id, noun);
    if (typeof p.prompt === 'string') {
      // our own composes echo back via announce; render only foreign ones
      if (!sentCorrs.has(env.correlation_id)) convMsg(noun, 'you', 'you', p.prompt, env.correlation_id);
    } else if (p.answer != null) {
      closeAskFromOutside(env.correlation_id, p.answer);
    }
  }
}

// ---------- publish / compose ----------
async function publish(topic, payload, correlation) {
  try {
    const r = await fetch('/api/publish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ topic, payload, correlation }),
    });
    return (await r.json()).ok === true;
  } catch {
    return false;
  }
}

$('#compose').addEventListener('submit', async (e) => {
  e.preventDefault();
  if (sel.kind !== 'agent') return;
  const agent = sel.agent;
  const input = $('#compose-input');
  const text = input.value.trim();
  if (!text) return;
  const conv = `web-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  const session = agentSessions.get(agent) ?? `web-${agent}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  agentSessions.set(agent, session);
  sentCorrs.add(conv);
  corrAgent.set(conv, agent);
  convMsg(agent, 'you', 'you', text, conv);
  input.value = '';
  const btn = $('#compose-send');
  const ok = await publish(`in/agent/${agent}`, { prompt: text, session }, conv);
  btn.textContent = ok ? 'accepted ✓' : 'failed ✕';
  btn.classList.toggle('sent', ok);
  setTimeout(() => { btn.textContent = 'transmit'; btn.classList.remove('sent'); }, 1400);
});

// ---------- history view (graceful when absent) ----------
async function history(params) {
  try {
    const r = await fetch(`/api/history?${new URLSearchParams(params)}`);
    const j = await r.json().catch(() => null);
    if (r.status === 503 || r.status === 504) { setHistoryOk(false); return null; }
    if (!r.ok || !j?.ok) return j ?? null;
    setHistoryOk(true);
    return j;
  } catch {
    return null;
  }
}
function setHistoryOk(v) {
  if (historyOk === v) return;
  historyOk = v;
  $('#history-hint').hidden = v !== false;
}

async function refreshAgents() {
  const j = await history({ kind: 'agents' });
  if (!j?.ok) return;
  for (const a of j.agents ?? []) touchAgent(a.agent, { sessions: a.sessions });
  renderNav();
}

// ---------- sessions & transcripts ----------
const sessionsPane = $('#sessions-pane');

function liveOnlyNote(extra) {
  const d = el('div', 'dim-note');
  d.appendChild(el('div', '', 'transcripts unavailable — live view only.'));
  if (extra) d.appendChild(el('div', 'dim-sub', extra));
  return d;
}

async function loadSessions(agent) {
  sessionsPane.textContent = '';
  sessionsPane.appendChild(el('div', 'dim-note', 'asking the history view…'));
  const j = await history({ kind: 'sessions', agent });
  if (sel.kind !== 'agent' || sel.agent !== agent || sel.tab !== 'sessions') return;
  sessionsPane.textContent = '';
  if (!j?.ok) {
    sessionsPane.appendChild(liveOnlyNote('turn on the history view under add-ons to browse transcripts.'));
    return;
  }
  const list = j.sessions ?? [];
  if (!list.length) {
    sessionsPane.appendChild(el('div', 'dim-note', 'no recorded sessions for this agent yet.'));
    return;
  }
  const tbl = el('div', 'sess-list');
  const head = el('div', 'sess-row sess-head');
  for (const h of ['session', 'first', 'last', 'msgs', 'events']) head.appendChild(el('span', '', h));
  tbl.appendChild(head);
  for (const s of list) {
    const row = el('button', 'sess-row');
    row.appendChild(el('span', 'sess-id', s.session));
    row.appendChild(el('span', '', shortTs(s.first_ts)));
    row.appendChild(el('span', '', shortTs(s.last_ts)));
    row.appendChild(el('span', '', String(s.message_count)));
    row.appendChild(el('span', '', String(s.event_count)));
    row.onclick = () => openTranscript(agent, s.session);
    tbl.appendChild(row);
    touchAgent(agent, { sessions: [s.session] });
  }
  sessionsPane.appendChild(tbl);
  renderNav();
}

async function openTranscript(agent, session, beforeId, prependInto) {
  if (sel.kind !== 'agent' || sel.agent !== agent || sel.tab !== 'sessions') selectAgent(agent, 'sessions');
  if (!prependInto) {
    sessionsPane.textContent = '';
    sessionsPane.appendChild(el('div', 'dim-note', `reading transcript ${session}…`));
  }
  const params = { kind: 'transcript', session };
  if (beforeId != null) params.before_id = beforeId;
  const j = await history(params);
  if (sel.kind !== 'agent' || sel.agent !== agent || sel.tab !== 'sessions') return;
  if (!j?.ok) {
    sessionsPane.textContent = '';
    sessionsPane.appendChild(liveOnlyNote(j?.error));
    return;
  }
  if (prependInto) {
    const frag = document.createDocumentFragment();
    for (const m of j.messages ?? []) frag.appendChild(transcriptMsg(m));
    prependInto.replaceWith(...(j.has_more ? [earlierBtn(agent, session, j.messages?.[0]?.id)] : []), frag);
    return;
  }
  sessionsPane.textContent = '';
  const bar = el('div', 'tr-bar');
  const back = el('button', 'tr-back', '← sessions');
  back.onclick = () => loadSessions(agent);
  bar.appendChild(back);
  bar.appendChild(el('span', 'tr-title', session));
  sessionsPane.appendChild(bar);
  const feed = el('div', 'tr-feed');
  if (j.has_more) feed.appendChild(earlierBtn(agent, session, j.messages?.[0]?.id));
  if (!(j.messages ?? []).length) feed.appendChild(el('div', 'dim-note', 'empty transcript.'));
  for (const m of j.messages ?? []) feed.appendChild(transcriptMsg(m));
  sessionsPane.appendChild(feed);
  feed.scrollTop = feed.scrollHeight;
}

function earlierBtn(agent, session, beforeId) {
  const b = el('button', 'tr-earlier', '… load earlier');
  b.onclick = () => openTranscript(agent, session, beforeId, b);
  return b;
}

function detailsBlock(label, cls, content) {
  const d = el('details', `tr-tool ${cls ?? ''}`);
  d.appendChild(el('summary', '', label));
  const pre = el('pre', 'tr-pre');
  pre.textContent = typeof content === 'string' ? content : JSON.stringify(content, null, 2);
  d.appendChild(pre);
  return d;
}

function transcriptMsg(m) {
  const c = m.content;
  const wrap = el('div', `tr-msg tr-${m.role}`);
  const head = el('div', 'msg-meta');
  head.appendChild(el('span', 'msg-who', m.role));
  head.appendChild(el('span', '', shortTs(m.created_at)));
  if (m.event_id != null) head.appendChild(el('span', 'msg-corr', `ev ${m.event_id}`));
  wrap.appendChild(head);

  if (c && typeof c === 'object' && c.truncated === true && c.preview != null) {
    const body = el('div', 'msg-body', c.preview);
    body.appendChild(el('div', 'dim-sub', `(truncated — ${c.chars} chars)`));
    wrap.appendChild(body);
    return wrap;
  }
  if (m.role === 'tool') {
    const name = c?.name ?? 'tool';
    wrap.appendChild(detailsBlock(`⚙ ${name} → result`, 'tr-tool-result', c?.content ?? c));
    return wrap;
  }
  const body = el('div', 'msg-body');
  const text = typeof c === 'string' ? c : c?.text;
  if (text) body.appendChild(el('div', 'tr-text', text));
  if (Array.isArray(c?.tool_calls)) {
    for (const tc of c.tool_calls) body.appendChild(detailsBlock(`⚙ ${tc.fn_name ?? 'call'}`, 'tr-tool-call', tc.fn_arguments ?? tc));
  }
  if (!body.children.length) body.appendChild(detailsBlock('raw message', '', c));
  wrap.appendChild(body);
  return wrap;
}

// ---------- chrome ----------
$('#signal-lamp').onclick = () => {
  $('#signal-lamp').classList.remove('lit');
  $('#signal-label').textContent = 'signal';
};

// ---------- the wire ----------
const es = new EventSource('/api/stream');
es.onmessage = (e) => {
  let m;
  try { m = JSON.parse(e.data); } catch { return; }
  if (m.kind === 'status') {
    if (m.agent) {
      DEFAULT_AGENT = m.agent;
      if (!agents.has(m.agent)) { touchAgent(m.agent); renderNav(); }
    }
    const c = $('#conn');
    c.className = `conn ${m.connected ? 'conn-up' : 'conn-down'}`;
    $('#conn-text').textContent = m.connected ? 'connected' : 'disconnected';
    $('#stat-broker').textContent = '';
  } else if (m.kind === 'message') {
    onMessage(m);
  }
};
es.onerror = () => {
  const c = $('#conn');
  c.className = 'conn conn-down';
  $('#conn-text').textContent = 'server lost — retrying';
};

// Model suggestions from the provider's /v1/models when it has one; the
// static datalist entries remain as the fallback.
(async () => {
  try {
    const r = await fetch('/api/admin/models');
    const j = await r.json();
    if (!(j.models ?? []).length) {
      // Say WHY the picker is running on canned suggestions — a missing
      // .env or a provider without /models should be visible, not silent.
      if (j.note) {
        const hint = $('#models-hint');
        if (hint) { hint.textContent = `model list unavailable: ${j.note}`; hint.hidden = false; }
        console.warn('models:', j.note);
      }
      return;
    }
    const dl = $('#model-suggestions');
    dl.textContent = '';
    for (const m of j.models) {
      const o = document.createElement('option');
      o.value = m.id;
      if (m.display_name) o.label = m.display_name;
      dl.appendChild(o);
    }
  } catch { /* static suggestions stand */ }
})();

// boot: the welcome front door (orients + routes to the primary agent) +
// disk agents (profiles ARE agents — a silent root still shows its
// identities) + history probe (re-probed so a later approve heals us).
selectWelcome();
loadDiskAgents();
refreshAgents();
setInterval(refreshAgents, 15000);

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
    ? 'transcripts are off until you approve the history package (kits & review).'
    : '';
}

function selectSignals() {
  sel = { kind: 'signals' };
  $('#stage-title').textContent = 'signals';
  $('#stage-note').textContent = 'the global rail — orange is algedonic, nothing else is';
  $('#agent-tabs').hidden = true;
  setFilter('signals');
  show('rail');
  renderRail();
  renderNav();
}

function selectSetup() {
  sel = { kind: 'setup' };
  $('#stage-title').textContent = 'kits & review';
  $('#stage-note').textContent = 'kits & grants — stage, then approve (here or in your terminal)';
  $('#agent-tabs').hidden = true;
  show('setup');
  renderNav();
  loadSetup();
}

// ---------- setup (staging only — the CLI is the commit gesture) ----------
async function adminGet(p) {
  try { const r = await fetch(`/api/admin/${p}`); return await r.json(); } catch { return { ok: false }; }
}

async function loadSetup() {
  const kitsBox = $('#setup-kits');
  const pendBox = $('#setup-pending');
  kitsBox.textContent = 'resolving…';
  pendBox.textContent = 'reading the ledger…';

  const [kits, pkgs] = await Promise.all([adminGet('kits'), adminGet('packages')]);
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
    const readmeBtn = el('button', 'ghost', 'readme');
    const stageBtn = el('button', '', provenance.has(k.name) ? 'stage again' : 'stage');
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
      stageBtn.disabled = true; stageBtn.textContent = 'staging…';
      const r = await fetch('/api/admin/kits/add', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ kit: k.name }),
      }).then((x) => x.json()).catch(() => ({ ok: false }));
      stageBtn.textContent = r.ok ? 'staged' : 'failed';
      if (r.ok) loadSetup();
    };
    kitsBox.appendChild(row);
  }
  if (!(kits.kits ?? []).length) {
    kitsBox.appendChild(el('div', 'dim-note', kits.ok === false
      ? `kit list failed: ${kits.error ?? 'unknown - is the elanus binary on the server PATH current?'}`
      : 'no kits resolvable — drop one in <root>/kits'));
  }

  pendBox.textContent = '';
  let pendingAny = false;
  for (const p of pkgs.packages ?? []) {
    const reqs = (p.grants ?? []).filter((g) => g.state === 'requested');
    if (!reqs.length) continue;
    pendingAny = true;
    const card = el('div', 'setup-pending-pkg');
    card.appendChild(el('div', 'setup-kit-name', p.name));
    for (const g of reqs) card.appendChild(el('div', 'setup-grant', `${g.kind}  ${g.value}`));
    const row = el('div', 'setup-row');
    const approveBtn = el('button', '', `approve ${p.name}`);
    approveBtn.onclick = async () => {
      approveBtn.disabled = true; approveBtn.textContent = 'approving…';
      const r = await fetch('/api/admin/approve', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ package: p.name }),
      }).then((x) => x.json()).catch(() => ({ ok: false }));
      if (r.ok) loadSetup(); else { approveBtn.textContent = 'failed'; }
    };
    row.appendChild(approveBtn);
    const cmd = el('code', 'setup-cmd', `elanus approve ${p.name}`);
    cmd.title = 'the same gesture from a terminal — click to copy';
    cmd.onclick = () => navigator.clipboard?.writeText(`elanus approve ${p.name}`);
    row.appendChild(cmd);
    card.appendChild(row);
    pendBox.appendChild(card);
  }
  if (!pendingAny) pendBox.appendChild(el('div', 'dim-note', 'nothing pending — the ledger is at rest'));
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
    $('#stage-note').textContent = 'transcripts from the ledger, via the history view';
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
async function loadConfigure(agentName) {
  const p = profileOf(agentName);
  cfgProfile = p?.profile ?? agentName;
  $('#cfg-note').textContent = '';
  $('#cfg-file').textContent = `profiles/${cfgProfile}/profile.toml`;
  const r = await fetch(`/api/admin/profile?name=${encodeURIComponent(cfgProfile)}`)
    .then((x) => x.json()).catch(() => ({ ok: false }));
  if (!r.ok) {
    $('#cfg-note').textContent = `no profile file for ${cfgProfile} — this agent only exists as traffic; create a profile to configure it`;
  }
  $('#cfg-toml').value = r.ok ? r.toml : '';
  // The parsed view comes from profile list (same loader the kernel uses).
  await loadDiskAgents();
  const d = profileOf(agentName) ?? {};
  $('#cfg-agent').value = d.agent ?? agentName;
  $('#cfg-model').value = d.model ?? '';
  $('#cfg-turns').value = d.max_turns ?? '';
  $('#cfg-workdir').value = d.workdir ?? '';
  $('#cfg-include').value = (d.skills?.include ?? []).join(', ');
  $('#cfg-exclude').value = (d.skills?.exclude ?? []).join(', ');
}

$('#cfg-save').onclick = async () => {
  if (!cfgProfile) return;
  const note = $('#cfg-note');
  note.textContent = 'saving…';
  const set = {};
  const newAgent = $('#cfg-agent').value.trim();
  if (newAgent) set['agent'] = newAgent;
  if ($('#cfg-model').value.trim()) set['model.model'] = $('#cfg-model').value.trim();
  if ($('#cfg-turns').value) set['model.max_turns'] = Number($('#cfg-turns').value);
  set['sandbox.workdir'] = $('#cfg-workdir').value.trim();
  const arr = (v) => v.split(',').map((x) => x.trim()).filter(Boolean);
  // Send as a real JS array — server's tomlValue sees Array.isArray and
  // encodes it as a TOML array. Sending a JSON-stringified string here was
  // the regression: it arrived as the string '["#"]' and the kernel refused.
  set['skills.include'] = arr($('#cfg-include').value).length ? arr($('#cfg-include').value) : ['#'];
  // Always sent: an EMPTY exclude list is a meaningful save (clearing it),
  // and an omitted key would silently keep the old value.
  set['skills.exclude'] = arr($('#cfg-exclude').value);
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
    if (v === '' && k !== 'sandbox.workdir') continue;
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
  body.appendChild(el('div', 'fail-hint', 'check the agent: a model set (configure), the daemon running, grants approved.'));
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
  sentCorrs.add(conv);
  corrAgent.set(conv, agent);
  convMsg(agent, 'you', 'you', text, conv);
  input.value = '';
  const btn = $('#compose-send');
  const ok = await publish(`in/agent/${agent}`, { prompt: text }, conv);
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
  d.appendChild(el('div', '', 'history package not running — live view only.'));
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
    sessionsPane.appendChild(liveOnlyNote('approve the history package under “kits & review” to browse transcripts.'));
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
    body.appendChild(el('div', 'dim-sub', `(truncated — ${c.chars} chars in the ledger)`));
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
    $('#conn-text').textContent = m.connected ? 'bus connected' : 'bus down';
    $('#stat-broker').textContent = m.broker ?? '';
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

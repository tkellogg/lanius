// elanus web — everything here arrived over plain MQTT (relayed via SSE).
'use strict';

const $ = (s) => document.querySelector(s);
const convFeed = $('#conv-feed');
const teleFeed = $('#tele-feed');
const convEmpty = $('#conv-empty');

let AGENT = 'main';
let count = 0;
let paused = false;
let filter = 'all';
const seenAsks = new Map(); // corr -> ask element refs

// ---------- helpers ----------
const timeOf = (env) => {
  const d = new Date(env?.ts ?? Date.now());
  return isNaN(d) ? '--:--:--' : d.toTimeString().slice(0, 8);
};
const summarize = (p, max = 110) => {
  if (p == null) return '';
  const s = typeof p === 'string' ? p : JSON.stringify(p);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
};
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};
const atBottom = (box) => box.scrollHeight - box.scrollTop - box.clientHeight < 60;
const stick = (box, was) => { if (was) box.scrollTop = box.scrollHeight; };

// ---------- telemetry ----------
const FILTERS = {
  all: () => true,
  work: (t) => t.startsWith('in/'),
  tools: (t) => /^obs\/[^/]+\/[^/]+\/[^/]+\/tool\//.test(t),
  signals: (t) => t.startsWith('signal/'),
};
function verbClass(topic) {
  if (topic.startsWith('signal/')) return 'v-signal';
  if (topic.startsWith('in/')) return 'v-in';
  if (FILTERS.tools(topic)) return 'v-tool';
  return 'v-obs';
}
function teleRow({ topic, env }) {
  if (paused || !FILTERS[filter](topic)) return;
  const was = atBottom(teleFeed);
  const row = el('div', `row ${verbClass(topic)}`);
  row.appendChild(el('span', 't', timeOf(env)));
  const body = el('span');
  const toolM = topic.match(/^obs\/[^/]+\/[^/]+\/([^/]+)\/tool\/([^/]+)\/(call|result)$/);
  if (toolM) {
    const b = el('span', 'badge', toolM[3] === 'call' ? '⚙ call' : '⚙ result');
    body.appendChild(b);
    body.appendChild(el('span', 'topic', `${toolM[2]} `));
  } else {
    body.appendChild(el('span', 'topic', `${topic} `));
  }
  body.appendChild(el('span', 'pay', summarize(env?.payload)));
  row.appendChild(body);
  teleFeed.appendChild(row);
  while (teleFeed.children.length > 600) teleFeed.firstChild.remove();
  stick(teleFeed, was);
}

// ---------- conversation ----------
function convMsg(who, cls, text, corr, meta) {
  convEmpty?.remove();
  const was = atBottom(convFeed);
  const m = el('div', `msg ${cls}`);
  const head = el('div', 'msg-meta');
  head.appendChild(el('span', 'msg-who', who));
  if (corr) head.appendChild(el('span', 'msg-corr', corr.slice(0, 18)));
  if (meta) head.appendChild(el('span', '', meta));
  m.appendChild(head);
  const body = el('div', 'msg-body', text);
  m.appendChild(body);
  convFeed.appendChild(m);
  stick(convFeed, was);
  return { m, body };
}

function renderAsk(env) {
  const corr = env.correlation_id;
  const p = env.payload ?? {};
  if (corr && seenAsks.has(corr)) return;
  convEmpty?.remove();
  const was = atBottom(convFeed);

  const m = el('div', 'msg agent ask');
  const head = el('div', 'msg-meta');
  head.appendChild(el('span', 'msg-who', 'agent asks'));
  if (corr) head.appendChild(el('span', 'msg-corr', corr.slice(0, 18)));
  m.appendChild(head);

  const body = el('div', 'msg-body');
  body.appendChild(el('div', 'ask-q', p.question ?? summarize(p)));

  const answer = (text) => {
    publish(`in/agent/${AGENT}`, { answer: text }, corr).then((ok) => {
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
  convFeed.appendChild(m);
  stick(convFeed, was);
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

function onMessage({ topic, env }) {
  count++;
  $('#stat-count').textContent = `${count} events`;
  teleRow({ topic, env });

  const p = env?.payload && typeof env.payload === 'object' ? env.payload : {};
  if (topic.startsWith('signal/')) {
    const lamp = $('#signal-lamp');
    lamp.classList.add('lit');
    $('#signal-label').textContent = topic.replace(/^signal\//, 'signal/');
    return;
  }
  if (topic.startsWith('in/human/')) {
    if (p.question != null) renderAsk(env);
    else if (typeof p.text === 'string') convMsg('agent', 'agent', p.text, env.correlation_id);
    return;
  }
  if (topic.startsWith('in/agent/')) {
    if (typeof p.prompt === 'string') {
      // our own composes echo back via announce; render only foreign ones
      if (!sentCorrs.has(env.correlation_id)) convMsg('you', 'you', p.prompt, env.correlation_id);
    } else if (p.answer != null) {
      closeAskFromOutside(env.correlation_id, p.answer);
    }
  }
}

// ---------- publish ----------
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

const sentCorrs = new Set();
$('#compose').addEventListener('submit', async (e) => {
  e.preventDefault();
  const input = $('#compose-input');
  const text = input.value.trim();
  if (!text) return;
  const conv = `web-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  sentCorrs.add(conv);
  convMsg('you', 'you', text, conv);
  input.value = '';
  const btn = $('#compose-send');
  const ok = await publish(`in/agent/${AGENT}`, { prompt: text }, conv);
  btn.textContent = ok ? 'accepted ✓' : 'failed ✕';
  btn.classList.toggle('sent', ok);
  setTimeout(() => { btn.textContent = 'transmit'; btn.classList.remove('sent'); }, 1400);
});

// ---------- chrome ----------
$('#signal-lamp').onclick = () => {
  $('#signal-lamp').classList.remove('lit');
  $('#signal-label').textContent = 'signal';
};
for (const b of document.querySelectorAll('.tele-filters button[data-f]')) {
  b.onclick = () => {
    filter = b.dataset.f;
    document.querySelectorAll('.tele-filters button[data-f]').forEach((x) => x.classList.toggle('on', x === b));
  };
}
$('#tele-pause').onclick = () => {
  paused = !paused;
  $('#tele-pause').textContent = paused ? '▶' : '⏸';
};

// ---------- the wire ----------
const es = new EventSource('/api/stream');
es.onmessage = (e) => {
  let m;
  try { m = JSON.parse(e.data); } catch { return; }
  if (m.kind === 'status') {
    if (m.agent) AGENT = m.agent;
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

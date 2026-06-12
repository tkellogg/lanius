// elanus web dashboard server.
//
// The PURE-MQTT-CLIENT constraint from ui/tui carries over, one hop removed:
// browsers cannot speak raw TCP MQTT, so this process is the ordinary
// anonymous loopback MQTT 5 client, and the browser talks to *it* — bus
// messages relayed over SSE, publishes accepted over POST. No sqlite, no
// trace.jsonl, no privileged access; the only filesystem touches are this
// directory's static files and <root>/bus.toml for broker discovery.
// History reads proxy to the userland `history` package's HTTP endpoint
// (HANDOFF phase 3): the daemon assigns it a loopback port, recorded in
// <root>/run/pkg-history/http.json — discovery from harness state, never
// retained bus messages (docs/security.md entry 11).
//
// AUTHORITY: read, converse, and STAGE. Admin endpoints compose pending
// state (kit add --pending, profile file edits) but never commit grants —
// approve/revoke stays in the CLI until the identity model lands
// (HANDOFF phase 5; docs/security.md entries 4-5: staging is workflow,
// not a boundary, and nothing here claims one).
//
//   node server.mjs --root /tmp/elanus-live [--port 7180] [--agent main]
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import mqtt from 'mqtt';
import { brokerUrl, parseArgs } from './config.mjs';

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  console.log('usage: node server.mjs [--root <harness root>] [--url mqtt://...] [--port 7180] [--agent main]');
  process.exit(0);
}
const BROKER = brokerUrl(args);
const PORT = args.port ?? Number(process.env.ELANUS_WEB_PORT ?? 7180);
const AGENT = args.agent ?? 'main';
const PUB = path.join(path.dirname(fileURLToPath(import.meta.url)), 'public');

// ---- bus side -------------------------------------------------------------
const RING_CAP = 1000; // late-joining browsers get recent history
const ring = [];
let seq = 0;
const sseClients = new Set();

const client = mqtt.connect(BROKER, {
  protocolVersion: 5,
  clean: true,
  clientId: `el-web-${process.pid}`,
  reconnectPeriod: 2000,
});
let connected = false;
client.on('connect', () => {
  connected = true;
  client.subscribe({ 'obs/#': { qos: 0 }, 'in/#': { qos: 1 }, 'signal/#': { qos: 1 } });
  broadcast({ kind: 'status', connected: true, broker: BROKER });
});
client.on('close', () => {
  connected = false;
  broadcast({ kind: 'status', connected: false, broker: BROKER });
});
client.on('message', (topic, payload) => {
  let env;
  try {
    env = JSON.parse(payload.toString('utf8'));
  } catch {
    env = { payload: payload.toString('utf8') };
  }
  const msg = { kind: 'message', seq: ++seq, topic, env };
  ring.push(msg);
  if (ring.length > RING_CAP) ring.shift();
  broadcast(msg);
});

// ---- history view proxy -----------------------------------------------------
// /api/history → POST <history endpoint>/query. The endpoint is re-read per
// request from run/pkg-history/http.json (cheap, and it heals across actor
// restarts). GET maps query params onto the flat kinds; POST passes the
// query DSL body through verbatim (kind "search": filter x select x page).
const HIST_KINDS = new Set(['agents', 'sessions', 'transcript', 'conversation', 'search']);
const ROOT = args.root ?? process.env.HARNESS_ROOT ?? null;

function historyEndpoint() {
  if (!ROOT) return null;
  try {
    const j = JSON.parse(fs.readFileSync(path.join(ROOT, 'run', 'pkg-history', 'http.json'), 'utf8'));
    return j.port ? `http://127.0.0.1:${j.port}` : null;
  } catch {
    return null;
  }
}

async function handleHistory(query, res) {
  if (!HIST_KINDS.has(query?.kind)) {
    res.writeHead(400, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: `kind must be one of ${[...HIST_KINDS].join('|')}` }));
    return;
  }
  const base = historyEndpoint();
  if (!base) {
    res.writeHead(503, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: 'history view unavailable — is the history package running and approved? (no run/pkg-history/http.json)' }));
    return;
  }
  try {
    const r = await fetch(`${base}/query`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(query),
      signal: AbortSignal.timeout(5000),
    });
    const body = await r.text();
    res.writeHead(r.status, { 'content-type': 'application/json' }).end(body);
  } catch (err) {
    res.writeHead(503, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: `history view unreachable: ${String(err.message ?? err)} — approve the history package if it is parked` }));
  }
}

// ---- admin: staging only ----------------------------------------------------
// Privileged gestures shell out to the elanus CLI — one code path for every
// human gesture, and this server adds no authority of its own. Kit installs
// are ALWAYS staged (--pending); profile files are edited directly (the
// same trust as the human's text editor — profiles are not ledger state).
import { execFile } from 'node:child_process';
const ELANUS_BIN = process.env.ELANUS_BIN || 'elanus';

function cli(cliArgs) {
  return new Promise((resolve) => {
    execFile(ELANUS_BIN, cliArgs, { env: { ...process.env, ...(ROOT ? { HARNESS_ROOT: ROOT } : {}) }, timeout: 30000 },
      (err, stdout, stderr) => resolve({ ok: !err, stdout: String(stdout), stderr: String(stderr), error: err ? String(err.message ?? err) : undefined }));
  });
}

function jsonLines(text) {
  return text.split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
}

function sendJson(res, code, body) {
  res.writeHead(code, { 'content-type': 'application/json' }).end(JSON.stringify(body));
}

const PROFILE_NAME_RE = /^[A-Za-z0-9_-]{1,64}$/;

async function handleAdmin(url, req, res, body) {
  if (url.pathname === '/api/admin/agents' && req.method === 'GET') {
    const r = await cli(['profile', 'list']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, profiles: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/agents' && req.method === 'POST') {
    const { name, agent, model } = body ?? {};
    if (typeof name !== 'string' || !PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: 'bad profile name' });
    const args = ['profile', 'new', name];
    if (agent) args.push('--agent', String(agent));
    if (model) args.push('--model', String(model));
    const r = await cli(args);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/agents/set' && req.method === 'POST') {
    // {name, set: {"agent": "kestrel", "model.max_turns": 12, ...}} →
    // elanus profile set <name> k=v... — the kernel owns the TOML edit
    // (comments survive, the result is validated before it lands).
    const { name, set } = body ?? {};
    if (typeof name !== 'string' || !PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: 'bad profile name' });
    if (!set || typeof set !== 'object' || !Object.keys(set).length) return sendJson(res, 400, { ok: false, error: 'need {set}' });
    const pairs = Object.entries(set).map(([k, v]) => `${k}=${typeof v === 'string' ? JSON.stringify(v) : v}`);
    const r = await cli(['profile', 'set', name, ...pairs]);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/kits/readme' && req.method === 'GET') {
    const kit = url.searchParams.get('kit');
    if (!kit) return sendJson(res, 400, { ok: false, error: 'need ?kit=' });
    const r = await cli(['kit', 'show', kit]);
    return sendJson(res, r.ok ? 200 : 404, r.ok ? { ok: true, readme: r.stdout } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/kits' && req.method === 'GET') {
    const r = await cli(['kit', 'list', '--json']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, kits: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/kits/add' && req.method === 'POST') {
    const { kit, copy } = body ?? {};
    if (typeof kit !== 'string' || !kit.length) return sendJson(res, 400, { ok: false, error: 'need {kit}' });
    // Staged, always: the UI composes; the CLI commits.
    const args = ['kit', 'add', kit, '--pending'];
    if (copy) args.push('--copy');
    const r = await cli(args);
    return sendJson(res, r.ok ? 200 : 500, { ok: r.ok, staged: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/packages' && req.method === 'GET') {
    const r = await cli(['packages', '--json']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, packages: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/profile' && (req.method === 'GET' || req.method === 'PUT')) {
    if (!ROOT) return sendJson(res, 503, { ok: false, error: 'no --root; profile editing needs the harness root' });
    const name = url.searchParams.get('name') ?? 'default';
    if (!PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: 'bad profile name' });
    const file = path.join(ROOT, 'profiles', name, 'profile.toml');
    if (req.method === 'GET') {
      try {
        return sendJson(res, 200, { ok: true, name, toml: fs.readFileSync(file, 'utf8') });
      } catch {
        return sendJson(res, 404, { ok: false, error: `no profile.toml for ${name}` });
      }
    }
    if (typeof body?.toml !== 'string') return sendJson(res, 400, { ok: false, error: 'need {toml}' });
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body.toml);
    return sendJson(res, 200, { ok: true, name });
  }
  sendJson(res, 404, { ok: false, error: 'unknown admin endpoint' });
}

function broadcast(obj) {
  const line = `data: ${JSON.stringify(obj)}\n\n`;
  for (const res of sseClients) res.write(line);
}

// ---- http side ------------------------------------------------------------
const MIME = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.mjs': 'text/javascript', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/api/stream') {
    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
      connection: 'keep-alive',
    });
    // catch-up: status first, then the ring
    res.write(`data: ${JSON.stringify({ kind: 'status', connected, broker: BROKER, agent: AGENT })}\n\n`);
    for (const m of ring) res.write(`data: ${JSON.stringify(m)}\n\n`);
    sseClients.add(res);
    req.on('close', () => sseClients.delete(res));
    return;
  }
  if (url.pathname.startsWith('/api/admin/')) {
    if (req.method === 'GET') {
      handleAdmin(url, req, res, null);
    } else {
      let body = '';
      req.on('data', (c) => (body += c));
      req.on('end', () => {
        let j = null;
        if (body.length) {
          try { j = JSON.parse(body); } catch { res.writeHead(400).end('bad json'); return; }
        }
        handleAdmin(url, req, res, j);
      });
    }
    return;
  }
  if (url.pathname === '/api/history' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      try {
        handleHistory(JSON.parse(body), res);
      } catch {
        res.writeHead(400).end('bad json');
      }
    });
    return;
  }
  if (url.pathname === '/api/history') {
    const q = { kind: url.searchParams.get('kind') };
    for (const k of ['agent', 'session', 'correlation', 'limit', 'before_id']) {
      const v = url.searchParams.get(k);
      if (v != null) q[k] = v;
    }
    handleHistory(q, res);
    return;
  }
  if (url.pathname === '/api/publish' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      let j;
      try {
        j = JSON.parse(body);
      } catch {
        res.writeHead(400).end('bad json');
        return;
      }
      const { topic, payload, correlation } = j;
      if (typeof topic !== 'string' || !topic.length || topic.includes('#') || topic.includes('+')) {
        res.writeHead(400).end('bad topic');
        return;
      }
      const props = correlation ? { userProperties: { 'el-correlation': String(correlation) } } : undefined;
      client.publish(topic, JSON.stringify(payload ?? {}), { qos: 1, properties: props }, (err) => {
        if (err) res.writeHead(502, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: String(err.message ?? err) }));
        else res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: true }));
      });
    });
    return;
  }
  // static
  let file = url.pathname === '/' ? '/index.html' : url.pathname;
  file = path.normalize(file).replace(/^(\.\.[/\\])+/, '');
  const full = path.join(PUB, file);
  if (!full.startsWith(PUB) || !fs.existsSync(full) || !fs.statSync(full).isFile()) {
    res.writeHead(404).end('not found');
    return;
  }
  res.writeHead(200, { 'content-type': MIME[path.extname(full)] ?? 'application/octet-stream' });
  fs.createReadStream(full).pipe(res);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`elanus web on http://127.0.0.1:${PORT}  (broker ${BROKER}, agent ${AGENT})`);
});

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
// AUTHORITY: the same as your terminal, because it shells out to it.
// Tim's call (2026-06-12): the earlier commits-stay-in-the-CLI rule
// claimed a boundary that doesn't exist — every local channel is equally
// unforgeable-less until the identity model lands (docs/security.md
// entries 3-5), so refusing an approve button here was theater. What IS
// different about a browser is hostile-origin traffic (CSRF, DNS
// rebinding), so every mutating route checks Origin/Host below, and
// UI-driven decisions carry decided_by=ui in the ledger for the trail.
//
//   node server.mjs [--root <elanus root>] [--port 7180] [--agent main]
// --root defaults to ~/.elanus/root — the same default the daemon uses — so
// no flag is needed when you're on the default root.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import mqtt from 'mqtt';
import os from 'node:os';
import { brokerUrl, parseArgs, resolveRoot } from './config.mjs';

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  console.log('usage: node server.mjs [--root <elanus root>] [--url mqtt://...] [--port 7180] [--agent main]\n  --root defaults to ~/.elanus/root (the daemon default); pass it only to target another root');
  process.exit(0);
}
const BROKER = brokerUrl(args);
const PORT = args.port ?? Number(process.env.ELANUS_WEB_PORT ?? 7180);
const AGENT = args.agent ?? 'main';
const HERE = path.dirname(fileURLToPath(import.meta.url));
const DIST = path.join(HERE, 'dist');
const PUB = DIST;
const ROOT = resolveRoot(args); // --root > $ELANUS_ROOT > ~/.elanus/root, as the daemon resolves it

// ---- observability --------------------------------------------------------
// One cheap, greppable place to watch the surface from the backend (Tim:
// "backend is cheaper to observe"). Lines go to stderr with a [web:<tag>] tag
// and an ISO timestamp; set ELANUS_WEB_LOG=<file> to ALSO append there, so a
// QA run can tail a file instead of scraping a terminal. Logging never throws.
const LOG_FILE = process.env.ELANUS_WEB_LOG || null;
function log(tag, msg) {
  const line = `${new Date().toISOString()} [web:${tag}] ${msg}`;
  console.error(line);
  if (LOG_FILE) { try { fs.appendFileSync(LOG_FILE, line + '\n'); } catch { /* observation must never break the server */ } }
}

// The web server is a trusted surface acting for the human (docs/identity.md):
// it presents the owner identity from the fenced secret store, so the broker
// stamps its events as the owner (the principal is an identity, default
// "owner", not the role "human"). Absent means we connect credential-less and
// are refused (deny-by-default).
// Matches src/secrets.rs valid_principal — keep in sync, or the surface could
// present a principal the broker would never resolve under that name (or a
// path-unsafe one). Falls back to "owner" exactly as the Rust resolution does.
function validPrincipal(name) {
  return !!name && name.length <= 64 && !name.startsWith('.') && !name.includes('/') && !name.includes('\\');
}
function ownerName() {
  const env = (process.env.ELANUS_OWNER || '').trim();
  if (validPrincipal(env)) return env;
  if (!ROOT) return 'owner';
  try {
    const n = fs.readFileSync(path.join(ROOT, '.secrets', '.owner-name'), 'utf8').trim();
    return validPrincipal(n) ? n : 'owner';
  } catch {
    return 'owner';
  }
}

function skillMetaFromMarkdown(raw) {
  const meta = {};
  const m = raw.match(/^---\n([\s\S]*?)\n---/);
  if (!m) return meta;
  for (const line of m[1].split(/\r?\n/)) {
    const kv = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/);
    if (!kv) continue;
    meta[kv[1]] = kv[2].replace(/^['"]|['"]$/g, '').trim();
  }
  return meta;
}

function leadingCommentSummary(raw) {
  const lines = [];
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) {
      if (lines.length) break;
      continue;
    }
    if (!line.trim().startsWith('#')) break;
    lines.push(line.replace(/^\s*#\s?/, '').trim());
  }
  return lines.join(' ').replace(/\s+/g, ' ').trim();
}

function arrayValues(raw, key) {
  const m = raw.match(new RegExp(`^\\s*${key}\\s*=\\s*\\[([^\\]]*)\\]`, 'm'));
  if (!m) return [];
  return [...m[1].matchAll(/"([^"]+)"/g)].map((v) => v[1]);
}

function manifestSummary(raw) {
  if (!raw) return null;
  const mode = raw.match(/^\s*mode\s*=\s*"([^"]+)"/m)?.[1] ?? null;
  const run = raw.match(/^\s*run\s*=\s*"([^"]+)"/m)?.[1] ?? null;
  const http = /^\s*http\s*=\s*true\s*$/m.test(raw);
  const hooks = [...raw.matchAll(/^\s*\[\[hook\]\]/gm)].length;
  const stages = [...raw.matchAll(/^\s*\[\[stage\]\]/gm)].length;
  const mcps = [...raw.matchAll(/^\s*\[\[mcp\]\]/gm)].length;
  const comment = leadingCommentSummary(raw);
  const labels = [];
  if (mode) labels.push(mode === 'daemon' ? 'actor daemon' : `${mode} actor`);
  if (http) labels.push('http service');
  if (hooks) labels.push(`${hooks} hook${hooks === 1 ? '' : 's'}`);
  if (stages) labels.push(`${stages} stage${stages === 1 ? '' : 's'}`);
  if (mcps) labels.push(`${mcps} mcp server${mcps === 1 ? '' : 's'}`);
  const actor = labels.length ? labels.join(', ') : null;
  const fallback = run ? `Runs ${run}${mode ? ` as ${mode}` : ''}.` : '';
  return {
    actor,
    mode,
    run,
    http,
    request: {
      subscribe: arrayValues(raw, 'subscribe'),
      publish: arrayValues(raw, 'publish'),
      blocking: arrayValues(raw, 'blocking'),
      fs_write: arrayValues(raw, 'fs_write'),
    },
    description: comment || fallback,
  };
}

async function listedKit(name) {
  const r = await cli(['kit', 'list', '--json']);
  if (!r.ok) return null;
  return jsonLines(r.stdout).find((k) => k.name === name) ?? null;
}

async function kitPackages(name) {
  const kit = await listedKit(name);
  if (!kit?.dir) return null;
  const packagesDir = path.join(kit.dir, 'packages');
  const out = [];
  let entries = [];
  try {
    entries = fs.readdirSync(packagesDir, { withFileTypes: true });
  } catch {
    return { ...kit, packages: out };
  }
  for (const ent of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (!ent.isDirectory()) continue;
    const dir = path.join(packagesDir, ent.name);
    const skillPath = path.join(dir, 'SKILL.md');
    let skill = null;
    try {
      const raw = fs.readFileSync(skillPath, 'utf8');
      const meta = skillMetaFromMarkdown(raw);
      skill = {
        name: meta.name || ent.name,
        description: meta.description || '',
      };
    } catch { /* package is not a skill */ }
    let manifest = null;
    try {
      manifest = manifestSummary(fs.readFileSync(path.join(dir, 'elanus.toml'), 'utf8'));
    } catch { /* package has no manifest */ }
    out.push({ name: ent.name, dir, skill, manifest });
  }
  return { ...kit, packages: out };
}
function humanCredential() {
  if (!ROOT) return null;
  try {
    const username = ownerName();
    const secret = fs.readFileSync(path.join(ROOT, '.secrets', username), 'utf8').trim();
    return secret ? { username, password: secret } : null;
  } catch {
    return null;
  }
}

// ---- bus side -------------------------------------------------------------
const RING_CAP = 1000; // late-joining browsers get recent history
const ring = [];
let seq = 0;
const sseClients = new Set();

const cred = humanCredential();
log('boot', `root=${ROOT} owner=${ownerName()} credential=${cred ? 'present' : 'MISSING — will be refused (deny-by-default); restart with the right --root/$ELANUS_ROOT'} broker=${BROKER} port=${PORT} agent=${AGENT}`);
const client = mqtt.connect(BROKER, {
  protocolVersion: 5,
  clean: true,
  clientId: `el-web-${process.pid}`,
  reconnectPeriod: 2000,
  ...(cred ?? {}),
});
let connected = false;
client.on('connect', (connack) => {
  connected = true;
  log('bus', `connected as ${cred?.username ?? 'ANONYMOUS'} (connack reason ${connack?.reasonCode ?? connack?.returnCode ?? 0}) — subscribing obs/# in/# signal/#`);
  client.subscribe({ 'obs/#': { qos: 0 }, 'in/#': { qos: 1 }, 'signal/#': { qos: 1 } });
  broadcast({ kind: 'status', connected: true, broker: BROKER });
});
client.on('close', () => {
  if (connected) log('bus', 'connection closed');
  connected = false;
  broadcast({ kind: 'status', connected: false, broker: BROKER });
});
// Transport failures and auth refusals both surface here; keep them distinct.
// ECONNREFUSED means nothing is listening on this port (the daemon is down or
// we're pointed at the wrong root) — that is NOT a credential problem, so don't
// blame credentials for it. Only an actual auth-shaped failure with no cred in
// hand warrants the deny-by-default hint.
client.on('error', (err) => {
  const code = err?.code ?? '';
  const hint = code === 'ECONNREFUSED'
    ? ` — nothing is listening at ${BROKER}; is the daemon running for this root (${ROOT})?`
    : (cred ? '' : ' — no credential found at this root (deny-by-default will refuse us)');
  log('bus', `error: ${code} ${err?.message ?? err}${hint}`);
});
client.on('reconnect', () => log('bus', `reconnecting to ${BROKER}…`));
client.on('offline', () => log('bus', 'offline'));
client.on('disconnect', (p) => log('bus', `broker disconnected us (reason ${p?.reasonCode ?? '?'})`));
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

// ---- admin: local human gestures -------------------------------------------
// Privileged gestures shell out to the elanus CLI — one code path for every
// human gesture, and this server adds no authority of its own. Human kit adds
// are a single accepted action; agent proposals remain reviewable requests.
import { execFile, execFileSync } from 'node:child_process';
// Which elanus answers admin calls matters (a stale install on PATH fails
// silently): prefer an explicit ELANUS_BIN, then the sibling dev build
// (this file lives in <repo>/ui/web), then PATH — and say so at startup.
const DEV_BIN = path.join(path.dirname(fileURLToPath(import.meta.url)), '../../target/debug/elanus');
const ELANUS_BIN = process.env.ELANUS_BIN || (fs.existsSync(DEV_BIN) ? DEV_BIN : 'elanus');
try {
  console.log(`elanus binary: ${ELANUS_BIN} (${execFileSync(ELANUS_BIN, ['--version'], { encoding: 'utf8' }).trim()})`);
} catch (e) {
  console.error(`elanus binary ${ELANUS_BIN} not runnable: ${e.message} — admin endpoints will fail`);
}

function cli(cliArgs) {
  log('cli', `elanus ${cliArgs.join(' ')}`);
  return new Promise((resolve) => {
    execFile(ELANUS_BIN, cliArgs, { env: { ...process.env, ...(ROOT ? { ELANUS_ROOT: ROOT } : {}) }, timeout: 30000 },
      (err, stdout, stderr) => {
        if (err) log('cli', `elanus ${cliArgs.join(' ')} FAILED: ${(String(stderr).trim().split('\n').pop() || err.message || '').slice(0, 200)}`);
        resolve({ ok: !err, stdout: String(stdout), stderr: String(stderr), error: err ? String(err.message ?? err) : undefined });
      });
  });
}

function jsonLines(text) {
  return text.split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
}

function sendJson(res, code, body) {
  res.writeHead(code, { 'content-type': 'application/json' }).end(JSON.stringify(body));
}

// The product calls them "agents"; the kernel calls them "profiles". Translate
// the CLI's raw error text at this boundary (docs/layering.md) so a person sees
// plain language, not an internal word or a bare "error:" prefix.
const BAD_NAME_MSG = 'names can use letters, numbers, dashes and underscores — no spaces';
function humanProfileError(raw) {
  const s = String(raw ?? '').replace(/^error:\s*/i, '').trim();
  if (!s) return 'that did not work';
  if (/bad profile name/i.test(s)) return BAD_NAME_MSG;
  return s.replace(/profiles/gi, 'agents').replace(/profile/gi, 'agent');
}

const PROFILE_NAME_RE = /^[A-Za-z0-9_-]{1,64}$/;
const PKG_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

function profileTomlPath(name) {
  const canonical = path.join(ROOT, 'config', 'agents', name, 'profile.toml');
  if (fs.existsSync(canonical)) return canonical;
  return path.join(ROOT, 'profiles', name, 'profile.toml');
}

// Browser-borne threats are the ones a terminal doesn't have: a hostile
// webpage POSTing to localhost (CSRF — browsers send these cross-origin
// even if they can't read the answer) and DNS rebinding (a hostile name
// resolving here, making its origin "same"). Mutations therefore require:
// a Host header that is genuinely local, and — when a browser supplies
// Origin — an Origin that matches it. curl/agents send no Origin and
// pass: they are local processes, which entry 3 already owns.
function originOk(req) {
  const host = req.headers.host ?? '';
  if (!/^(127\.0\.0\.1|localhost|\[::1\])(:\d+)?$/.test(host)) return false;
  const origin = req.headers.origin;
  if (origin == null) return true;
  try {
    return new URL(origin).host === host;
  } catch {
    return false;
  }
}

async function handleAdmin(url, req, res, body) {
  if (req.method !== 'GET' && !originOk(req)) {
    return sendJson(res, 403, { ok: false, error: 'cross-origin request refused (CSRF/DNS-rebinding guard)' });
  }
  if (url.pathname === '/api/admin/models' && req.method === 'GET') {
    const r = await cli(['models', '--json']);
    // A provider without /v1/models (some compat layers) is a graceful
    // empty list — the UI keeps its static suggestions.
    return sendJson(res, 200, r.ok ? { ok: true, models: jsonLines(r.stdout) } : { ok: true, models: [], note: (r.stderr || r.error || '').trim() });
  }
  if ((url.pathname === '/api/admin/approve' || url.pathname === '/api/admin/revoke') && req.method === 'POST') {
    const pkg = body?.package;
    if (typeof pkg !== 'string' || !PKG_NAME_RE.test(pkg)) return sendJson(res, 400, { ok: false, error: 'need {package}' });
    const verb = url.pathname.endsWith('approve') ? 'approve' : 'revoke';
    const r = await cli([verb, pkg, '--by', 'ui']);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/agents' && req.method === 'GET') {
    const r = await cli(['profile', 'list']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, profiles: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/agents' && req.method === 'POST') {
    const { name, agent, model } = body ?? {};
    if (typeof name !== 'string' || !PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: BAD_NAME_MSG });
    const args = ['profile', 'new', name];
    if (agent) args.push('--agent', String(agent));
    if (model) args.push('--model', String(model));
    const r = await cli(args);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : humanProfileError(r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/agents/set' && req.method === 'POST') {
    // {name, set: {"agent": "kestrel", "model.max_turns": 12, ...}} →
    // elanus profile set <name> k=v... — the kernel owns the TOML edit
    // (comments survive, the result is validated before it lands).
    const { name, set } = body ?? {};
    if (typeof name !== 'string' || !PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: BAD_NAME_MSG });
    if (!set || typeof set !== 'object' || !Object.keys(set).length) return sendJson(res, 400, { ok: false, error: 'need {set}' });
    // Encode each JSON value as TOML value text for `profile set`: arrays
    // become real TOML arrays (a JSON string array IS one), strings get
    // quoted, numbers/bools pass bare. Mis-encoding here is how
    // include = "[\"#\"]" once reached a profile (it was refused by the
    // kernel's validate-before-write, but the save still failed).
    const tomlValue = (v) => {
      if (Array.isArray(v)) return `[${v.map((x) => tomlValue(x)).join(', ')}]`;
      if (v && typeof v === 'object') {
        return `{ ${Object.entries(v).map(([k, val]) => `${k} = ${tomlValue(val)}`).join(', ')} }`;
      }
      return typeof v === 'string' ? JSON.stringify(v) : String(v);
    };
    const pairs = Object.entries(set).map(([k, v]) => `${k}=${tomlValue(v)}`);
    const r = await cli(['profile', 'set', name, ...pairs]);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : humanProfileError(r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/kits/readme' && req.method === 'GET') {
    const kit = url.searchParams.get('kit');
    if (!kit) return sendJson(res, 400, { ok: false, error: 'need ?kit=' });
    const r = await cli(['kit', 'show', kit]);
    return sendJson(res, r.ok ? 200 : 404, r.ok ? { ok: true, readme: r.stdout } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/kits/packages' && req.method === 'GET') {
    const kit = url.searchParams.get('kit');
    if (!kit || !PKG_NAME_RE.test(kit)) return sendJson(res, 400, { ok: false, error: 'bad kit' });
    const summary = await kitPackages(kit);
    return sendJson(res, summary ? 200 : 404, summary ? { ok: true, kit: summary } : { ok: false, error: 'kit not found' });
  }
  if (url.pathname === '/api/admin/kits' && req.method === 'GET') {
    const r = await cli(['kit', 'list', '--json']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, kits: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/kits/add' && req.method === 'POST') {
    const { kit, copy } = body ?? {};
    if (typeof kit !== 'string' || !kit.length) return sendJson(res, 400, { ok: false, error: 'need {kit}' });
    const args = ['kit', 'add', kit];
    if (copy) args.push('--copy');
    const r = await cli(args);
    return sendJson(res, r.ok ? 200 : 500, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/packages' && req.method === 'GET') {
    const profile = url.searchParams.get('profile') ?? 'default';
    if (!PROFILE_NAME_RE.test(profile)) return sendJson(res, 400, { ok: false, error: BAD_NAME_MSG });
    const r = await cli(['packages', '--json', '--profile', profile]);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, packages: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/configs' && req.method === 'GET') {
    const pkg = url.searchParams.get('package');
    const args = pkg ? ['config', 'list', pkg] : ['config', 'list'];
    const r = await cli(args);
    if (!r.ok) return sendJson(res, 500, { ok: false, error: r.stderr || r.error });
    return sendJson(res, 200, pkg
      ? { ok: true, config: jsonLines(r.stdout)[0] ?? { package: pkg, toml: '' } }
      : { ok: true, configs: jsonLines(r.stdout) });
  }
  if (url.pathname === '/api/admin/configs/set' && req.method === 'POST') {
    const { package: pkg, key, value } = body ?? {};
    if (typeof pkg !== 'string' || !PKG_NAME_RE.test(pkg)) return sendJson(res, 400, { ok: false, error: 'need {package}' });
    if (typeof key !== 'string' || !key.trim()) return sendJson(res, 400, { ok: false, error: 'need {key}' });
    if (typeof value !== 'string') return sendJson(res, 400, { ok: false, error: 'need {value}' });
    const r = await cli(['config', 'set', pkg, key.trim(), value]);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/proposals' && req.method === 'GET') {
    const r = await cli(['config', 'proposals']);
    return sendJson(res, r.ok ? 200 : 500, r.ok ? { ok: true, proposals: jsonLines(r.stdout) } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/proposals/show' && req.method === 'GET') {
    const id = url.searchParams.get('id') ?? '';
    if (!/^[A-Za-z0-9]{1,40}$/.test(id)) return sendJson(res, 400, { ok: false, error: 'bad request id' });
    const r = await cli(['config', 'show', id]);
    return sendJson(res, r.ok ? 200 : 404, r.ok ? { ok: true, diff: r.stdout } : { ok: false, error: r.stderr || r.error });
  }
  if (url.pathname === '/api/admin/proposals/accept' && req.method === 'POST') {
    const id = body?.id;
    if (typeof id !== 'string' || !/^[A-Za-z0-9]{1,40}$/.test(id)) return sendJson(res, 400, { ok: false, error: 'bad request id' });
    const r = await cli(['config', 'accept', id]);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/proposals/decline' && req.method === 'POST') {
    const id = body?.id;
    if (typeof id !== 'string' || !/^[A-Za-z0-9]{1,40}$/.test(id)) return sendJson(res, 400, { ok: false, error: 'bad request id' });
    const r = await cli(['config', 'decline', id]);
    return sendJson(res, r.ok ? 200 : 400, { ok: r.ok, output: r.stdout, error: r.ok ? undefined : (r.stderr || r.error) });
  }
  if (url.pathname === '/api/admin/profile' && (req.method === 'GET' || req.method === 'PUT')) {
    if (!ROOT) return sendJson(res, 503, { ok: false, error: 'no --root; profile editing needs the elanus root' });
    const name = url.searchParams.get('name') ?? 'default';
    if (!PROFILE_NAME_RE.test(name)) return sendJson(res, 400, { ok: false, error: BAD_NAME_MSG });
    const file = profileTomlPath(name);
    if (req.method === 'GET') {
      try {
        const toml = fs.readFileSync(file, 'utf8');
        const parsed = await cli(['profile', 'get', name]);
        return sendJson(res, 200, {
          ok: true,
          name,
          toml,
          profile: parsed.ok ? (jsonLines(parsed.stdout)[0] ?? null) : null,
          profile_error: parsed.ok ? undefined : humanProfileError(parsed.stderr || parsed.error),
        });
      } catch {
        return sendJson(res, 404, { ok: false, error: `no profile.toml for ${name}` });
      }
    }
    if (typeof body?.toml !== 'string') return sendJson(res, 400, { ok: false, error: 'need {toml}' });
    // Validate and write through the CLI. A malformed file would otherwise save
    // as "ok" and then make the agent silently vanish; the CLI validates,
    // writes, commits to config/live, and records the acceptance event.
    const tmp = path.join(os.tmpdir(), `el-profile-candidate-${process.pid}-${Date.now()}.toml`);
    fs.writeFileSync(tmp, body.toml);
    const v = await cli(['profile', 'put', name, tmp]);
    fs.rmSync(tmp, { force: true });
    return sendJson(res, v.ok ? 200 : 400, v.ok ? { ok: true, name } : { ok: false, error: humanProfileError(v.stderr || v.error) });
  }
  sendJson(res, 404, { ok: false, error: 'unknown admin endpoint' });
}

function broadcast(obj) {
  const line = `data: ${JSON.stringify(obj)}\n\n`;
  for (const res of sseClients) res.write(line);
}

// ---- http side ------------------------------------------------------------
const MIME = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.mjs': 'text/javascript', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.ico': 'image/x-icon' };

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  // Per-request line, except the SSE stream (which never "finishes" — logged
  // on open/close below instead). This is the spine of backend observability:
  // every admin/history/publish call shows up here with its status and timing.
  const startedAt = Date.now();
  res.on('finish', () => {
    if (url.pathname !== '/api/stream') log('http', `${req.method} ${url.pathname}${url.search} → ${res.statusCode} (${Date.now() - startedAt}ms)`);
  });
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
    log('sse', `client connected (${sseClients.size} total)`);
    req.on('close', () => { sseClients.delete(res); log('sse', `client disconnected (${sseClients.size} total)`); });
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
    if (!originOk(req)) {
      res.writeHead(403, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: 'cross-origin request refused' }));
      return;
    }
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
        log('pub', `${topic}${correlation ? ` corr=${correlation}` : ''} → ${err ? 'ERR ' + (err.message ?? err) : 'ok'}`);
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

// Smoke: a REAL daemon on a throwaway root, the web server as the pure MQTT
// client, a plain HTTP client as the browser. Proves: SSE relay (bus → page),
// publish endpoint (page → bus → ledger, correlation intact), ring catch-up.
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import mqtt from 'mqtt';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const BIN = path.join(REPO, 'target/debug');
const TMP = fs.mkdtempSync('/tmp/elanus-web-smoke.');
const BUS_PORT = 18000 + (process.pid % 2000);
const WEB_PORT = 7300 + (process.pid % 500);
const ENV = { ...process.env, HARNESS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}` };

let failures = 0;
const ok = (m) => console.log(`  ok: ${m}`);
const fail = (m) => { console.error(`FAIL: ${m}`); failures++; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(desc, fn, timeoutMs = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (await fn()) { ok(desc); return true; }
    await sleep(100);
  }
  fail(`${desc} (timed out)`);
  return false;
}
const elanus = (...a) => execFileSync(path.join(BIN, 'elanus'), a, { env: ENV, encoding: 'utf8' });
const sql = (q) => execFileSync('sqlite3', [path.join(TMP, 'harness.db'), q], { encoding: 'utf8' }).trim();

// -- daemon on a throwaway root --
elanus('init');
fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
const daemon = spawn(path.join(BIN, 'elanus'), ['daemon', '--interval-ms', '200'], { env: ENV, stdio: 'ignore' });
const probe = mqtt.connect(`mqtt://127.0.0.1:${BUS_PORT}`, { protocolVersion: 5, reconnectPeriod: 300 });
await waitFor('daemon listener bound', () => new Promise((r) => { probe.connected ? r(true) : probe.once('connect', () => r(true)); setTimeout(() => r(probe.connected), 250); }));

// -- the server under test --
const server = spawn('node', [path.join(REPO, 'ui/web/server.mjs'), '--root', TMP, '--port', String(WEB_PORT)], {
  env: ENV, stdio: ['ignore', 'pipe', 'inherit'],
});
const BASE = `http://127.0.0.1:${WEB_PORT}`;
await waitFor('web server up', async () => {
  try { return (await fetch(`${BASE}/`)).ok; } catch { return false; }
});

// -- SSE client (the browser) --
const events = [];
const sse = await fetch(`${BASE}/api/stream`);
const reader = sse.body.getReader();
(async () => {
  const dec = new TextDecoder();
  let buf = '';
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf('\n\n')) !== -1) {
      const chunk = buf.slice(0, i); buf = buf.slice(i + 2);
      const line = chunk.split('\n').find((l) => l.startsWith('data: '));
      if (line) { try { events.push(JSON.parse(line.slice(6))); } catch {} }
    }
  }
})();
await waitFor('SSE status: bus connected', () => events.some((m) => m.kind === 'status' && m.connected));

// 1. bus → page
elanus('bus', 'pub', 'obs/test/web', '{"msg":"web-smoke"}');
await waitFor('bus event relayed over SSE', () =>
  events.some((m) => m.kind === 'message' && m.topic === 'obs/test/web' && m.env?.payload?.msg === 'web-smoke'));

// 2. an ask announces and relays (daemon sweep, not the publish echo)
elanus('emit', 'in/human/owner', '--correlation', 'web-corr-1', '--payload', '{"question":"deploy?","options":["yes","no"]}');
await waitFor('ask relayed with correlation', () =>
  events.some((m) => m.topic === 'in/human/owner' && m.env?.correlation_id === 'web-corr-1'));

// 3. page → bus: answer with correlation, lands in the ledger correctly
const r = await fetch(`${BASE}/api/publish`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ topic: 'in/agent/main', payload: { answer: 'yes' }, correlation: 'web-corr-1' }),
});
const rj = await r.json();
rj.ok ? ok('publish accepted (PUBACK relayed)') : fail(`publish rejected: ${rj.error ?? r.status}`);
await waitFor('answer in the ledger with the ask correlation', () =>
  sql(`SELECT COUNT(*) FROM events WHERE type='in/agent/main' AND correlation_id='web-corr-1' AND json_extract(payload,'$.answer')='yes'`) === '1');

// 4. wildcard publish rejected
const bad = await fetch(`${BASE}/api/publish`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ topic: 'in/#', payload: {} }),
});
bad.status === 400 ? ok('wildcard publish rejected') : fail(`wildcard publish got ${bad.status}`);

// 5. ring catch-up: a LATE browser still sees history
const late = await fetch(`${BASE}/api/stream`);
const lateText = await new Promise(async (resolve) => {
  const rd = late.body.getReader();
  const dec = new TextDecoder();
  let buf = '';
  const t = setTimeout(() => { rd.cancel(); resolve(buf); }, 1500);
  for (;;) {
    const { done, value } = await rd.read().catch(() => ({ done: true }));
    if (done) break;
    buf += dec.decode(value, { stream: true });
    if (buf.includes('web-smoke')) { clearTimeout(t); rd.cancel(); resolve(buf); break; }
  }
});
lateText.includes('web-smoke') ? ok('late joiner got ring catch-up') : fail('no catch-up for late joiner');

// -- teardown --
server.kill('SIGKILL');
daemon.kill('SIGKILL');
probe.end(true);
try { execFileSync('pkill', ['-9', '-f', TMP]); } catch {}
console.log(failures === 0 ? 'ALL PASS' : `${failures} failure(s)`);
process.exit(failures === 0 ? 0 : 1);

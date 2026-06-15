// Smoke: a REAL daemon on a throwaway root, the web server as the pure MQTT
// client, a plain HTTP client as the browser. Proves: SSE relay (bus → page),
// publish endpoint (page → bus → ledger, correlation intact), ring catch-up,
// and the history view in BOTH states — absent (live-only degradation: /api/
// history → 503, no endpoint file) and installed+approved (queries proxied
// end to end to the package's harness-negotiated HTTP endpoint).
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
// .timeout: the daemon holds the db in WAL; writers must wait, not fail
const sql = (q) => execFileSync('sqlite3', ['-cmd', '.timeout 5000', path.join(TMP, 'harness.db'), q], { encoding: 'utf8' }).trim();

// -- daemon on a throwaway root --
elanus('init');
fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
const daemon = spawn(path.join(BIN, 'elanus'), ['daemon', '--interval-ms', '200'], { env: ENV, stdio: 'ignore' });
// The probe acts as the owner: present the owner credential (minted at init)
// so it is accepted once unauthenticated connections are denied.
const humanSecret = fs.readFileSync(path.join(TMP, '.secrets', 'owner'), 'utf8').trim();
const probe = mqtt.connect(`mqtt://127.0.0.1:${BUS_PORT}`, { protocolVersion: 5, reconnectPeriod: 300, username: 'owner', password: humanSecret });
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

// 6. history view: a stdlib package, serving out of the box (docs/config.md).
// No install dance — init auto-approves it — so /api/history answers 200, and
// an unknown kind is still rejected (400) before any proxy. (The degraded 503
// path is exercised in 7b after a --force revoke.)
await waitFor('history serving out of the box (stdlib, approved at init)', async () => {
  return (await fetch(`${BASE}/api/history?kind=agents`)).status === 200;
}, 30000);
const badKind = await fetch(`${BASE}/api/history?kind=drop_tables`);
badKind.status === 400 ? ok('unknown history kind rejected (400)') : fail(`bad kind got ${badKind.status}`);

// 7. seed a transcript and query it through the already-serving view. history
// is a stdlib package (present + approved at init), so there is no cp/approve
// step — just seed the ledger and ask.
/^history\s/m.test(elanus('packages')) ? ok('history package present (stdlib)') : fail('history package missing');
sql(`INSERT INTO events(type, payload, state, correlation_id)
     VALUES ('in/agent/main','{"prompt":"hi"}','done','web-hist-conv');
     INSERT INTO messages(session_id, role, content, event_id) VALUES
       ('s-hist-test','user','{"role":"user","text":"hi"}', last_insert_rowid()),
       ('s-hist-test','assistant','{"role":"assistant","text":"hello","tool_calls":[{"call_id":"c1","fn_name":"shell","fn_arguments":"{\\"cmd\\":\\"ls\\"}"}]}',
        (SELECT id FROM events WHERE correlation_id='web-hist-conv')),
       ('s-hist-test','tool','{"role":"tool","tool_call_id":"c1","name":"shell","content":"ok"}',
        (SELECT id FROM events WHERE correlation_id='web-hist-conv'));`);

const hist = async (params) => {
  const r = await fetch(`${BASE}/api/history?${new URLSearchParams(params)}`);
  return { status: r.status, body: await r.json().catch(() => null) };
};
// the supervisor boots the actor on its next tick (with backoff if it raced
// the approval), so the first probe retries until the view answers
// The actor booted at init; the seed above lands in the ledger after that, so
// poll until the read-only view sees the seeded session (not merely until it
// answers 200), to be robust to read-visibility timing.
let agentsResp = null;
await waitFor('history actor serves the seeded session', async () => {
  const { status, body } = await hist({ kind: 'agents' });
  const main = status === 200 && body?.ok && (body.agents ?? []).find((a) => a.agent === 'main');
  if (main && main.sessions.includes('s-hist-test')) { agentsResp = body; return true; }
  return false;
}, 30000);
if (agentsResp) {
  const main = (agentsResp.agents ?? []).find((a) => a.agent === 'main');
  main && main.sessions.includes('s-hist-test')
    ? ok('agents query: main + seeded session')
    : fail(`agents query wrong: ${JSON.stringify(agentsResp)}`);

  const sess = await hist({ kind: 'sessions', agent: 'main' });
  const s = (sess.body?.sessions ?? []).find((x) => x.session === 's-hist-test');
  s && s.message_count === 3 && s.first_ts && s.last_ts
    ? ok('sessions query: counts + timestamps')
    : fail(`sessions query wrong: ${JSON.stringify(sess.body)}`);

  const tr = await hist({ kind: 'transcript', session: 's-hist-test' });
  const roles = (tr.body?.messages ?? []).map((m) => m.role).join(',');
  roles === 'user,assistant,tool' && tr.body.has_more === false
    && tr.body.messages[1].content.tool_calls?.[0]?.fn_name === 'shell'
    ? ok('transcript query: roles in order, tool call intact')
    : fail(`transcript wrong: ${JSON.stringify(tr.body)}`);

  // pagination: limit=2 → the LAST two messages, has_more, then page back
  const page = await hist({ kind: 'transcript', session: 's-hist-test', limit: '2' });
  const pRoles = (page.body?.messages ?? []).map((m) => m.role).join(',');
  pRoles === 'assistant,tool' && page.body.has_more === true
    ? ok('transcript pagination: tail page + has_more')
    : fail(`pagination wrong: ${JSON.stringify(page.body)}`);
  const earlier = await hist({ kind: 'transcript', session: 's-hist-test', before_id: String(page.body?.messages?.[0]?.id ?? 0) });
  (earlier.body?.messages ?? []).map((m) => m.role).join(',') === 'user' && earlier.body.has_more === false
    ? ok('transcript pagination: before_id pages back')
    : fail(`before_id wrong: ${JSON.stringify(earlier.body)}`);

  const conv = await hist({ kind: 'conversation', correlation: 'web-hist-conv' });
  (conv.body?.events ?? []).some((e) => e.type === 'in/agent/main' && e.payload?.prompt === 'hi')
    ? ok('conversation query: events by correlation')
    : fail(`conversation wrong: ${JSON.stringify(conv.body)}`);

  const err = await hist({ kind: 'transcript' }); // missing {session}
  err.status === 400 && err.body?.ok === false && /session/.test(err.body.error ?? '')
    ? ok('view reports per-query errors as real 400s')
    : fail(`error path wrong: ${err.status} ${JSON.stringify(err.body)}`);

  // the search DSL over POST: filter x projection (truncate) x pagination
  const search = await fetch(`${BASE}/api/history`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ kind: 'search', filter: { roles: ['tool'] }, select: { tool_results: { truncate: 1 } } }),
  });
  const sj = await search.json().catch(() => null);
  const hit = (sj?.messages ?? []).find((m) => m.session === 's-hist-test');
  search.status === 200 && hit && hit.content?.content === 'o…' && hit.content?.truncated_to === 1
    ? ok('search DSL: role filter + tool_result truncation projection')
    : fail(`search DSL wrong: ${search.status} ${JSON.stringify(sj)}`);
}

// 7b. the server degrades honestly when the endpoint is unreachable: with the
// discovery file gone, /api/history is the live-only 503 the UI shows as a hint
// — never a hang or a 500 — and it heals the instant the file returns (the
// server re-reads it per request; the actor itself never stopped).
const histJson = path.join(TMP, 'run/pkg-history/http.json');
const savedHist = fs.readFileSync(histJson, 'utf8');
fs.rmSync(histJson);
(await fetch(`${BASE}/api/history?kind=agents`)).status === 503
  ? ok('history endpoint unreachable → 503 live-only degradation')
  : fail('degraded path did not 503 with the discovery file removed');
fs.writeFileSync(histJson, savedHist);
(await fetch(`${BASE}/api/history?kind=agents`)).status === 200
  ? ok('history heals immediately when the endpoint returns')
  : fail('history did not heal after restoring the discovery file');

// 8. admin = staging only (HANDOFF phase 5): the server composes pending
// state via the CLI and edits profile files, but never commits a grant.
const kitsResp = await (await fetch(`${BASE}/api/admin/kits`)).json();
(kitsResp.ok && kitsResp.kits.some((k) => k.name === 'dev'))
  ? ok('admin: kit list (dev kit resolvable)')
  : fail(`admin kits wrong: ${JSON.stringify(kitsResp)}`);

const add = await (await fetch(`${BASE}/api/admin/kits/add`, {
  method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kit: 'dev' }),
})).json();
add.ok && add.staged ? ok('admin: kit add stages') : fail(`kit add wrong: ${JSON.stringify(add)}`);
sql(`SELECT COUNT(*) FROM grants WHERE package='git-protect' AND state='approved'`) === '0'
  ? ok('admin: staging granted nothing')
  : fail('admin kit add approved grants');

const pkgs = await (await fetch(`${BASE}/api/admin/packages`)).json();
const gp = (pkgs.packages ?? []).find((p) => p.name === 'git-protect');
gp && gp.grants.some((g) => g.state === 'requested')
  ? ok('admin: pending queue shows the staged requests')
  : fail(`admin packages wrong: ${JSON.stringify(gp)}`);

const prof0 = await (await fetch(`${BASE}/api/admin/profile?name=default`)).json();
prof0.ok && /agent/.test(prof0.toml) ? ok('admin: profile read') : fail(`profile read wrong: ${JSON.stringify(prof0)}`);
await fetch(`${BASE}/api/admin/profile?name=default`, {
  method: 'PUT', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ toml: prof0.toml + '\n# edited-by-ui\n' }),
});
const prof1 = await (await fetch(`${BASE}/api/admin/profile?name=default`)).json();
/edited-by-ui/.test(prof1.toml) ? ok('admin: profile write round-trips') : fail('profile write lost');
const trav = await fetch(`${BASE}/api/admin/profile?name=../evil`);
trav.status === 400 ? ok('admin: profile name traversal rejected') : fail(`traversal got ${trav.status}`);

// 9. agent management: profiles ARE agents — list them from disk, create
// one, reconfigure it, rename it (the kernel's profile CLI does the toml).
const ags0 = await (await fetch(`${BASE}/api/admin/agents`)).json();
ags0.ok && ags0.profiles.some((p) => p.profile === 'default')
  ? ok('admin: agents listed from profiles on disk')
  : fail(`agents list wrong: ${JSON.stringify(ags0)}`);
const mk = await (await fetch(`${BASE}/api/admin/agents`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ name: 'kestrel', model: 'claude-haiku-4-5-20251001' }),
})).json();
mk.ok ? ok('admin: agent created') : fail(`create failed: ${JSON.stringify(mk)}`);
const setr = await (await fetch(`${BASE}/api/admin/agents/set`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ name: 'kestrel', set: { agent: 'falcon', 'model.max_turns': 9 } }),
})).json();
setr.ok ? ok('admin: agent reconfigured + renamed') : fail(`set failed: ${JSON.stringify(setr)}`);
const ags1 = await (await fetch(`${BASE}/api/admin/agents`)).json();
const kf = (ags1.profiles ?? []).find((p) => p.profile === 'kestrel');
kf && kf.agent === 'falcon' && kf.max_turns === 9 && kf.model === 'claude-haiku-4-5-20251001'
  ? ok('admin: rename + knobs visible through the kernel loader')
  : fail(`reloaded profile wrong: ${JSON.stringify(kf)}`);
// Arrays must round-trip as TOML arrays — the exact regression Tim hit:
// skills.include arrived as the STRING "[\"*\"]" and the kernel refused it.
const arrset = await (await fetch(`${BASE}/api/admin/agents/set`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ name: 'kestrel', set: { 'skills.include': ['#'], 'skills.exclude': ['notes'], 'sandbox.workdir': '' } }),
})).json();
arrset.ok ? ok('admin: array knobs accepted') : fail(`array set failed: ${JSON.stringify(arrset)}`);
const ags2 = await (await fetch(`${BASE}/api/admin/agents`)).json();
const k2 = (ags2.profiles ?? []).find((p) => p.profile === 'kestrel');
JSON.stringify(k2?.skills?.include) === '["#"]' && JSON.stringify(k2?.skills?.exclude) === '["notes"]'
  ? ok('admin: skills arrays round-trip through the kernel loader')
  : fail(`skills wrong after array set: ${JSON.stringify(k2?.skills)}`);

const badset = await fetch(`${BASE}/api/admin/agents/set`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ name: 'kestrel', set: { 'model.max_turns': 'lots' } }),
});
badset.status === 400 ? ok('admin: invalid set refused before it lands') : fail(`bad set got ${badset.status}`);
const rdme = await (await fetch(`${BASE}/api/admin/kits/readme?kit=dev`)).json();
rdme.ok && /git|workdir/i.test(rdme.readme) ? ok('admin: kit readme preview') : fail(`readme wrong: ${JSON.stringify(rdme)}`);

// 10. commits from the UI (Tim's call: same authority as the terminal,
// because it shells out to it) — guarded against the browser-only threats.
const appr = await (await fetch(`${BASE}/api/admin/approve`, {
  method: 'POST', headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ package: 'git-protect' }),
})).json();
appr.ok ? ok('admin: approve commits from the UI') : fail(`approve failed: ${JSON.stringify(appr)}`);
sql(`SELECT COUNT(*) FROM grants WHERE package='git-protect' AND state='approved' AND decided_by='ui'`) !== '0'
  ? ok('admin: ledger trail says decided_by=ui')
  : fail('no decided_by=ui rows after UI approve');
const csrf = await fetch(`${BASE}/api/admin/approve`, {
  method: 'POST',
  headers: { 'content-type': 'application/json', origin: 'https://evil.example' },
  body: JSON.stringify({ package: 'git-protect' }),
});
csrf.status === 403 ? ok('admin: hostile-origin mutation refused (CSRF guard)') : fail(`cross-origin approve got ${csrf.status}`);
// fetch/undici refuses to forge Host; curl doesn't.
const rebindCode = execFileSync('curl', ['-s', '-o', '/dev/null', '-w', '%{http_code}',
  '-X', 'POST', '-H', 'Host: attacker.example', '-H', 'content-type: application/json',
  '-d', '{"package":"git-protect"}', `${BASE}/api/admin/approve`], { encoding: 'utf8' });
rebindCode === '403' ? ok('admin: non-local Host refused (DNS-rebinding guard)') : fail(`rebound host got ${rebindCode}`);
const models = await (await fetch(`${BASE}/api/admin/models`)).json();
models.ok && Array.isArray(models.models)
  ? ok('admin: models endpoint answers gracefully (empty list without a provider)')
  : fail(`models wrong: ${JSON.stringify(models)}`);
const kitsHere = await (await fetch(`${BASE}/api/admin/kits`)).json();
kitsHere.kits.some((k) => k.name === 'core')
  ? ok('admin: core kit resolvable from <root>/kits (seeded at init)')
  : fail(`core kit missing: ${JSON.stringify(kitsHere.kits?.map((k) => k.name))}`);

// -- teardown --
server.kill('SIGKILL');
daemon.kill('SIGKILL');
probe.end(true);
try { execFileSync('pkill', ['-9', '-f', TMP]); } catch {}
console.log(failures === 0 ? 'ALL PASS' : `${failures} failure(s)`);
process.exit(failures === 0 ? 0 : 1);

// Focused SSE reconnect regression: quiet stream keepalive plus browser recovery
// after the first /api/stream connection is interrupted.
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const BIN = path.join(REPO, 'target/debug');
const TMP = fs.mkdtempSync('/tmp/lanius-sse-reconnect.');
const BUS_PORT = 23000 + (process.pid % 2000);
const WEB_PORT = 9800 + (process.pid % 500);
const BASE = `http://127.0.0.1:${WEB_PORT}`;
const WEB_LOG = path.join(TMP, 'web.log');
const ENV = { ...process.env, LANIUS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}`, LANIUS_WEB_LOG: WEB_LOG };

let failures = 0;
let browser = null;
let server = null;
let daemon = null;

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
const lanius = (...a) => execFileSync(path.join(BIN, 'lanius'), a, { env: ENV, encoding: 'utf8' });

async function readUntil(url, needle, timeoutMs) {
  const res = await fetch(url);
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let text = '';
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      const left = Math.max(1, deadline - Date.now());
      const read = reader.read();
      const timeout = new Promise((resolve) => setTimeout(() => resolve({ done: true, timeout: true }), left));
      const chunk = await Promise.race([read, timeout]);
      if (chunk.done) break;
      text += dec.decode(chunk.value, { stream: true });
      if (text.includes(needle)) return true;
    }
    return false;
  } finally {
    await reader.cancel().catch(() => {});
  }
}

try {
  lanius('init');
  fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
  daemon = spawn(path.join(BIN, 'lanius'), ['daemon', '--interval-ms', '200'], { env: ENV, stdio: 'ignore' });
  server = spawn(path.join(BIN, 'lanius'), ['web', '--port', String(WEB_PORT)], { env: ENV, stdio: ['ignore', 'pipe', 'inherit'] });
  await waitFor('web server up', async () => {
    try { return (await fetch(`${BASE}/`)).ok; } catch { return false; }
  }, 20000);

  await waitFor('SSE emits ping keepalive on an otherwise quiet stream', () =>
    readUntil(`${BASE}/api/stream`, 'event: ping', 20000), 22000);

  browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ baseURL: BASE });
  const page = await ctx.newPage();
  const consoleErrors = [];
  page.on('console', (msg) => {
    if (msg.type() !== 'error') return;
    const text = msg.text();
    if (/\/api\/stream/i.test(text) && /interrupted|incomplete|ERR_/i.test(text)) return;
    consoleErrors.push(text);
  });
  page.on('pageerror', (err) => consoleErrors.push(`[pageerror] ${err.message}`));

  let streamHits = 0;
  await page.route('**/api/stream', async (route) => {
    streamHits += 1;
    if (streamHits === 1) {
      await route.fulfill({
        status: 200,
        headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
        body: 'retry: 10\n\ndata: {"kind":"status","connected":false,"broker":"forced-close","agent":"main"}\n\n',
      });
      return;
    }
    await route.continue();
  });

  await page.goto('/');
  await page.waitForSelector('#conn-text', { timeout: 10000 });
  await waitFor('browser opens a replacement /api/stream after interruption', () => streamHits >= 2, 8000);
  await waitFor('UI connection indicator heals to connected', async () =>
    /connected/i.test(await page.$eval('#conn-text', (el) => el.textContent).catch(() => '')), 10000);

  await page.evaluate(async () => {
    await fetch('/api/publish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        topic: 'in/agent/main/code-sse-reconnect',
        payload: { prompt: 'urgent after reconnect', priority: 9, event_id: 88001 },
      }),
    });
  });
  await waitFor('live MQTT event arrives through the reconnected stream', async () =>
    page.$eval('#signal-lamp', (el) => el.classList.contains('lit')).catch(() => false), 10000);

  if (consoleErrors.length) fail(`unexpected browser error(s):\n${consoleErrors.join('\n')}`);
  else ok('no unexpected browser errors');
} finally {
  if (browser) await browser.close().catch(() => {});
  if (server) server.kill('SIGKILL');
  if (daemon) daemon.kill('SIGKILL');
  try { execFileSync('pkill', ['-9', '-f', TMP]); } catch {}
  fs.rmSync(TMP, { recursive: true, force: true });
}

console.log(failures === 0 ? 'ALL PASS' : `${failures} failure(s)`);
process.exit(failures === 0 ? 0 : 1);

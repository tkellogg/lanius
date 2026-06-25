// Walkthrough: screenshots of every view into /tmp/elanus-ui-shots/.
// Same stack pattern as smoke.mjs / ui.spec.mjs. Human reviews the shots;
// taste problems are caught by looking, not asserting.
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const BIN = path.join(REPO, 'target/debug');
const TMP = fs.mkdtempSync('/tmp/elanus-ui-walk.');
// Offset by 5000 from smoke, 2000 from spec, to avoid collisions.
const BUS_PORT = 23000 + (process.pid % 2000);
const WEB_PORT = 9800 + (process.pid % 500);
const BASE = `http://127.0.0.1:${WEB_PORT}`;
const ENV = { ...process.env, ELANUS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}` };
const SHOTS = '/tmp/elanus-ui-shots';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, timeoutMs = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (await fn()) return true;
    await sleep(100);
  }
  return false;
}
const elanus = (...a) => execFileSync(path.join(BIN, 'elanus'), a, { env: ENV, encoding: 'utf8' });

// Clear and recreate the shots directory.
fs.rmSync(SHOTS, { recursive: true, force: true });
fs.mkdirSync(SHOTS, { recursive: true });

const shots = [];
async function shot(page, name) {
  const file = path.join(SHOTS, `${name}.png`);
  await page.screenshot({ path: file, fullPage: false });
  shots.push(file);
  console.log(`  shot: ${file}`);
}

// -- stack setup --
elanus('init');
fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
const daemon = spawn(path.join(BIN, 'elanus'), ['daemon', '--interval-ms', '200'], { env: ENV, stdio: 'ignore' });
const server = spawn(path.join(BIN, 'elanus'), ['web', '--port', String(WEB_PORT)], {
  env: ENV, stdio: ['ignore', 'pipe', 'inherit'],
});
await waitFor(async () => { try { return (await fetch(`${BASE}/`)).ok; } catch { return false; } }, 20000);

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ baseURL: BASE, viewport: { width: 1280, height: 800 } });

// ── 1. blank root — welcome front door, then signals ──────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.waitForSelector('#view-welcome');
  await sleep(1500); // let SSE connect + disk agents load
  await shot(page, '01-welcome-front-door');
  await page.click('.nav-signals');
  await page.waitForSelector('#view-rail:not([hidden])');
  await shot(page, '01b-signals-blank-root');
  await page.close();
}

// ── 2. create an agent + new-agent form ───────────────────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await sleep(800);
  await page.fill('#na-name', 'kestrel');
  await page.fill('#na-model', 'claude-haiku-4-5-20251001');
  await shot(page, '02-new-agent-form');
  await page.click('#na-create');
  await waitFor(async () => { const h = await page.$eval('#view-configure', (el) => el.hidden); return !h; }, 8000);
  await sleep(500);
  await shot(page, '03-configure-tab-fresh');
  await page.close();
}

// ── 3. configure tab (filled) ─────────────────────────────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  // Select kestrel agent.
  await waitFor(async () => {
    const items = await page.$$('#nav-agents .nav-item');
    for (const item of items) {
      if ((await item.textContent()).includes('kestrel')) { await item.click(); return true; }
    }
    return false;
  }, 8000);
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])');
  await sleep(1000);
  // Fill form values so the shot shows a configured state.
  await page.fill('#cfg-model', 'claude-haiku-4-5-20251001');
  await page.fill('#cfg-turns', '12');
  await page.fill('#cfg-include', '#');
  await page.fill('#cfg-exclude', 'notes');
  await shot(page, '04-configure-tab-filled');
  await page.close();
}

// ── 4. add-ons — catalog ─────────────────────────────────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await waitFor(async () => {
    const text = await page.$eval('#setup-kits', (el) => el.textContent);
    return text.includes('dev') || text.includes('core');
  }, 10000);
  await shot(page, '05-kits-catalog');
  // Expand a readme.
  const rows = await page.$$('.setup-kit');
  for (const row of rows) {
    const name = await row.$eval('.setup-kit-name', (el) => el.textContent).catch(() => '');
    if (name.includes('dev')) {
      const btn = await row.$('button.ghost');
      if (btn) { await btn.click(); await sleep(800); break; }
    }
  }
  await shot(page, '06-kits-readme-expanded');
  // Add the dev kit.
  for (const row of await page.$$('.setup-kit')) {
    const name = await row.$eval('.setup-kit-name', (el) => el.textContent).catch(() => '');
    if (name.includes('dev')) {
      const btns = await row.$$('button:not(.ghost)');
      for (const btn of btns) {
        if (/\badd\b/i.test(await btn.textContent())) { await btn.click(); break; }
      }
      break;
    }
  }
  // Wait for the installed list to populate.
  await waitFor(async () => {
    const text = await page.$eval('#setup-configs', (el) => el.textContent);
    return /git-protect/i.test(text);
  }, 10000);
  await shot(page, '07-add-ons-installed');
  await waitFor(async () => /no agent requests/i.test(await page.$eval('#setup-pending', (el) => el.textContent)), 10000);
  await shot(page, '08-agent-requests-empty');
  await page.close();
}

// ── 5. converse — sent message ────────────────────────────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  const firstBtn = await page.$('#nav-agents .nav-item');
  if (firstBtn) await firstBtn.click();
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  await page.fill('#compose-input', 'hello from the walkthrough');
  await shot(page, '09-compose-ready');
  await page.click('#compose-send');
  await sleep(1500);
  await shot(page, '10-converse-sent');
  // A harness-emitted failure threads into the conversation as an explicit
  // error bubble — the realistic out-of-box state (agent can't reach a model).
  const corr = await page.$eval('#conv-holder .msg.you', (el) => (el.title || '').replace('correlation ', '')).catch(() => 'wt-fail');
  elanus('emit', 'in/human/owner', '--correlation', corr || 'wt-fail', '--payload',
    JSON.stringify({ failed: true, error: 'llm call failed (model claude-…): connection refused', agent: 'main' }));
  await sleep(1200);
  await shot(page, '10b-converse-failure');
  await page.close();
}

// ── 6. sessions tab (history absent — live-only hint) ─────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  const firstBtn = await page.$('#nav-agents .nav-item');
  if (firstBtn) await firstBtn.click();
  await page.click('[data-tab="sessions"]');
  await page.waitForSelector('#view-sessions:not([hidden])');
  await sleep(2500); // let history probe settle
  await shot(page, '11-sessions-degraded');
  await page.close();
}

// ── 7. telemetry rail ─────────────────────────────────────────────────────────
{
  const page = await ctx.newPage();
  await page.goto('/');
  // Emit a few bus messages so the rail has something to show.
  try { elanus('bus', 'pub', 'obs/agent/kestrel/s1/think', '{"step":"plan"}'); } catch {}
  try { elanus('bus', 'pub', 'in/agent/kestrel', '{"prompt":"walk"}'); } catch {}
  await page.waitForSelector('#nav-agents .nav-item');
  const firstBtn = await page.$('#nav-agents .nav-item');
  if (firstBtn) await firstBtn.click();
  await page.click('[data-tab="telemetry"]');
  await page.waitForSelector('#view-rail:not([hidden])');
  await sleep(1500);
  await shot(page, '12-telemetry-rail');
  await page.close();
}

// -- teardown --
await browser.close();
server.kill('SIGKILL');
daemon.kill('SIGKILL');
try { execFileSync('pkill', ['-9', '-f', TMP]); } catch {}

console.log('\nscreenshots saved:');
for (const f of shots) console.log(`  ${f}`);

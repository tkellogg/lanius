// Browser e2e: headless Chromium against a live stack (daemon + web server).
// Same stack pattern as smoke.mjs — throwaway root, unique ports from pid.
// Page errors and console errors are test failures; that is the main value
// of this layer over the HTTP smoke.
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const BIN = path.join(REPO, 'target/debug');
const TMP = fs.mkdtempSync('/tmp/elanus-ui-spec.');
// Offset by 3000 from smoke.mjs to avoid collisions when both run together.
const BUS_PORT = 21000 + (process.pid % 2000);
const WEB_PORT = 9300 + (process.pid % 500);
const BASE = `http://127.0.0.1:${WEB_PORT}`;
const ENV = { ...process.env, ELANUS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}` };

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

// -- stack setup (mirrors smoke.mjs) --
elanus('init');
fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
const daemon = spawn(path.join(BIN, 'elanus'), ['daemon', '--interval-ms', '200'], { env: ENV, stdio: 'ignore' });
const server = spawn('node', [path.join(REPO, 'ui/web/server.mjs'), '--root', TMP, '--port', String(WEB_PORT)], {
  env: ENV, stdio: ['ignore', 'pipe', 'inherit'],
});
await waitFor('web server up', async () => {
  try { return (await fetch(`${BASE}/`)).ok; } catch { return false; }
}, 20000);

// -- browser --
const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ baseURL: BASE });

// Shared page error accumulator — every test that opens a page attaches to this.
const pageErrors = [];
const consoleErrors = [];

async function newPage() {
  const page = await ctx.newPage();
  page.on('pageerror', (err) => {
    const msg = `[pageerror] ${err.message}`;
    pageErrors.push(msg);
    console.error(`BROWSER ERROR: ${msg}`);
  });
  page.on('console', (msg) => {
    if (msg.type() !== 'error') return;
    const t = msg.text();
    // Expected: font CDN unavailable in headless, history 503 before the
    // probe settles, model list empty without a provider.
    if (/fonts\.googleapis|fonts\.gstatic/i.test(t)) return;
    if (/model list unavailable/i.test(t)) return;
    // history probe returns 503 before the package is installed — expected.
    if (/503|Service Unavailable/i.test(t)) return;
    consoleErrors.push(`[console.error] ${t}`);
    console.error(`BROWSER CONSOLE ERR: ${t}`);
  });
  return page;
}

// Waits for the configure form to finish loading by checking that the model
// field is non-empty OR cfg-note shows an error (no profile case). Without
// this the haiku model's default max_turns=24 races the test's fill() calls.
async function waitForConfigureLoaded(page) {
  const t0 = Date.now();
  while (Date.now() - t0 < 8000) {
    const note = await page.$eval('#cfg-note', (el) => el.textContent).catch(() => '');
    const model = await page.$eval('#cfg-model', (el) => el.value).catch(() => '');
    // loadConfigure finishes by populating cfg-model (or setting an error note)
    if (model.length > 0 || note.includes('no profile')) return;
    await sleep(80);
  }
}

// ── flow 1: boot ─────────────────────────────────────────────────────────────
// Welcome view is the front door on load, routing to the primary agent; the
// default agent appears in nav from disk.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents', { timeout: 10000 });
  // The harness init seeds a 'default' profile → agent 'main' in the nav.
  await waitFor('boot: default agent visible in nav', async () => {
    const items = await page.$$eval('#nav-agents .nav-item', (els) => els.map((e) => e.textContent));
    return items.some((t) => t.includes('main') || t.includes('default'));
  });
  const welcomeVisible = await page.$eval('#view-welcome', (el) => !el.hidden);
  welcomeVisible ? ok('boot: welcome view is the front door on load') : fail('boot: welcome view hidden on load');
  // The welcome routes to the primary agent — converse button is present.
  await waitFor('boot: welcome offers the primary agent', async () => {
    const t = await page.$eval('#welcome-agent', (el) => el.textContent).catch(() => '');
    return /converse with/.test(t);
  });
  // Agent tabs must NOT show on welcome (the [hidden] regression guard).
  const tabsHidden = await page.$eval('#agent-tabs', (el) => el.hidden && getComputedStyle(el).display === 'none');
  tabsHidden ? ok('boot: agent tabs hidden off-agent (no leak)') : fail('boot: agent tabs leaked onto welcome');
  // Signals still reachable from the nav.
  await page.click('.nav-signals');
  await page.waitForSelector('#view-rail:not([hidden])', { timeout: 5000 });
  ok('boot: signals reachable from nav');
  await page.close();
}

// ── flow 2: new agent ────────────────────────────────────────────────────────
// Navigate to setup, create an agent, assert it appears in nav.
const testAgentProfile = 'harrier';
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('.nav-setup');
  await page.click('.nav-setup');
  await page.waitForSelector('#na-name', { state: 'visible' });
  await page.fill('#na-name', testAgentProfile);
  await page.fill('#na-model', 'claude-haiku-4-5-20251001');
  await page.click('#na-create');
  await waitFor('new agent: configure tab opens', async () => {
    return !(await page.$eval('#view-configure', (el) => el.hidden));
  }, 10000);
  await waitFor('new agent: appears in nav', async () => {
    const items = await page.$$eval('#nav-agents .nav-item', (els) => els.map((e) => e.textContent));
    return items.some((t) => t.includes(testAgentProfile));
  });
  await page.close();
}

// ── flow 3: configure save + reload ──────────────────────────────────────────
// The exact layer that broke: form→server encoding of skills.include as array.
// Set model, max turns 7, include '#', exclude 'notes', save, assert note,
// reload page, assert values persisted.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  await waitFor('configure: harrier in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    for (const item of items) {
      if ((await item.textContent()).includes(testAgentProfile)) { await item.click(); return true; }
    }
    return false;
  });
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])', { timeout: 5000 });
  // loadConfigure is async — wait for it to populate the model field before
  // filling, otherwise the haiku default max_turns=24 overwrites our value.
  await waitForConfigureLoaded(page);
  await page.fill('#cfg-model', 'claude-haiku-4-5-20251001');
  await page.fill('#cfg-turns', '7');
  await page.fill('#cfg-include', '#');
  await page.fill('#cfg-exclude', 'notes');
  await page.click('#cfg-save');
  await waitFor('configure: saved note visible', async () => {
    return /saved/i.test(await page.$eval('#cfg-note', (el) => el.textContent));
  }, 5000);
  // Reload and navigate back to configure.
  await page.reload();
  await page.waitForSelector('#nav-agents .nav-item');
  await waitFor('configure reload: harrier in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    for (const item of items) {
      if ((await item.textContent()).includes(testAgentProfile)) { await item.click(); return true; }
    }
    return false;
  });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])');
  // Must wait for the async loadConfigure to finish before reading values.
  await waitForConfigureLoaded(page);
  const model = await page.$eval('#cfg-model', (el) => el.value);
  const turns = await page.$eval('#cfg-turns', (el) => el.value);
  const include = await page.$eval('#cfg-include', (el) => el.value);
  const exclude = await page.$eval('#cfg-exclude', (el) => el.value);
  model.includes('haiku') ? ok('configure reload: model persisted') : fail(`configure reload: model wrong: "${model}"`);
  turns === '7' ? ok('configure reload: max_turns persisted') : fail(`configure reload: turns wrong: "${turns}"`);
  include.includes('#') ? ok('configure reload: skills.include persisted') : fail(`configure reload: include wrong: "${include}"`);
  exclude.includes('notes') ? ok('configure reload: skills.exclude persisted') : fail(`configure reload: exclude wrong: "${exclude}"`);
  await page.close();
}

// ── flow 4: rename ────────────────────────────────────────────────────────────
// Change the agent field, save, assert nav updates and selection follows.
const renamedAgent = 'falcon';
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  await waitFor('rename: harrier in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    for (const item of items) {
      if ((await item.textContent()).includes(testAgentProfile)) { await item.click(); return true; }
    }
    return false;
  });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])');
  await waitForConfigureLoaded(page);
  // Rename the agent field.
  await page.fill('#cfg-agent', renamedAgent);
  // cfg-save handler sets note to 'saving…', sends the API call, then on
  // success sets note to 'saved — applies on the next run' BEFORE calling
  // selectAgent (which clears the note). Assert on the API success which
  // means the profile was written — the note timing is fragile.
  const renameResp = await page.evaluate(async (name) => {
    const r = await fetch('/api/admin/agents/set', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'harrier', set: { agent: name } }),
    });
    return r.json();
  }, renamedAgent);
  renameResp.ok ? ok('rename: API accepted the rename') : fail(`rename: API failed: ${JSON.stringify(renameResp)}`);
  // Reload nav so the renamed agent shows.
  await page.reload();
  await page.waitForSelector('#nav-agents .nav-item');
  await waitFor('rename: new name in nav', async () => {
    const items = await page.$$eval('#nav-agents .nav-item', (els) => els.map((e) => e.textContent));
    return items.some((t) => t.includes(renamedAgent));
  });
  await page.close();
}

// ── flow 5: kits & review ────────────────────────────────────────────────────
// Catalog lists seeded kits, readme expands, stage → pending fills,
// approve → queue drains.
{
  const page = await newPage();
  await page.goto('/');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await waitFor('kits: catalog visible', async () => {
    return /dev|core|funnel/.test(await page.$eval('#setup-kits', (el) => el.textContent));
  }, 10000);
  // Expand the dev kit readme.
  await waitFor('kits: dev kit readme button', async () => {
    for (const row of await page.$$('.setup-kit')) {
      const name = await row.$eval('.setup-kit-name', (el) => el.textContent).catch(() => '');
      if (!name.includes('dev')) continue;
      const btn = await row.$('button.ghost');
      if (btn) { await btn.click(); return true; }
    }
    return false;
  });
  await waitFor('kits: readme expands', async () => {
    for (const pre of await page.$$('.setup-readme')) {
      if (!(await pre.evaluate((el) => el.hidden)) && (await pre.textContent()).length > 10) return true;
    }
    return false;
  }, 8000);
  // Stage the dev kit by clicking the stage button in the dev kit row.
  // The page reloads (#setup-kits) after staging, so capture the button
  // text we want before clicking and don't hold a reference past the click.
  let stageClicked = false;
  for (const row of await page.$$('.setup-kit')) {
    const name = await row.$eval('.setup-kit-name', (el) => el.textContent).catch(() => '');
    if (!name.includes('dev')) continue;
    for (const btn of await row.$$('button:not(.ghost)')) {
      if (/\badd\b/i.test(await btn.textContent())) {
        await btn.click();
        stageClicked = true;
        break;
      }
    }
    break;
  }
  stageClicked ? ok('kits: staged dev kit') : fail('kits: stage button not found');
  // The click handler calls loadSetup() on success, which re-renders.
  await waitFor('kits: pending queue populated', async () => {
    return /git-protect|approve/i.test(await page.$eval('#setup-pending', (el) => el.textContent));
  }, 10000);
  ok('kits: pending queue shows staged requests');
  // Approve — use page.click() rather than holding an element ref across
  // async boundaries; loadSetup re-renders the DOM so any captured handle
  // goes stale. page.click finds a fresh element at call time.
  // Approve all pending packages (the dev kit may stage multiple: git-protect,
  // recent-history, window). Each click triggers loadSetup() which re-renders.
  let firstApproved = false;
  await waitFor('kits: approve all pending packages', async () => {
    const btn = await page.$('#setup-pending button');
    if (!btn) {
      // No more approve buttons — check if it's the "at rest" state.
      const text = await page.$eval('#setup-pending', (el) => el.textContent);
      return /nothing to confirm|all set/i.test(text);
    }
    firstApproved = true;
    await page.click('#setup-pending button');
    // Brief pause for loadSetup() to start re-rendering before we re-query.
    await sleep(500);
    return false; // keep looping until all approved
  }, 30000);
  if (firstApproved) ok('kits: all pending packages approved, queue at rest');
  else fail('kits: no approve buttons found after staging');
  await page.close();
}

// ── flow 6: converse round trip ───────────────────────────────────────────────
// Type into #compose-input, submit, message appears in the feed.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  // Select whichever agent is visible (falcon after rename, or main).
  await waitFor('converse: agent in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    if (items.length) { await items[0].click(); return true; }
    return false;
  });
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  const msg = `hello-${Date.now()}`;
  await page.fill('#compose-input', msg);
  await page.click('#compose-send');
  // The app optimistically inserts the message into convMsg before the
  // MQTT echo arrives, so the feed should update immediately.
  await waitFor('converse: message in feed', async () => {
    return (await page.$eval('#conv-holder', (el) => el.textContent)).includes(msg);
  }, 8000);
  // A labeled failure (harness emits these when an agent run breaks) renders
  // as an explicit error bubble in the thread, not silence. Inject one with
  // the correlation of the message we just sent so it threads here.
  const corr = await page.$eval('#conv-holder .msg.you', (el) => (el.title || '').replace('correlation ', '')).catch(() => '');
  const agentName = await page.$eval('#nav-agents .nav-item.on', (el) => el.textContent.trim().replace(/^⟁\s*/, '').replace(/·live$/, '').trim()).catch(() => 'main');
  elanus('emit', `in/human/owner`, '--correlation', corr || 'spec-fail', '--payload',
    JSON.stringify({ failed: true, error: 'spec-injected failure', agent: agentName }));
  await waitFor('converse: agent failure renders as an error bubble', async () => {
    return (await page.$eval('#conv-holder', (el) => el.textContent)).includes('spec-injected failure');
  }, 8000);
  await page.close();
}

// ── flow 7: history works out of the box ──────────────────────────────────────
// history is a stdlib package, approved at init, so the sessions tab is backed
// by the real transcript view — NOT the "history package not running" note —
// and the footer degradation hint stays hidden.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  const firstAgent = await page.$('#nav-agents .nav-item');
  if (firstAgent) await firstAgent.click();
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  await page.click('[data-tab="sessions"]');
  await page.waitForSelector('#view-sessions:not([hidden])', { timeout: 5000 });
  // Resolves to a real (possibly empty) session list, not the live-only note.
  await waitFor('history: sessions tab backed by the live view (not degraded)', async () => {
    const t = await page.$eval('#sessions-pane', (el) => el.textContent);
    return !/history package not running|live view only|asking the history view/i.test(t);
  }, 12000);
  // The footer hint shows only when history is absent; it heals to hidden once
  // a query succeeds (the sessions probe above is one).
  await waitFor('history: footer degradation hint hidden (history is on)', async () => {
    return await page.$eval('#history-hint', (el) => el.hidden);
  }, 12000);
  await page.close();
}

// ── page errors check ─────────────────────────────────────────────────────────
if (pageErrors.length) {
  fail(`${pageErrors.length} JS page error(s) across all views:\n${pageErrors.join('\n')}`);
} else {
  ok('no page errors across all views');
}
if (consoleErrors.length) {
  fail(`${consoleErrors.length} console.error(s) across all views:\n${consoleErrors.join('\n')}`);
} else {
  ok('no unexpected console errors across all views');
}

// -- teardown --
await browser.close();
server.kill('SIGKILL');
daemon.kill('SIGKILL');
try { execFileSync('pkill', ['-9', '-f', TMP]); } catch {}
console.log(failures === 0 ? 'ALL PASS' : `${failures} failure(s)`);
process.exit(failures === 0 ? 0 : 1);

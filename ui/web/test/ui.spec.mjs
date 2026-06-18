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
function createConfigProposal(id, pkg, toml) {
  const cfg = path.join(TMP, 'config');
  const branch = `tmp-${id}`;
  execFileSync('git', ['-C', cfg, 'checkout', '-q', '-b', branch, 'live'], { encoding: 'utf8' });
  fs.writeFileSync(path.join(cfg, 'packages', `${pkg}.toml`), toml);
  execFileSync('git', ['-C', cfg, 'add', `packages/${pkg}.toml`], { encoding: 'utf8' });
  execFileSync('git', ['-C', cfg, '-c', 'user.name=qa', '-c', 'user.email=qa@local', 'commit', '-q', '-m', `proposal ${id}`], { encoding: 'utf8' });
  const sha = execFileSync('git', ['-C', cfg, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  execFileSync('git', ['-C', cfg, 'update-ref', `refs/proposals/${id}`, sha], { encoding: 'utf8' });
  execFileSync('git', ['-C', cfg, 'checkout', '-q', 'live'], { encoding: 'utf8' });
  execFileSync('git', ['-C', cfg, 'branch', '-D', branch], { encoding: 'utf8' });
}

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
// Set model, max run steps 7, include '#', exclude 'notes', save, assert note,
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
  await page.fill('#cfg-context-program', 'default');
  await page.fill('#cfg-context-max-ms', '12000');
  await waitFor('configure: run budget label is not conversation turns', async () => {
    const text = await page.$eval('#cfg-section-model', (el) => el.textContent);
    return /max run steps/i.test(text)
      && /activation's model\/tool loop/i.test(text)
      && !/max turns/i.test(text);
  }, 5000);
  await waitFor('configure: context program is first-class agent config', async () => {
    const text = await page.$eval('#cfg-section-context', (el) => el.textContent);
    return /context program/i.test(text)
      && /max context ms/i.test(text)
      && /context stage chain/i.test(text)
      && /raw TOML stores this as the context\.stage array/i.test(text);
  }, 5000);
  const windowContextStage = () => page.locator('#cfg-context-chain .cfg-context-stage[data-stage="window/window"]').first();
  await waitFor('configure: context stage chain renders as tiles', async () => {
    const text = await page.$eval('#cfg-context-chain', (el) => el.textContent);
    return /window\/window/.test(text) && /timeout ms/i.test(text);
  }, 8000);
  await windowContextStage().locator('button[aria-label="remove window/window"]').click();
  await waitFor('configure: context stage remove updates add menu', async () => {
    const chainText = await page.$eval('#cfg-context-chain', (el) => el.textContent);
    const option = await page.$eval('#cfg-context-add-stage', (el) => el.value).catch(() => '');
    return !/window\/window/.test(chainText) && option === 'window/window';
  }, 5000);
  await page.click('#cfg-context-add');
  await waitFor('configure: context stage add restores tile', async () => {
    return /window\/window/.test(await page.$eval('#cfg-context-chain', (el) => el.textContent));
  }, 5000);
  await windowContextStage().locator('label', { hasText: 'timeout ms' }).locator('input').fill('9000');
  await waitFor('configure: context stage declared setting renders in tile', async () => {
    const text = await windowContextStage().textContent();
    return /Window rows/.test(text)
      && /context stage window/.test(text)
      && /type:\s*number/.test(text);
  }, 5000);
  await windowContextStage()
    .locator('.cfg-config-row', { hasText: 'Window rows' })
    .locator('input[type="number"]')
    .first()
    .fill('60');
  const moveDown = windowContextStage().locator('button[aria-label="move window/window down"]').first();
  if (await moveDown.count() && !(await moveDown.isDisabled())) {
    await moveDown.click();
    ok('configure: context stage reorder control is usable');
  }
  await page.$eval('#cfg-include', (el) => el.value = '#');
  await waitFor('configure: package tree shows matched packages', async () => {
    return /history|harness-doctrine|self-modify/.test(await page.$eval('#cfg-package-configs', (el) => el.textContent));
  }, 8000);
  await waitFor('configure: only the packages section renders skill/package controls', async () => {
    const hasSkillsSection = await page.$('#cfg-section-skills');
    const indexText = await page.$eval('.cfg-index', (el) => el.textContent);
    return !hasSkillsSection && !/\bskills\b/i.test(indexText);
  }, 5000);
  await waitFor('configure: vars are raw advanced context parameters', async () => {
    const hasVarsSection = await page.$('#cfg-section-vars');
    const indexText = await page.$eval('.cfg-index', (el) => el.textContent);
    const rawText = await page.$eval('#cfg-section-raw', (el) => el.textContent);
    return !hasVarsSection
      && !/\bvars\b/i.test(indexText)
      && /advanced context parameters/i.test(rawText)
      && /legacy\s+\[vars\]/i.test(rawText);
  }, 5000);
  await page.click('text=advanced context parameters');
  await page.fill('#cfg-vars .cfg-var-key', 'window_rows');
  await page.fill('#cfg-vars .cfg-var-value', '50');
  await waitFor('configure: packages render as kit groups and package rows', async () => {
    const text = await page.$eval('#cfg-package-configs', (el) => el.textContent);
    const sections = await page.$$eval('#cfg-package-configs > details.cfg-package-group > summary', (els) => els.map((el) => el.textContent?.trim().toLowerCase()));
    return /chat/.test(text)
      && !sections.includes('core')
      && !sections.includes('local')
      && sections.some((s) => /stdlib|instance/.test(s))
      && !/current settings|TOML value/.test(text);
  }, 8000);
  const windowPackage = () => page.locator('#cfg-package-configs .cfg-package-card[data-package="window"]').first();
  await waitFor('configure: window context-stage package row exists', async () => {
    return await windowPackage().count() > 0;
  }, 8000);
  await windowPackage().locator('summary').click();
  await windowPackage().locator('.cfg-package-config-toggle').click();
  await waitFor('configure: typed context-stage setting renders from manifest', async () => {
    const text = await windowPackage().textContent();
    return /Window rows/.test(text)
      && /context stage window/.test(text)
      && /type:\s*number/.test(text)
      && /Maximum transcript rows/.test(text);
  }, 8000);
  const windowRows = await windowPackage()
    .locator('.cfg-config-row', { hasText: 'Window rows' })
    .locator('input[type="number"]')
    .first()
    .inputValue();
  windowRows === '80'
    ? ok('configure: typed context-stage default value rendered')
    : fail(`configure: window_rows default wrong: "${windowRows}"`);
  await waitFor('configure: skill filters are not text boxes', async () => {
    const types = await page.$$eval('#cfg-include, #cfg-exclude', (els) => els.map((el) => el.type));
    return types.every((t) => t === 'hidden');
  }, 5000);
  await page.click('#cfg-kit-add-toggle');
  await waitFor('configure: kit add modal opens', async () => {
    return await page.$eval('#cfg-kit-add-modal', (el) => el.open);
  }, 5000);
  await waitFor('configure: kit add list loads in modal', async () => {
    return /core|dev|stdlib/.test(await page.$eval('#cfg-kit-add-list', (el) => el.textContent));
  }, 8000);
  const devKit = page.locator('#cfg-kit-add-list details', { hasText: 'dev' }).first();
  await devKit.locator('summary').click();
  await waitFor('configure: kit modal previews package actors', async () => {
    const text = await devKit.textContent();
    return /git-protect/.test(text) && /hook/.test(text);
  }, 8000);
  await page.click('#cfg-kit-add-list .cfg-split-caret');
  await waitFor('configure: kit add split menu exposes actions', async () => {
    const hasNativeModeSelect = await page.$('#cfg-kit-add-list select');
    const menuText = await page.$eval('#cfg-kit-add-list .cfg-action-menu:not([hidden])', (el) => el.textContent).catch(() => '');
    return !hasNativeModeSelect && /link/.test(menuText) && /copy/.test(menuText);
  }, 5000);
  await page.click('#cfg-kit-add-close');
  const historyPackage = () => page.locator('#cfg-package-configs .cfg-package-card[data-package="history"]').first();
  await historyPackage().locator('summary').click();
  const packageToggle = () => historyPackage().locator('.cfg-package-disable').first();
  if (await packageToggle().count()) {
    await packageToggle().click();
    await waitFor('configure: disable package writes exclude', async () => {
      return (await page.$eval('#cfg-exclude', (el) => el.value)).split(',').map((s) => s.trim()).includes('history');
    }, 5000);
    await historyPackage().locator('summary').click();
    await packageToggle().click();
    await waitFor('configure: enable package removes exclude', async () => {
      return !(await page.$eval('#cfg-exclude', (el) => el.value)).split(',').map((s) => s.trim()).includes('history');
    }, 5000);
  } else {
    fail('configure: history package toggle not found');
  }
  const historyKit = page.locator('#cfg-package-configs details.cfg-package-group', { has: page.locator('.cfg-package-card[data-package="history"]') }).first();
  const kitToggle = historyKit.locator('.cfg-kit-toggle[aria-label^="disable all"]');
  if (await kitToggle.count()) {
    await kitToggle.click();
    await waitFor('configure: disable package group writes excludes', async () => {
      const excluded = (await page.$eval('#cfg-exclude', (el) => el.value)).split(',').map((s) => s.trim());
      return excluded.includes('history');
    }, 5000);
    await historyKit.locator('.cfg-kit-toggle[aria-label^="enable all"]').click();
    await waitFor('configure: enable package group removes excludes', async () => {
      const excluded = (await page.$eval('#cfg-exclude', (el) => el.value)).split(',').map((s) => s.trim());
      return !excluded.includes('history');
    }, 5000);
  } else {
    fail('configure: package group toggle not found');
  }
  await historyPackage().locator('summary').click();
  await packageToggle().click();
  await waitFor('configure: disable package persists through save', async () => {
    return (await page.$eval('#cfg-exclude', (el) => el.value)).split(',').map((s) => s.trim()).includes('history');
  }, 5000);
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
  const contextProgram = await page.$eval('#cfg-context-program', (el) => el.value);
  const contextMaxMs = await page.$eval('#cfg-context-max-ms', (el) => el.value);
  const contextWindowTimeout = await page.locator('#cfg-context-chain .cfg-context-stage[data-stage="window/window"]')
    .locator('label', { hasText: 'timeout ms' })
    .locator('input')
    .first()
    .inputValue();
  const include = await page.$eval('#cfg-include', (el) => el.value);
  const exclude = await page.$eval('#cfg-exclude', (el) => el.value);
  await page.click('text=advanced context parameters');
  const varKey = await page.$eval('#cfg-vars .cfg-var-key', (el) => el.value);
  const varValue = await page.$eval('#cfg-vars .cfg-var-value', (el) => el.value);
  const rawToml = await page.$eval('#cfg-toml', (el) => el.value);
  model.includes('haiku') ? ok('configure reload: model persisted') : fail(`configure reload: model wrong: "${model}"`);
  turns === '7' ? ok('configure reload: max_turns persisted') : fail(`configure reload: turns wrong: "${turns}"`);
  contextProgram === 'default' && contextMaxMs === '12000'
    ? ok('configure reload: context program policy persisted')
    : fail(`configure reload: context policy wrong: "${contextProgram}" ${contextMaxMs}`);
  contextWindowTimeout === '9000'
    ? ok('configure reload: context stage timeout persisted')
    : fail(`configure reload: context stage timeout wrong: "${contextWindowTimeout}"`);
  include.includes('#') ? ok('configure reload: skills.include persisted') : fail(`configure reload: include wrong: "${include}"`);
  exclude.includes('history') ? ok('configure reload: skills.exclude persisted') : fail(`configure reload: exclude wrong: "${exclude}"`);
  varKey === 'window_rows' && varValue === '60'
    ? ok('configure reload: advanced context parameter persisted')
    : fail(`configure reload: context parameter wrong: "${varKey}"="${varValue}"`);
  /\[vars\][\s\S]*window_rows\s*=\s*"60"/.test(rawToml)
    ? ok('configure reload: raw [vars] persisted')
    : fail('configure reload: raw [vars] missing window_rows');
  /\[context\][\s\S]*max_total_ms\s*=\s*12000/.test(rawToml)
    ? ok('configure reload: raw [context] persisted')
    : fail('configure reload: raw [context] missing max_total_ms');
  /\[context\][\s\S]*stage\s*=\s*\[[\s\S]*package\s*=\s*"window"[\s\S]*timeout_ms\s*=\s*9000/.test(rawToml)
    ? ok('configure reload: raw context.stage array persisted')
    : fail('configure reload: raw context.stage array missing window timeout');
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

// ── flow 5: add-ons ───────────────────────────────────────────────────────────
// Catalog lists seeded add-ons, details expand, add takes effect, settings save.
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
  // Add the dev kit by clicking the add button in the dev row.
  // The page reloads (#setup-kits) after add, so capture the button
  // text we want before clicking and don't hold a reference past the click.
  let addClicked = false;
  for (const row of await page.$$('.setup-kit')) {
    const name = await row.$eval('.setup-kit-name', (el) => el.textContent).catch(() => '');
    if (!name.includes('dev')) continue;
    for (const btn of await row.$$('button:not(.ghost)')) {
      if (/\badd\b/i.test(await btn.textContent())) {
        await btn.click();
        addClicked = true;
        break;
      }
    }
    break;
  }
  addClicked ? ok('add-ons: added dev kit') : fail('add-ons: add button not found');
  await waitFor('add-ons: durable add banner', async () => {
    return /added dev/i.test(await page.$eval('#setup-status', (el) => el.textContent));
  }, 10000);
  await sleep(1900);
  /added dev/i.test(await page.$eval('#setup-status', (el) => el.textContent))
    ? ok('add-ons: add banner persists')
    : fail('add-ons: add banner vanished');
  await waitFor('add-ons: installed list includes git-protect', async () => {
    return /git-protect/.test(await page.$eval('#setup-configs', (el) => el.textContent));
  }, 10000);
  let saved = false;
  let savedCard = null;
  for (const card of await page.$$('#setup-configs .setup-pending-pkg')) {
    const text = await card.textContent();
    if (!/git-protect/.test(text)) continue;
    const inputs = await card.$$('input');
    if (inputs.length >= 2) {
      await inputs[0].fill('mode');
      await inputs[1].fill('"watch"');
      const btn = await card.$('button');
      if (btn) {
        await btn.click();
        saved = true;
        savedCard = card;
      }
    }
    break;
  }
  saved ? ok('add-ons: package setting saved from UI') : fail('add-ons: setting form not found');
  await waitFor('add-ons: package setting save confirmed by readback', async () => {
    if (!savedCard) return false;
    const text = await savedCard.textContent();
    return /saved/.test(text);
  }, 10000);
  await savedCard.$eval('details', (el) => { if (!el.open) el.querySelector('summary')?.click(); });
  await waitFor('add-ons: package setting visible in current settings', async () => {
    return /mode = "watch"/.test(await savedCard.$eval('pre', (el) => el.textContent).catch(() => ''));
  }, 10000);
  await waitFor('add-ons: agent requests resting state', async () => {
    return /no agent requests/i.test(await page.$eval('#setup-pending', (el) => el.textContent));
  }, 10000);
  createConfigProposal('webui', 'git-protect', 'mode = "browser-proposed"\n');
  await page.click('.nav-setup');
  await waitFor('add-ons: proposal request appears', async () => {
    return /wants to change settings/i.test(await page.$eval('#setup-pending', (el) => el.textContent));
  }, 10000);
  await page.click('#setup-pending button.ghost');
  await waitFor('add-ons: proposal diff visible', async () => {
    return /browser-proposed/.test(await page.$eval('#setup-pending pre', (el) => el.textContent).catch(() => ''));
  }, 10000);
  const buttons = await page.$$('#setup-pending button');
  for (const btn of buttons) {
    if (/^accept$/i.test(await btn.textContent())) { await btn.click(); break; }
  }
  await waitFor('add-ons: proposal accepted through UI', async () => {
    return /accepted the change/i.test(await page.$eval('#setup-status', (el) => el.textContent));
  }, 10000);
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
  const publishes = [];
  page.on('request', (req) => {
    if (!req.url().endsWith('/api/publish') || req.method() !== 'POST') return;
    try { publishes.push(JSON.parse(req.postData() || '{}')); } catch {}
  });
  const msg = `hello-${Date.now()}`;
  await page.fill('#compose-input', msg);
  await page.click('#compose-send');
  // The app optimistically inserts the message into convMsg before the
  // MQTT echo arrives, so the feed should update immediately.
  await waitFor('converse: message in feed', async () => {
    return (await page.$eval('#conv-holder', (el) => el.textContent)).includes(msg);
  }, 8000);
  const msg2 = `${msg}-again`;
  await page.fill('#compose-input', msg2);
  await page.click('#compose-send');
  await waitFor('converse: second message in same visible feed', async () => {
    const t = await page.$eval('#conv-holder', (el) => el.textContent);
    return t.includes(msg) && t.includes(msg2);
  }, 8000);
  await waitFor('converse: browser publishes one stable session id', async () => {
    if (publishes.length < 2) return false;
    const [a, b] = publishes;
    return a.payload?.session
      && a.payload.session === b.payload?.session
      && a.correlation
      && b.correlation
      && a.correlation !== b.correlation;
  }, 5000);
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
// history is stdlib, on at init, so the sessions tab is backed
// by the real transcript view — NOT the unavailable-transcripts note —
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
    return !/transcripts unavailable|live view only|asking the history view/i.test(t);
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

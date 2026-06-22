// Browser e2e: headless Chromium against a live stack (daemon + web server).
// Same stack pattern as smoke.mjs — throwaway root, unique ports from pid.
// Page errors and console errors are test failures; that is the main value
// of this layer over the HTTP smoke.
// Catalog of record: docs/ui-flows/configuration.md.
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
const WEB_LOG = path.join(TMP, 'web.log');
const ENV = { ...process.env, ELANUS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}`, ELANUS_WEB_LOG: WEB_LOG };

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
  // Contrast baseline (M1): every text token must clear WCAG AA 4.5:1 against
  // the page bg, the panel bg, and the active-row bg. Locking the values here
  // catches a regression that pulls --dim or --meta back below the floor.
  const contrast = await page.evaluate(() => {
    const css = getComputedStyle(document.documentElement);
    const hex = (v) => css.getPropertyValue(v).trim();
    const tok = { bg: hex('--bg'), panel: hex('--panel'), active: '#1b1d18', ink: hex('--ink'), dim: hex('--dim'), meta: hex('--meta') };
    const lin = (ch) => { const c = ch / 255; return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
    const lum = (h) => { const r = parseInt(h.slice(1, 3), 16), g = parseInt(h.slice(3, 5), 16), b = parseInt(h.slice(5, 7), 16); return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b); };
    const ratio = (a, b) => { const l1 = lum(a), l2 = lum(b); const hi = Math.max(l1, l2), lo = Math.min(l1, l2); return (hi + 0.05) / (lo + 0.05); };
    const min = (fg) => Math.min(ratio(fg, tok.bg), ratio(fg, tok.panel), ratio(fg, tok.active));
    return { inkAA: min(tok.ink) >= 4.5, dimAA: min(tok.dim) >= 4.5, metaAA: min(tok.meta) >= 4.5, ratios: { ink: min(tok.ink), dim: min(tok.dim), meta: min(tok.meta) } };
  });
  contrast.inkAA ? ok(`contrast: --ink clears AA (${contrast.ratios.ink.toFixed(2)})`) : fail(`contrast: --ink below AA (${contrast.ratios.ink.toFixed(2)})`);
  contrast.dimAA ? ok(`contrast: --dim clears AA (${contrast.ratios.dim.toFixed(2)})`) : fail(`contrast: --dim below AA (${contrast.ratios.dim.toFixed(2)})`);
  contrast.metaAA ? ok(`contrast: --meta clears AA (${contrast.ratios.meta.toFixed(2)})`) : fail(`contrast: --meta below AA (${contrast.ratios.meta.toFixed(2)})`);
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
  await waitFor('setup: health home shows root, credential, broker and history', async () => {
    const text = await page.$eval('.setup-home', (el) => el.textContent);
    return /root/i.test(text) && /owner credential/i.test(text) && /broker/i.test(text) && /history/i.test(text);
  }, 8000);
  await waitFor('setup: guided agent wizard exposes purpose, workdir, cost cap and autonomy', async () => {
    const text = await page.$eval('.setup-wizard', (el) => el.textContent);
    return /purpose/i.test(text) && /home \/ workdir/i.test(text) && /run-step cap/i.test(text) && /autonomy/i.test(text);
  }, 8000);
  await waitFor('setup: cost visibility separates hard caps from estimates', async () => {
    const text = await page.$eval('.setup-cost', (el) => el.textContent);
    return /cost visibility/i.test(text) && /hard activation limits/i.test(text) && /fake precision/i.test(text);
  }, 8000);
  await waitFor('setup: trust footprint exposes local root/data surface', async () => {
    const text = await page.$eval('.setup-trust', (el) => el.textContent);
    return /active principal/i.test(text) && /web relay/i.test(text) && /database/i.test(text) && /config repo/i.test(text);
  }, 8000);
  // M3: Create is disabled until a name is entered.
  const createDisabledAtStart = await page.$eval('#na-create', (el) => el.disabled);
  createDisabledAtStart ? ok('wizard: Create disabled on empty name') : fail('wizard: Create enabled before name entered');
  // M3: the workdir field flags a path that does not exist (closed-set-style
  // validation per ui-preferences.md — let the field catch a typo before save).
  await page.fill('#na-workdir', '/tmp/elanus-definitely-does-not-exist-xyz');
  await page.$eval('#na-workdir', (el) => el.blur());
  await waitFor('wizard: bogus workdir is flagged at the field', async () => {
    const text = await page.$eval('.wizard-grid', (el) => el.textContent);
    return /does not exist/i.test(text);
  }, 5000);
  await page.fill('#na-workdir', '');
  // M3: model field shows the provider-unavailable state at the field when no
  // list is loaded (no API key in the spec env), not silent free text.
  await waitFor('wizard: unavailable provider is signaled at the model field', async () => {
    const text = await page.$eval('.wizard-grid', (el) => el.textContent);
    return /provider list unavailable/i.test(text);
  }, 5000);
  await page.fill('#na-name', testAgentProfile);
  await page.fill('#na-purpose', 'qa regression agent');
  await page.fill('#na-model', 'claude-haiku-4-5-20251001');
  await page.fill('#na-turns', '11');
  const createEnabledAfterName = await page.$eval('#na-create', (el) => !el.disabled);
  createEnabledAfterName ? ok('wizard: Create enabled after name entered') : fail('wizard: Create still disabled after name entered');
  await page.click('#na-create');
  await waitFor('new agent: converse tab opens', async () => {
    return !(await page.$eval('#view-converse', (el) => el.hidden));
  }, 10000);
  const composeLabel = await page.$eval('#compose-input', (el) => el.getAttribute('aria-label'));
  composeLabel === `message ${testAgentProfile}` ? ok('new agent: compose targets the new agent') : fail(`new agent: compose target wrong (${composeLabel})`);
  await waitFor('new agent: configure pointer is visible from converse', async () => {
    const text = await page.$eval('#conv-configure-hint', (el) => el.textContent).catch(() => '');
    return text.includes(`Tune ${testAgentProfile} anytime in configure`);
  });
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
  await waitFor('configure: essentials are first and plumbing is advanced', async () => {
    const essentials = await page.$eval('#cfg-section-essentials', (el) => el.textContent);
    return /essentials/i.test(essentials)
      && /name/i.test(essentials)
      && /model/i.test(essentials)
      && /max run steps/i.test(essentials)
      && /autonomy/i.test(essentials)
      && /working directory/i.test(essentials)
      && !/parent|prepend path|effective path/i.test(essentials);
  }, 5000);
  await waitFor('configure: essentials reflect the agent (model, cap, autonomy, hint)', async () => {
    const text = await page.$eval('#cfg-section-essentials', (el) => el.textContent);
    const turns = await page.$eval('#cfg-turns', (el) => el.value);
    return /claude-haiku-4-5-20251001/.test(text)
      && turns === '7'
      && /hard ceiling for one activation/i.test(text)
      && /cost\/performance:\s*cheap/i.test(text);
  }, 5000);
  // M6: autonomy consequence renders exactly once in the essentials screenful
  // (was duplicated as a separate <p> + the cost-card em).
  await waitFor('configure: autonomy consequence appears exactly once', async () => {
    const text = await page.$eval('#cfg-section-essentials', (el) => el.textContent);
    const m = text.match(/This agent cannot accept its own setting changes/gi);
    return m && m.length === 1;
  }, 5000);
  const beforeAutonomy = await page.$eval('#cfg-autonomy-consequence', (el) => el.textContent);
  await page.selectOption('#cfg-autonomy', 'assisted');
  await waitFor('configure: autonomy consequence updates', async () => {
    const text = await page.$eval('#cfg-autonomy-consequence', (el) => el.textContent);
    return text !== beforeAutonomy && /low-risk agent setting changes/i.test(text);
  }, 5000);
  await page.selectOption('#cfg-autonomy', 'off');
  await page.click('#cfg-section-advanced > summary');
  await page.fill('#cfg-context-program', 'default');
  await page.fill('#cfg-context-max-ms', '12000');
  await waitFor('configure: run budget label is not conversation turns', async () => {
    const text = await page.$eval('#cfg-section-essentials', (el) => el.textContent);
    return /max run steps/i.test(text)
      && /activation's model\/tool loop/i.test(text)
      && !/max turns/i.test(text);
  }, 5000);
  await waitFor('configure: context program is first-class agent config', async () => {
    const text = await page.$eval('#cfg-section-context', (el) => el.textContent);
    return /context program/i.test(text)
      && /max context ms/i.test(text)
      && /context steps/i.test(text)
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
      && /agent context window/.test(text)
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
      && /Advanced values for context and templates/i.test(rawText);
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
  await waitFor('configure: package row declares setting scope before expansion', async () => {
    const text = await windowPackage().locator('summary').textContent();
    return /settings can be saved for every agent or for harrier only/i.test(text);
  }, 5000);
  await windowPackage().locator('summary').click();
  await windowPackage().locator('.cfg-package-config-toggle').click();
  await waitFor('configure: typed context-stage setting renders from manifest', async () => {
    const text = await windowPackage().textContent();
    return /Window rows/.test(text)
      && /agent context window/.test(text)
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
  await waitFor('configure: package setting declares shared scope and effective source', async () => {
    const text = await windowPackage().locator('.cfg-config-row', { hasText: 'Window rows' }).textContent();
    return /shared default for every agent/i.test(text)
      && /effective here:\s*80/i.test(text)
      && /from the package default/i.test(text);
  }, 5000);
  const windowRowsRow = windowPackage().locator('.cfg-config-row', { hasText: 'Window rows' });
  await windowRowsRow.locator('input[type="number"]').first().fill('70');
  await windowRowsRow.locator('button[aria-label^="save window.window_rows for every agent"]').click();
  await waitFor('configure: shared package setting save is labeled for every agent', async () => {
    const text = await windowRowsRow.textContent();
    return /saved for every agent/i.test(text)
      && /effective here:\s*70/i.test(text)
      && /from the shared default/i.test(text);
  }, 8000);
  await windowRowsRow.locator('input[type="number"]').first().fill('60');
  await windowRowsRow.locator('button[aria-label^="save window.window_rows for harrier"]').click();
  await waitFor('configure: per-agent package setting save is labeled for this agent', async () => {
    const text = await windowRowsRow.textContent();
    return /saved for harrier/i.test(text)
      && /effective here:\s*60/i.test(text)
      && /overridden here for harrier/i.test(text);
  }, 8000);
  await windowRowsRow.locator('input[type="number"]').first().fill('70');
  await windowRowsRow.locator('button[aria-label^="save window.window_rows for every agent"]').click();
  await waitFor('configure: shared save preserves selected agent override source', async () => {
    const text = await windowRowsRow.textContent();
    return /saved for every agent/i.test(text)
      && /effective here:\s*60/i.test(text)
      && /overridden here for harrier/i.test(text);
  }, 8000);
  await waitFor('configure: context tile declares agent-only scope', async () => {
    const text = await windowContextStage().locator('.cfg-config-row', { hasText: 'Window rows' }).textContent();
    return /applies to harrier only/i.test(text);
  }, 5000);
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
  await page.click('#cfg-section-advanced > summary');
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
  const varRows = await page.$$eval('#cfg-vars .cfg-var-row', (rows) => rows.map((row) => ({
    key: row.querySelector('.cfg-var-key')?.value,
    value: row.querySelector('.cfg-var-value')?.value,
  })));
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
  varRows.some((row) => row.key === 'window_rows' && row.value === '60')
    ? ok('configure reload: advanced context parameter persisted')
    : fail(`configure reload: context parameter wrong: ${JSON.stringify(varRows)}`);
  /\[vars\][\s\S]*window_rows\s*=\s*"60"/.test(rawToml)
    ? ok('configure reload: raw [vars] persisted')
    : fail('configure reload: raw [vars] missing window_rows');
  /\[context\][\s\S]*max_total_ms\s*=\s*12000/.test(rawToml)
    ? ok('configure reload: raw [context] persisted')
    : fail('configure reload: raw [context] missing max_total_ms');
  /\[context\][\s\S]*stage\s*=\s*\[[\s\S]*package\s*=\s*"window"[\s\S]*timeout_ms\s*=\s*9000/.test(rawToml)
    ? ok('configure reload: raw context.stage array persisted')
    : fail('configure reload: raw context.stage array missing window timeout');
  const reloadedWindowPackage = () => page.locator('#cfg-package-configs .cfg-package-card[data-package="window"]').first();
  await reloadedWindowPackage().evaluate((el) => { if (!el.open) el.querySelector('summary')?.click(); });
  await reloadedWindowPackage().locator('.cfg-package-config-toggle').click();
  await waitFor('configure reload: selected agent shows its package override source', async () => {
    const text = await reloadedWindowPackage().locator('.cfg-config-row', { hasText: 'Window rows' }).textContent();
    return /effective here:\s*60/i.test(text) && /overridden here for harrier/i.test(text);
  }, 8000);
  await waitFor('configure reload: selected agent context tile shows its override source', async () => {
    const text = await page.locator('#cfg-context-chain .cfg-context-stage[data-stage="window/window"]')
      .locator('.cfg-config-row', { hasText: 'Window rows' })
      .textContent();
    return /effective here:\s*60/i.test(text) && /overridden here for harrier/i.test(text);
  }, 8000);
  const harrierCostSummary = await page.$eval('.cfg-cost-summary', (el) => el.textContent);
  await waitFor('configure reload: second agent in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    for (const item of items) {
      const text = await item.textContent();
      if (/\bmain\b/.test(text)) { await item.click(); return true; }
    }
    return false;
  });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])');
  await waitForConfigureLoaded(page);
  await page.click('#cfg-section-advanced > summary');
  const mainWindowPackage = () => page.locator('#cfg-package-configs .cfg-package-card[data-package="window"]').first();
  await waitFor('configure reload: cost summary follows second agent', async () => {
    const model = await page.$eval('#cfg-model', (el) => el.value);
    const turns = await page.$eval('#cfg-turns', (el) => el.value);
    const autonomy = await page.$eval('#cfg-autonomy', (el) => el.value);
    const text = await page.$eval('#cfg-section-essentials', (el) => el.textContent);
    return text !== harrierCostSummary
      && model !== 'claude-haiku-4-5-20251001'
      && turns !== '7'
      && text.includes(model)
      && text.includes(autonomy);
  }, 5000);
  await mainWindowPackage().evaluate((el) => { if (!el.open) el.querySelector('summary')?.click(); });
  await mainWindowPackage().locator('.cfg-package-config-toggle').click();
  await waitFor('configure reload: second agent sees shared package setting', async () => {
    const text = await mainWindowPackage().locator('.cfg-config-row', { hasText: 'Window rows' }).textContent();
    return /effective here:\s*70/i.test(text) && /from the shared default/i.test(text);
  }, 8000);
  await waitFor('configure reload: second agent context tile sees shared setting', async () => {
    const text = await page.locator('#cfg-context-chain .cfg-context-stage[data-stage="window/window"]')
      .locator('.cfg-config-row', { hasText: 'Window rows' })
      .textContent();
    return /effective here:\s*70/i.test(text) && /from the shared default/i.test(text);
  }, 8000);
  const webLog = fs.existsSync(WEB_LOG) ? fs.readFileSync(WEB_LOG, 'utf8') : '';
  /web:cli.*elanus config set window window_rows 70/.test(webLog)
    ? ok('configure scope: backend log recorded shared config write')
    : fail('configure scope: backend log missing shared config write');
  /web:cli.*elanus profile set harrier .*vars\.window_rows="60"/.test(webLog)
    ? ok('configure scope: backend log recorded per-agent profile write')
    : fail('configure scope: backend log missing per-agent profile write');
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
  await waitFor('setup: cost panel names the agent it describes', async () => {
    const text = await page.$eval('.setup-cost', (el) => el.textContent);
    return /Showing\s+(main|default)/i.test(text);
  }, 10000);
  await waitFor('kits: catalog visible', async () => {
    return /dev|core|funnel/.test(await page.$eval('#setup-kits', (el) => el.textContent));
  }, 10000);
  await waitFor('capabilities: catalog uses outcome language and installed state', async () => {
    const text = await page.$eval('#setup-kits', (el) => el.textContent);
    return /capability|available|installed|useful behavior/i.test(text);
  }, 10000);
  await waitFor('capabilities: coding agents are honest coming-soon catalog entry', async () => {
    const text = await page.$eval('#coding-agent-entry', (el) => el.textContent).catch(() => '');
    return /coming soon/i.test(text) && /Codex|Claude Code/.test(text) && /sandbox/.test(text) && /recorded activity trail|recording/.test(text) && /cost control|spend ceiling/.test(text) && /not configured/i.test(text);
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
  await waitFor('risk: installed capabilities show approval/risk badges', async () => {
    const text = await page.$eval('#setup-configs', (el) => el.textContent);
    return /approved|needs review|hook|daemon|local http|broad publish|low surface/i.test(text);
  }, 10000);
  const savedCard = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'window' }).first();
  await savedCard.locator('button', { hasText: 'settings' }).click();
  await waitFor('add-ons: instance package settings render typed manifest inputs', async () => {
    const text = await savedCard.textContent();
    const count = await savedCard.locator('.cfg-config-row', { hasText: 'Window rows' }).locator('input[type="number"]').count();
    return count > 0 && /applies to every agent/i.test(text) && !/TOML value|using TOML/i.test(text);
  }, 10000);
  const setupWindowRows = savedCard.locator('.cfg-config-row', { hasText: 'Window rows' });
  await setupWindowRows.locator('input[type="number"]').fill('72');
  await setupWindowRows.locator('button[aria-label^="save window.window_rows for every agent"]').click();
  ok('add-ons: package setting saved from typed UI');
  await waitFor('add-ons: package setting save confirmed by readback', async () => {
    return /saved and reloaded/.test(await savedCard.textContent());
  }, 10000);
  await savedCard.locator('details').evaluate((el) => { if (!el.open) el.querySelector('summary')?.click(); });
  await waitFor('add-ons: package setting visible in current settings', async () => {
    return /window_rows = 72/.test(await savedCard.locator('pre').textContent().catch(() => ''));
  }, 10000);
  const copyResp = await page.evaluate(async () => {
    const r = await fetch('/api/admin/kits/add', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ kit: 'core', copy: true }),
    });
    return r.json();
  });
  copyResp.ok ? ok('add-ons: copied core kit via API') : fail(`add-ons: copied core kit failed: ${JSON.stringify(copyResp)}`);
  await page.reload();
  await page.waitForSelector('.nav-setup');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await waitFor('add-ons: copied package explains removal gap', async () => {
    const copied = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'harness-doctrine' }).first();
    const text = await copied.textContent().catch(() => '');
    const turnOffCount = await copied.locator('button', { hasText: 'turn off' }).count().catch(() => 1);
    return /Copied into this installation; removal is not supported here yet/i.test(text) && turnOffCount === 0;
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
  const devPackage = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'git-protect' }).first();
  await devPackage.locator('button', { hasText: 'turn off' }).click();
  await waitFor('add-ons: turn off confirmation explains unlink behavior', async () => {
    const text = await devPackage.textContent();
    return /Turn off dev/i.test(text) && /review record stays/i.test(text);
  }, 5000);
  await devPackage.locator('.setup-confirm button').click();
  await waitFor('add-ons: linked kit turn off is durable', async () => {
    const status = await page.$eval('#setup-status', (el) => el.textContent);
    const list = await page.$eval('#setup-configs', (el) => el.textContent);
    return /turned off dev/i.test(status) && !/git-protect/.test(list);
  }, 10000);
  const webLog = fs.readFileSync(WEB_LOG, 'utf8');
  /elanus kit unlink dev/.test(webLog)
    ? ok('add-ons: backend log recorded kit unlink')
    : fail('add-ons: backend log missing kit unlink');
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
  const stableConversation = publishes[0]?.payload?.session;
  await waitFor('converse: nav shows labeled conversation, not a raw id', async () => {
    const rows = await page.$$eval('#nav-agents .nav-conversation', (els) => els.map((el) => ({
      text: el.textContent || '',
      title: el.getAttribute('title') || '',
    }))).catch(() => []);
    return rows.some((row) => row.text.includes(msg) && row.title === stableConversation && !row.text.includes(stableConversation));
  }, 8000);
  const selectedAgent = await page.$eval('#nav-agents .nav-agent.on', (el) => el.getAttribute('data-sel')?.replace(/^agent:/, '') || 'main').catch(() => 'main');
  await page.reload();
  await page.waitForSelector('#nav-agents .nav-agent');
  await waitFor('converse reload: selected agent returns to converse', async () => {
    const agents = await page.$$('#nav-agents .nav-agent');
    for (const item of agents) {
      if ((await item.getAttribute('data-sel')) === `agent:${selectedAgent}`) { await item.click(); return true; }
    }
    return false;
  }, 8000);
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  const msg3 = `${msg}-after-reload`;
  await page.fill('#compose-input', msg3);
  await page.click('#compose-send');
  await waitFor('converse reload: compose reuses current conversation', async () => {
    return publishes.length >= 3 && publishes[2]?.payload?.session === stableConversation;
  }, 8000);
  await page.click('#conv-new');
  await waitFor('converse: new conversation clears the visible thread', async () => {
    const t = await page.$eval('#conv-holder', (el) => el.textContent);
    return !t.includes(msg) && /start a conversation/i.test(t);
  }, 5000);
  const msg4 = `${msg}-fork`;
  await page.fill('#compose-input', msg4);
  await page.click('#compose-send');
  await waitFor('converse: new conversation publishes a fresh id', async () => {
    return publishes.length >= 4
      && publishes[3]?.payload?.session
      && publishes[3].payload.session !== stableConversation;
  }, 8000);
  // A labeled failure (harness emits these when an agent run breaks) renders
  // as an explicit error bubble in the thread, not silence. Inject one with
  // the correlation of the message we just sent so it threads here.
  const corr = await page.$eval('#conv-holder .msg.you', (el) => (el.title || '').replace('correlation ', '')).catch(() => '');
  const agentName = await page.$eval('#nav-agents .nav-agent.on', (el) => el.getAttribute('data-sel')?.replace(/^agent:/, '') || 'main').catch(() => 'main');
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

// ── flow 8: narrow viewport (sub-900px) ──────────────────────────────────────
// M2: the app must not clip at phone widths. The agent tab strip wraps, the
// sidebar is a drawer (closed by default, expands on click), and the compose
// input + primary actions are reachable without horizontal scroll.
{
  const page = await newPage();
  await page.setViewportSize({ width: 400, height: 800 });
  await page.goto('/');
  // At narrow, the nav drawer is collapsed by default — wait for the toggle
  // (always visible) instead of `#nav-agents` (correctly hidden inside it).
  await page.waitForSelector('#nav-toggle', { timeout: 10000 });
  // Masthead connection indicator is visible (not clipped off-screen).
  await waitFor('narrow: connection indicator visible', async () => {
    const conn = await page.$eval('#conn-text', (el) => el.textContent).catch(() => '');
    return !!conn;
  });
  const noClipAtBoot = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
  noClipAtBoot ? ok('narrow: no horizontal overflow at boot') : fail('narrow: document overflows at boot');
  // Nav drawer is collapsed by default — agent list not visible, toggle is.
  const navListHidden = await page.$eval('#nav-list', (el) => getComputedStyle(el).display === 'none');
  navListHidden ? ok('narrow: nav drawer collapsed by default') : fail('narrow: nav drawer leaked open at narrow');
  const toggleVisible = await page.$eval('#nav-toggle', (el) => getComputedStyle(el).display !== 'none');
  toggleVisible ? ok('narrow: nav toggle visible') : fail('narrow: nav toggle missing');
  // Expand the drawer, pick the first agent, and confirm it closes again.
  await page.click('#nav-toggle');
  await waitFor('narrow: nav drawer expands', async () => {
    return await page.$eval('#nav-list', (el) => getComputedStyle(el).display !== 'none');
  });
  await waitFor('narrow: first agent in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    if (items.length) { await items[0].click(); return true; }
    return false;
  });
  await waitFor('narrow: nav drawer closes after selection', async () => {
    return await page.$eval('#nav-list', (el) => getComputedStyle(el).display === 'none');
  });
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  // Configure tab strip wraps; nothing clips.
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])', { timeout: 5000 });
  await waitForConfigureLoaded(page);
  const noClipAtConfigure = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
  noClipAtConfigure ? ok('narrow: no horizontal overflow on configure') : fail('narrow: configure overflows the viewport');
  // Converse: compose input is reachable without horizontal scroll.
  await page.click('[data-tab="converse"]');
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  const composeReachable = await page.$eval('#compose-input', (el) => {
    const r = el.getBoundingClientRect();
    return r.left >= 0 && r.right <= window.innerWidth && r.width > 40;
  });
  composeReachable ? ok('narrow: compose input reachable inside viewport') : fail('narrow: compose input clipped or too small');
  await page.close();
}

// ── flow 9: a11y baseline ─────────────────────────────────────────────────────
// M4: keyboard focus is visible everywhere; the conversation feed is announced
// to assistive tech; reduced-motion users get no infinite alarm flash.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  // The :focus-visible rule exists with a real outline. (Read cssText, not
  // style.outlineWidth — CSSOM doesn't expand shorthands containing var().)
  const focusRule = await page.evaluate(() => {
    for (const sheet of document.styleSheets) {
      let rules;
      try { rules = sheet.cssRules; } catch { continue; } // cross-origin stylesheet (CDN fonts)
      for (const rule of (rules ?? [])) {
        if (rule.selectorText === ':focus-visible') {
          return { cssText: rule.cssText || '', outline: rule.style.outline || '' };
        }
      }
    }
    return null;
  });
  focusRule && /\boutline\b/.test(focusRule.cssText)
    ? ok(`a11y: global :focus-visible rule present (${focusRule.outline || focusRule.cssText.slice(0, 80)})`)
    : fail(`a11y: no :focus-visible rule (${JSON.stringify(focusRule)})`);
  // Keyboard Tab produces an outlined element on the live page.
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await waitFor('a11y: keyboard focus paints an outline', async () => {
    return await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return false;
      const o = getComputedStyle(el);
      return o.outlineStyle !== 'none' && parseInt(o.outlineWidth, 10) > 0;
    });
  });
  // The conversation feed is a polite live region (so replies are announced).
  await waitFor('a11y: first agent in nav', async () => {
    const items = await page.$$('#nav-agents .nav-item');
    if (items.length) { await items[0].click(); return true; }
    return false;
  });
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  const feedAttrs = await page.$eval('.conv-feed', (el) => ({
    role: el.getAttribute('role'),
    live: el.getAttribute('aria-live'),
  }));
  feedAttrs.role === 'log' && feedAttrs.live === 'polite'
    ? ok('a11y: conversation feed is a polite live region')
    : fail(`a11y: conversation feed attrs wrong: ${JSON.stringify(feedAttrs)}`);
  // The high-volume telemetry feed stays out of the live region.
  await page.click('[data-tab="telemetry"]');
  await page.waitForSelector('#view-rail:not([hidden])', { timeout: 5000 });
  const teleLive = await page.$eval('#tele-feed', (el) => el.getAttribute('aria-live') ?? '(none)');
  teleLive === 'off' ? ok('a11y: telemetry feed is aria-live=off (not announced)') : fail(`a11y: telemetry feed aria-live=${teleLive}`);
  // Tabs are not a misleading role="tablist" without aria-controls.
  const tablistMissing = await page.$eval('#agent-tabs', (el) => el.getAttribute('role') !== 'tablist');
  tablistMissing ? ok('a11y: tab strip is not a half-pattern tablist') : fail('a11y: agent-tabs still claims role=tablist without aria-controls');
  await page.close();
}

// ── flow 10: product language + companion identity (M5) ─────────────────────
// Kernel words (session, raw ids) stay off default surfaces; the agent has a
// stable colored monogram in nav and converse; the cockpit toggle restores
// Tim's vocabulary without re-theming the whole surface.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  // Two visible agents → two distinct identity chip colors.
  await waitFor('identity: at least one agent chip in nav', async () => {
    const chips = await page.$$('#nav-agents .nav-agent .agent-chip');
    return chips.length >= 1;
  });
  // Toggle into cockpit mode: the panel-head noun changes warm → cockpit.
  const warmHead = await page.$eval('.nav .panel-head h2', (el) => el.textContent.trim()).catch(() => '');
  await page.click('#theme-toggle');
  await waitFor('identity: cockpit toggle changes panel-head noun', async () => {
    const h = await page.$eval('.nav .panel-head h2', (el) => el.textContent.trim()).catch(() => '');
    return h && h !== warmHead;
  });
  await page.click('#theme-toggle');
  await waitFor('identity: warm mode restores the warm noun', async () => {
    const h = await page.$eval('.nav .panel-head h2', (el) => el.textContent.trim()).catch(() => '');
    return h === warmHead;
  });
  // Pick the first agent; the warm tab is labeled "history" not "sessions".
  await page.click('#nav-agents .nav-item');
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  const tabText = await page.$$eval('#agent-tabs button', (els) => els.map((e) => e.textContent.trim()));
  const sessionsWordGone = !tabText.some((t) => /^sessions$/i.test(t));
  sessionsWordGone ? ok(`language: "sessions" tab renamed in warm mode (saw: ${tabText.join('|')})`) : fail(`language: tab still says "sessions" (${tabText.join('|')})`);
  // Converse header shows the identity chip alongside the agent name.
  const chipInHeader = await page.$eval('#conv-configure-hint', (el) => el.querySelector('.agent-chip') != null).catch(() => false);
  chipInHeader ? ok('identity: chip rendered in converse header') : fail('identity: converse header missing chip');
  // The compose button says "Send", not "transmit".
  const sendLabel = await page.$eval('#compose-send', (el) => el.textContent.trim()).catch(() => '');
  /^send$/i.test(sendLabel) ? ok('language: compose button is "Send"') : fail(`language: compose button is "${sendLabel}"`);
  await page.close();
}


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

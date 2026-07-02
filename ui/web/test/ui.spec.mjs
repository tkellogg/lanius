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
// The server under test: the Rust `elanus web` (the embedded SPA, src/web.rs).
// server.mjs/config.mjs were retired (web-packaging M4) — the Rust server is the
// only path now.
const server = spawn(path.join(BIN, 'elanus'), ['web', '--port', String(WEB_PORT)], { env: ENV, stdio: ['ignore', 'pipe', 'inherit'] });
// server.mjs is retired (M4): the Rust server is always under test now. Kept as a
// named constant so the Rust-only assertion gates below read intentionally.
const USE_RUST = true;
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
  // Contrast baseline (M1 + M5): every text token must clear WCAG AA 4.5:1
  // against the page bg, the panel bg, and the active-row bg (var(--hover)).
  // Derives `active` from the live token and checks BOTH themes, so it stays
  // correct under light mode rather than assuming the dark active color.
  const contrast = await page.evaluate(() => {
    const root = document.documentElement;
    const prior = root.getAttribute('data-theme');
    const lin = (ch) => { const c = ch / 255; return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
    const lum = (h) => { const r = parseInt(h.slice(1, 3), 16), g = parseInt(h.slice(3, 5), 16), b = parseInt(h.slice(5, 7), 16); return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b); };
    const ratio = (a, b) => { const l1 = lum(a), l2 = lum(b); const hi = Math.max(l1, l2), lo = Math.min(l1, l2); return (hi + 0.05) / (lo + 0.05); };
    const measure = (theme) => {
      root.dataset.theme = theme;
      const css = getComputedStyle(root);
      const hex = (v) => css.getPropertyValue(v).trim();
      const tok = { bg: hex('--bg'), panel: hex('--panel'), active: hex('--hover'), ink: hex('--ink'), dim: hex('--dim'), meta: hex('--meta') };
      const min = (fg) => Math.min(ratio(fg, tok.bg), ratio(fg, tok.panel), ratio(fg, tok.active));
      return { ink: min(tok.ink), dim: min(tok.dim), meta: min(tok.meta) };
    };
    const out = { dark: measure('dark'), light: measure('light') };
    if (prior === null) root.removeAttribute('data-theme'); else root.dataset.theme = prior;
    return out;
  });
  for (const theme of ['dark', 'light']) {
    const r = contrast[theme];
    r.ink >= 4.5 ? ok(`contrast(${theme}): --ink clears AA (${r.ink.toFixed(2)})`) : fail(`contrast(${theme}): --ink below AA (${r.ink.toFixed(2)})`);
    r.dim >= 4.5 ? ok(`contrast(${theme}): --dim clears AA (${r.dim.toFixed(2)})`) : fail(`contrast(${theme}): --dim below AA (${r.dim.toFixed(2)})`);
    r.meta >= 4.5 ? ok(`contrast(${theme}): --meta clears AA (${r.meta.toFixed(2)})`) : fail(`contrast(${theme}): --meta below AA (${r.meta.toFixed(2)})`);
  }
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
  await waitFor('setup: cage posture is legible (writes/reads/network in product words)', async () => {
    const text = await page.$eval('.setup-home', (el) => el.textContent);
    // M4 (single-cage): the three cage dimensions surface, in product words —
    // never "SBPL"/"Seatbelt"/"cage-jargon". A default install reads "open" on
    // every dimension where enforcement is available, or "unavailable here" off
    // macOS. Either way the labels and one legible value must render.
    const hasLabels = /cage \(writes\)/i.test(text) && /cage \(reads\)/i.test(text) && /cage \(network\)/i.test(text);
    const hasValue = /writes (fenced|open)|unavailable here/i.test(text) && /(reads open|some folders hidden|allow-list|unavailable here)/i.test(text) && /(network open|this machine only|network off|unavailable here)/i.test(text);
    return hasLabels && hasValue;
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
    return await windowContextStage().count() === 1
      && /timeout ms/i.test(await windowContextStage().textContent());
  }, 8000);
  await page.click('#cfg-context-add');
  await waitFor('configure: context stage new opens assistant modal', async () => {
    return await page.$eval('#cfg-context-assistant-modal', (el) => el.open).catch(() => false)
      && await page.locator('#cfg-context-assistant-modal .agent-assistant').count() === 1
      && await page.locator('#cfg-context-assistant-modal .agent-assistant-head select').count() === 1;
  }, 5000);
  await waitFor('configure: opening context assistant preserves existing chain', async () => {
    return await windowContextStage().count() === 1;
  }, 5000);
  await page.click('button[aria-label="close context assistant"]');
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
  // M2 (ui-truthfulness): the add-capability modal header must not leak the
  // internal words "kit"/"package" — it speaks in product words (capability).
  {
    const head = await page.$eval('#cfg-kit-add-modal .cfg-modal-head', (el) => el.textContent || '');
    (!/\bkit\b/i.test(head) && !/\bpackages?\b/i.test(head) && /capabilit/i.test(head))
      ? ok('vocabulary: add-capability modal header avoids "kit"/"package"')
      : fail(`vocabulary: internal words leaked into the add modal header (${head})`);
  }
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
    // M2: trust wording is "allowed"/"needs review", never the internal "approved".
    return /allowed|needs review|hook|daemon|local http|broad publish|low surface/i.test(text);
  }, 10000);
  // ── M2 (ui-truthfulness): the internal word "approved" must not appear as a
  // capability status badge — trust reads as "allowed"/"on"/"needs review".
  {
    const configsText = await page.$eval('#setup-configs', (el) => el.textContent);
    !/\bapproved\b/i.test(configsText)
      ? ok('vocabulary: capability badges avoid the internal word "approved"')
      : fail('vocabulary: "approved" leaked into capability badges');
    const signalsTitle = await page.$eval('.nav-signals', (el) => el.getAttribute('title') || '');
    !/\btopic\b/i.test(signalsTitle)
      ? ok('vocabulary: signals nav tooltip avoids the internal word "topic"')
      : fail(`vocabulary: "topic" leaked into the signals nav tooltip (${signalsTitle})`);
  }
  // ── M1 (ui-truthfulness): liveness — a stopped/failed capability must be
  // visibly distinct from a running one, and a never-run one reads "not started".
  // Seed status on the bus for two installed capabilities (the same
  // obs/package/<name>/status the dispatcher publishes), leaving others unseeded.
  // Publish through the web server's own authenticated bus connection (/api/publish)
  // rather than shelling `elanus bus pub`, so the seed does not depend on the CLI's
  // ambient identity env (ELANUS_PACKAGE/ELANUS_BUS_TOKEN when run inside a session).
  await page.evaluate(async () => {
    const pub = (topic, payload) => fetch('/api/publish', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ topic, payload }) });
    await pub('obs/package/git-protect/status', { state: 'alive', pid: 4242 });
    await pub('obs/package/window/status', { state: 'dead', exit_code: 1 });
  });
  await waitFor('liveness: /api/liveness reflects the seeded running/failed states', async () => {
    const j = await page.evaluate(async () => (await fetch('/api/liveness')).json());
    return j?.actors?.['git-protect']?.status === 'running' && j?.actors?.['window']?.status === 'failed';
  }, 8000);
  await page.reload();
  await page.waitForSelector('.nav-setup');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await waitFor('liveness: installed vs running vs failed vs not-started are distinguishable', async () => {
    const gp = await page.locator('#setup-configs .setup-pending-pkg', { hasText: 'git-protect' }).first().textContent().catch(() => '');
    const win = await page.locator('#setup-configs .setup-pending-pkg', { hasText: 'window' }).first().textContent().catch(() => '');
    const all = await page.$eval('#setup-configs', (el) => el.textContent);
    return /running/i.test(gp) && /failed/i.test(win) && /not started/i.test(all);
  }, 10000);
  // ── M3 (ui-truthfulness): cost honesty — a run-step limit is a HARD CAP, a
  // throttle is a SOFT LIMIT; they must render as separate groups, and the
  // estimate is honestly "unknown" (never a fake $0) until pricing is known.
  elanus('profile', 'set', 'default', 'model.max_turns=24', 'throttle.work.llm_tokens_per_hour=50000');
  await page.reload();
  await page.waitForSelector('.nav-setup');
  await page.click('.nav-setup');
  await page.waitForSelector('#view-setup:not([hidden])');
  await page.$eval('.setup-cost', (el) => { el.open = true; });
  await waitFor('cost: hard cap and soft limit render as separate groups', async () => {
    const hard = await page.$eval('.setup-cost .cost-hard', (el) => el.textContent).catch(() => '');
    const soft = await page.$eval('.setup-cost .cost-soft', (el) => el.textContent).catch(() => '');
    return /hard cap/i.test(hard) && /run steps/i.test(hard) && /soft limit/i.test(soft) && /tokens\/hour/i.test(soft);
  }, 8000);
  {
    const est = await page.$eval('.setup-cost .cost-estimate', (el) => el.textContent).catch(() => '');
    (/estimate/i.test(est) && /unknown/i.test(est) && !/\$0\b/.test(est))
      ? ok('cost: estimate is labeled and honest (unknown, never $0)')
      : fail(`cost: estimate wording is not honest (${est})`);
  }
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
  // M2: the message tooltip says "conversation <id>" (never the internal
  // "correlation") — strip that product-word prefix to recover the flow id.
  const msgTitle = await page.$eval('#conv-holder .msg.you', (el) => el.title || '').catch(() => '');
  (/^conversation /.test(msgTitle) && !/correlation/i.test(msgTitle))
    ? ok('vocabulary: message tooltip says "conversation", not the internal "correlation"')
    : fail(`vocabulary: message tooltip wording leaks internal vocabulary (${msgTitle})`);
  const corr = msgTitle.replace('conversation ', '');
  const agentName = await page.$eval('#nav-agents .nav-agent.on', (el) => el.getAttribute('data-sel')?.replace(/^agent:/, '') || 'main').catch(() => 'main');
  elanus('emit', `in/human/owner`, '--correlation', corr || 'spec-fail', '--payload',
    JSON.stringify({ failed: true, error: 'spec-injected failure', agent: agentName }));
  await waitFor('converse: agent failure renders as an error bubble', async () => {
    return (await page.$eval('#conv-holder', (el) => el.textContent)).includes('spec-injected failure');
  }, 8000);
  await page.close();
}

// ── flow 6e: platform-trust M4 — the raw-HTML render gate ─────────────────────
// docs/handoffs/platform-trust.md M4: agent messages render as markdown; raw HTML
// is gated on the platform trust level (bus.toml). At FULL trust rehype-raw is on
// and a message's raw <button> becomes a real element; at REDUCED trust the same
// markup is shown as escaped text (no live element). The gate reads /api/status
// `trust`, which the Rust server derives fresh from bus.toml per request.
{
  const htmlAgent = 'htmlprobe';
  const seed = await newPage();
  await seed.goto('/');
  await seed.evaluate(async (name) => {
    await fetch('/api/admin/agents', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name }),
    }).catch(() => {});
  }, htmlAgent);
  await seed.close();

  // The message carries a raw <button> with a data-sel probe (the same discipline
  // the rest of the spec uses to find live elements).
  const htmlMsg = '<button data-sel="rawhtml-probe">CLICKME-RAW</button>';
  const openConverse = async (page) => {
    await page.waitForSelector('#nav-agents .nav-agent');
    await waitFor('trust-html: agent selectable', async () => {
      const agents = await page.$$('#nav-agents .nav-agent');
      for (const item of agents) {
        if ((await item.getAttribute('data-sel')) === `agent:${htmlAgent}`) { await item.click(); return true; }
      }
      return false;
    }, 8000);
    await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  };

  // FULL trust (the setup default — bus.toml has no `trust`, so full): the raw
  // <button> renders as a REAL element inside the feed.
  const full = await newPage();
  await full.goto('/');
  await openConverse(full);
  await full.fill('#compose-input', htmlMsg);
  await full.click('#compose-send');
  await waitFor('trust-html: full trust renders raw <button> as a real element', async () => {
    return (await full.$('#conv-holder button[data-sel="rawhtml-probe"]')) !== null;
  }, 8000);
  await full.close();

  // Flip to REDUCED trust (keep the bind so the stack stays connected), reload so
  // the SPA re-fetches /api/status, and re-send the same message: now the markup
  // is visible TEXT and there is NO live <button>.
  fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\ntrust = "reduced"\n`);
  const reduced = await newPage();
  await reduced.goto('/');
  await waitFor('trust-html: status reports reduced trust after flip', async () => {
    try { return (await (await fetch(`${BASE}/api/status`)).json()).trust === 'reduced'; } catch { return false; }
  }, 8000);
  await openConverse(reduced);
  await reduced.fill('#compose-input', htmlMsg);
  await reduced.click('#compose-send');
  await waitFor('trust-html: reduced trust shows the HTML as escaped text', async () => {
    const t = await reduced.$eval('#conv-holder', (el) => el.textContent);
    return t.includes('<button') && t.includes('CLICKME-RAW');
  }, 8000);
  const liveBtn = await reduced.$('#conv-holder button[data-sel="rawhtml-probe"]');
  liveBtn === null
    ? ok('trust-html: reduced trust does NOT create a live <button> (escaped)')
    : fail('trust-html: reduced trust leaked a live <button> element');
  await reduced.close();
  // Restore full trust for the remaining flows (they assume the default posture).
  fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
}

// ── flow 6b: chat-rendering M2 — comms-plane-vs-trace decision ────────────────
// docs/handoffs/chat-rendering.md M2: the converse view decides what to show by
// WHETHER comms-plane traffic exists between the owner and the agent (a read off
// the ledger), not by any per-agent flag.
//  - a comms-plane agent (it has in/agent prompts + correlated in/human replies)
//    renders #view-converse[data-mode="comms"] with >=1 message in the feed;
//  - a trace-only agent (a coding worker, no comms-plane traffic) renders
//    #view-converse[data-mode="trace"] with NO chat feed, and is present in the
//    runs surface (/api/code/sessions).
// The decision is derivable purely from bus/ledger reads — the projection
// (/api/conversations) returns rows for the comms agent and [] for the worker.
{
  const commsAgent = 'companion';
  // The comms-plane agent: create its profile (so nav lists it), then seed a
  // conversation on the comms plane — an in/agent prompt on a NON-worker (web-)
  // session plus a correlated in/human reply. conversation_rows threads exactly
  // this shape; the same read any third-party UI would make.
  const seedPage = await newPage();
  await seedPage.goto('/');
  const created = await seedPage.evaluate(async (name) => {
    const r = await fetch('/api/admin/agents', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    return r.json().catch(() => ({}));
  }, commsAgent);
  created.ok ? ok(`chat-render: created comms agent ${commsAgent}`) : fail(`chat-render: could not create ${commsAgent} (${JSON.stringify(created)})`);
  await seedPage.close();
  const commsCorr = `chatrender-${Date.now().toString(36)}`;
  try {
    elanus('emit', `in/agent/${commsAgent}`, '--correlation', commsCorr, '--payload',
      JSON.stringify({ prompt: 'hello companion', session: `web-${commsAgent}-seed` }));
    elanus('emit', 'in/human/owner', '--correlation', commsCorr, '--payload',
      JSON.stringify({ text: 'hi there — I am here when you need me', session: `web-${commsAgent}-seed` }));
  } catch (e) { fail(`chat-render: seeding comms-plane traffic failed: ${e.message ?? e}`); }
  // The trace-only agent: a coding worker. Seed on the EXACT in/agent/<agent>
  // topic the projection queries, with a bus-derived `code-*` SESSION in the
  // payload — so this genuinely exercises session-based eviction (not a topic
  // mismatch). conversation_rows drops `code-*` worker sessions, so
  // /api/conversations?agent=claude-code returns []  →  trace fallback.
  const traceAgent = 'claude-code';
  try {
    elanus('emit', `in/agent/${traceAgent}`, '--payload',
      JSON.stringify({ prompt: 'run the build', session: 'code-chatrender01' }));
  } catch (e) { fail(`chat-render: seeding trace-only traffic failed: ${e.message ?? e}`); }

  // REGRESSION (codex cross-model verify, 2026-06-25): the comms-vs-trace gate
  // must key on the `code-*` SESSION, never the agent NOUN. A coding-noun agent
  // (codex/claude-code) that DID curate a comms-plane conversation on a NON-
  // `code-*` session MUST still surface as comms. Seed exactly that under the
  // `codex` noun — an in/agent/codex prompt on a web- session + a correlated
  // in/human reply — and assert below that the projection keeps it. Without the
  // fix (noun-gated is_worker_session) this returns 0 and the assertion fails.
  const nounAgent = 'codex';
  const nounCorr = `chatrender-noun-${Date.now().toString(36)}`;
  try {
    elanus('emit', `in/agent/${nounAgent}`, '--correlation', nounCorr, '--payload',
      JSON.stringify({ prompt: 'ping from a coding-noun agent', session: `web-${nounAgent}-comms` }));
    elanus('emit', 'in/human/owner', '--correlation', nounCorr, '--payload',
      JSON.stringify({ text: 'a coding-noun agent can still hold a conversation', session: `web-${nounAgent}-comms` }));
  } catch (e) { fail(`chat-render: seeding coding-noun comms traffic failed: ${e.message ?? e}`); }

  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');

  // The comms-plane agent renders the conversation from the comms plane.
  const commsConv = await page.evaluate(async (name) => {
    const r = await fetch(`/api/conversations?agent=${encodeURIComponent(name)}`);
    const j = await r.json().catch(() => ({}));
    return { ok: !!j.ok, count: (j.conversations ?? []).length };
  }, commsAgent);
  commsConv.ok && commsConv.count > 0
    ? ok(`chat-render: comms projection returns ${commsConv.count} conversation(s) for ${commsAgent}`)
    : fail(`chat-render: comms projection empty for ${commsAgent} (${JSON.stringify(commsConv)})`);
  await waitFor(`chat-render: ${commsAgent} in nav`, async () => {
    const items = await page.$$('#nav-agents .nav-agent');
    for (const item of items) {
      if ((await item.getAttribute('data-sel')) === `agent:${commsAgent}`) { await item.click(); return true; }
    }
    return false;
  }, 10000);
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  // The decision lands on the comms plane for this agent.
  await waitFor('chat-render: comms agent renders the comms-plane chat surface', async () => {
    const mode = await page.$eval('#view-converse', (el) => el.getAttribute('data-mode')).catch(() => null);
    return mode === 'comms';
  }, 10000);
  // Open the seeded conversation; conversation_messages projects the raw in/agent
  // prompt + correlated in/human reply (the comms plane) into the feed.
  await waitFor('chat-render: seeded conversation listed in nav', async () => {
    const rows = await page.$$('#nav-agents .nav-conversation');
    if (rows.length) { await rows[0].click(); return true; }
    return false;
  }, 10000);
  await waitFor('chat-render: comms agent shows a comms-plane conversation (>=1 message)', async () => {
    const mode = await page.$eval('#view-converse', (el) => el.getAttribute('data-mode')).catch(() => null);
    const msgs = await page.$$eval('.conv-feed .msg', (els) => els.length).catch(() => 0);
    return mode === 'comms' && msgs >= 1;
  }, 10000);

  // The trace-only agent shows NO chat conversation; it falls back to the trace.
  const traceConv = await page.evaluate(async (name) => {
    const r = await fetch(`/api/conversations?agent=${encodeURIComponent(name)}`);
    const j = await r.json().catch(() => ({}));
    return { ok: !!j.ok, count: (j.conversations ?? []).length };
  }, traceAgent);
  traceConv.ok && traceConv.count === 0
    ? ok(`chat-render: comms projection returns no conversations for trace-only ${traceAgent}`)
    : fail(`chat-render: trace-only ${traceAgent} unexpectedly has conversations (${JSON.stringify(traceConv)})`);

  // REGRESSION: a coding-NOUN agent with a non-`code-*` comms conversation MUST
  // surface as comms (the projection keys on session, not noun). This would fail
  // under the old noun-gated is_worker_session.
  const nounConv = await page.evaluate(async (name) => {
    const r = await fetch(`/api/conversations?agent=${encodeURIComponent(name)}`);
    const j = await r.json().catch(() => ({}));
    const convs = j.conversations ?? [];
    return { ok: !!j.ok, count: convs.length, sessions: convs.map((c) => c.session) };
  }, nounAgent);
  nounConv.ok && nounConv.count >= 1 && nounConv.sessions.every((s) => !String(s).startsWith('code-'))
    ? ok(`chat-render: coding-noun ${nounAgent} surfaces its comms conversation (noun does not gate): ${JSON.stringify(nounConv.sessions)}`)
    : fail(`chat-render: coding-noun ${nounAgent} comms conversation wrongly gated by noun (${JSON.stringify(nounConv)})`);
  // Reach the worker's converse view: it is evicted to the Workers drawer and
  // lands on telemetry; click the converse tab to inspect the decision there.
  const opened = await page.evaluate(async (name) => {
    const drawer = document.querySelector('#nav-workers');
    if (drawer && !drawer.open) drawer.open = true;
    const btns = [...document.querySelectorAll('#nav-workers .nav-worker')];
    const hit = btns.find((b) => (b.textContent || '').includes(name));
    if (!hit) return false;
    hit.click();
    return true;
  }, traceAgent);
  if (opened) {
    await page.waitForSelector('#agent-tabs', { state: 'visible' });
    await page.click('[data-tab="converse"]');
    await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
    await waitFor('chat-render: trace-only agent shows NO chat conversation (trace fallback)', async () => {
      const mode = await page.$eval('#view-converse', (el) => el.getAttribute('data-mode')).catch(() => null);
      const feed = await page.$('.conv-feed');
      const runsLink = await page.$('#conv-open-runs');
      return mode === 'trace' && !feed && !!runsLink;
    }, 10000);
    // The trace-only agent IS reachable in the runs/trace surface: it is listed in
    // the Workers drawer (the UI's entry into the obs-trace surface). `opened`
    // already located it there. The trace fallback's own "open runs" link routes to
    // the runs view; click it to confirm the route.
    ok(`chat-render: trace-only ${traceAgent} is present in the runs/trace surface (Workers drawer)`);
    // Guard: a missing/late #conv-open-runs must not throw an uncaught TimeoutError
    // that aborts the whole suite — degrade to a recorded failure for this assertion.
    const runsLink = await page.$('#conv-open-runs');
    if (runsLink) {
      await runsLink.click();
      await waitFor('chat-render: trace fallback links into the runs surface', async () =>
        page.$eval('[data-sel="code-sessions"]', (el) => el.classList.contains('on')).catch(() => false), 5000);
    } else {
      fail('chat-render: trace fallback #conv-open-runs link missing (cannot route into runs surface)');
    }
    // The obs-driven code projection (/api/code/sessions) is populated by flight-
    // recorder telemetry the spec stack does not stand up (same limitation flow 11
    // documents); when present it should list the worker, else tolerate its absence.
    const inRuns = await page.evaluate(async (name) => {
      const r = await fetch('/api/code/sessions');
      if (!r.ok) return null;
      const j = await r.json().catch(() => null);
      const rows = Array.isArray(j) ? j : (j?.sessions ?? j?.rows ?? []);
      if (!Array.isArray(rows)) return null;
      return rows.length === 0 ? null : rows.some((s) => JSON.stringify(s).includes(name));
    }, traceAgent);
    inRuns === false
      ? fail(`chat-render: code projection has runs but not trace-only ${traceAgent}`)
      : ok(`chat-render: code projection ${inRuns === true ? `lists ${traceAgent}` : 'has no projected runs in this stack — tolerated'}`);
  } else {
    fail(`chat-render: could not reach the trace-only agent ${traceAgent} in the Workers drawer`);
  }
  await page.close();
}

// ── flow 6d: html-messages — an agent's format="html" reply renders deliberately ─
// docs/handoffs/html-messages.md M2: `format` records the agent's DELIBERATE
// intent on the ledger and the feed renders BY it — but raw HTML becoming live
// DOM stays gated on ONE thing: platform trust===full. So:
//  - at FULL trust, a format="html" reply carrying a <button> renders a REAL
//    <button> element, while a default (markdown) reply renders as markdown
//    (no raw element leaks from it);
//  - at REDUCED trust, the same format="html" body shows its markup as visible
//    ESCAPED text, no live element — `format` never widens the trust gate.
// Trust is flipped by rewriting bus.toml between renders (/api/status reads it
// fresh); restored to full afterward so later flows are unaffected.
{
  const htmlAgent = 'formsmith';
  const htmlSession = `web-${htmlAgent}-html`;
  const htmlCorr = `htmlmsg-${Date.now().toString(36)}`;
  const busReduced = `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\ntrust = "reduced"\n`;
  const busFull = `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`;
  // Create the comms agent (POST is a human gesture; do it while trust is full).
  const seedPage = await newPage();
  await seedPage.goto('/');
  const created = await seedPage.evaluate(async (name) => {
    const r = await fetch('/api/admin/agents', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    return r.json().catch(() => ({}));
  }, htmlAgent);
  created.ok ? ok(`html-messages: created comms agent ${htmlAgent}`) : fail(`html-messages: could not create ${htmlAgent} (${JSON.stringify(created)})`);
  await seedPage.close();
  // Seed a comms conversation: an in/agent prompt + two correlated agent replies
  // on in/human/<owner> — one format="html" carrying a <button>, one default.
  try {
    elanus('emit', `in/agent/${htmlAgent}`, '--correlation', htmlCorr, '--payload',
      JSON.stringify({ prompt: 'give me a way to pick', session: htmlSession }));
    elanus('emit', 'in/human/owner', '--correlation', htmlCorr, '--payload',
      JSON.stringify({ text: '<button data-html-demo="1">Press me</button>', session: htmlSession, format: 'html' }));
    elanus('emit', 'in/human/owner', '--correlation', htmlCorr, '--payload',
      JSON.stringify({ text: '**plain markdown reply** <button data-html-plain="1">no</button>', session: htmlSession, format: 'markdown' }));
  } catch (e) { fail(`html-messages: seeding failed: ${e.message ?? e}`); }

  // Open the conversation and return the state of the two seeded replies.
  const openConversation = async (page) => {
    await page.goto('/');
    await page.waitForSelector('#nav-agents .nav-item');
    await waitFor(`html-messages: ${htmlAgent} in nav`, async () => {
      const items = await page.$$('#nav-agents .nav-agent');
      for (const item of items) {
        if ((await item.getAttribute('data-sel')) === `agent:${htmlAgent}`) { await item.click(); return true; }
      }
      return false;
    }, 10000);
    await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
    // The nav lists EVERY agent's conversations, so scope to THIS one by its
    // session title (the button's `title` is c.session) rather than rows[0].
    await waitFor('html-messages: seeded conversation listed in nav', async () => {
      const rows = await page.$$('#nav-agents .nav-conversation');
      for (const row of rows) {
        if ((await row.getAttribute('title')) === htmlSession) { await row.click(); return true; }
      }
      return false;
    }, 10000);
    await waitFor('html-messages: conversation feed has the seeded replies', async () => {
      const msgs = await page.$$eval('.conv-feed .msg', (els) => els.length).catch(() => 0);
      return msgs >= 2;
    }, 10000);
  };

  // FULL trust (the stack default): the html reply is a live <button>; the
  // markdown reply renders as markdown (its inline <button> stays escaped — only
  // format="html" gets the whole-body HTML path).
  {
    const page = await newPage();
    await openConversation(page);
    // /api/status reports full so the SPA sets allowHtml=true.
    const trust = await page.evaluate(async () => (await (await fetch('/api/status')).json()).trust);
    trust === 'full' ? ok('html-messages: platform trust is full for the live-render case') : fail(`html-messages: expected full trust, got ${trust}`);
    const liveBtn = await page.$('.conv-feed .msg-html button[data-html-demo="1"]');
    liveBtn ? ok('html-messages: full trust renders format="html" as a real <button>') : fail('html-messages: full trust did not render the html reply as a live <button>');
    // The default markdown reply renders markdown (a <strong>), and its inline
    // <button> is NOT a live element (markdown text, not raw HTML block).
    const strong = await page.$('.conv-feed strong');
    strong ? ok('html-messages: default reply renders as markdown (<strong>)') : fail('html-messages: default reply did not render markdown');
    // Decision 3 (small touches): at full trust, inline raw HTML inside an
    // ORDINARY markdown message also renders live (today's rehype-raw path) —
    // format="html" is the whole-body mode, not the only way HTML renders.
    const plainBtn = await page.$('.conv-feed button[data-html-plain="1"]');
    plainBtn ? ok('html-messages: full trust also renders inline HTML in a markdown reply (small touches)') : fail('html-messages: inline HTML in a markdown reply did not render at full trust');
    await page.close();
  }

  // REDUCED trust: rewrite bus.toml and reload — the same html body now shows as
  // visible escaped text, no live element. The trust gate, not `format`, decides.
  fs.writeFileSync(path.join(TMP, 'bus.toml'), busReduced);
  {
    const page = await newPage();
    await openConversation(page);
    const trust = await page.evaluate(async () => (await (await fetch('/api/status')).json()).trust);
    trust === 'reduced' ? ok('html-messages: platform trust flipped to reduced') : fail(`html-messages: expected reduced trust, got ${trust}`);
    const liveBtn = await page.$('.conv-feed .msg-html button[data-html-demo="1"]');
    liveBtn ? fail('html-messages: reduced trust must NOT render a live <button>') : ok('html-messages: reduced trust renders no live element for the html reply');
    const escaped = await page.$eval('.conv-feed', (el) => el.textContent.includes('<button data-html-demo="1">Press me</button>')).catch(() => false);
    escaped ? ok('html-messages: reduced trust shows the html markup as visible escaped text') : fail('html-messages: reduced trust did not show the html markup as escaped text');
    await page.close();
  }
  // Restore full trust so later flows (which POST as the human) are unaffected.
  fs.writeFileSync(path.join(TMP, 'bus.toml'), busFull);
}

// ── flow 6c: ambient-conversations — an agent that speaks first is replyable ───
// docs/handoffs/ambient-conversations.md: an unprompted send_message (a timer or
// event handler firing, no preceding in/agent prompt) must surface as its own
// labeled, replyable conversation — not a notification you can only watch. The
// send lands on in/human/<owner> carrying the run's session and is sent by the
// agent (sender = the agent noun), which is the only link back to the agent.
//  - /api/conversations?agent=<name> returns a conversation for it;
//  - it opens a #view-converse thread with >=1 message;
//  - its source badge is NOT "you" (honest: the agent reached out);
//  - replying into it appends and threads by the conversation's session, with no
//    duplicate of the just-sent message.
{
  const ambientAgent = 'beacon';
  const ambientSession = `run-${ambientAgent}-${Date.now().toString(36)}`;
  const ambientText = `your nightly build finished — all green (${Date.now().toString(36)})`;
  // `elanus emit` records the sender from ELANUS_ACTOR; the real send_message
  // path runs with ELANUS_ACTOR = the agent noun, so seed the same way.
  const emitAs = (actor, ...a) =>
    execFileSync(path.join(BIN, 'elanus'), a, { env: { ...ENV, ELANUS_ACTOR: actor }, encoding: 'utf8' });

  const seedPage = await newPage();
  await seedPage.goto('/');
  const created = await seedPage.evaluate(async (name) => {
    const r = await fetch('/api/admin/agents', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    return r.json().catch(() => ({}));
  }, ambientAgent);
  created.ok ? ok(`ambient: created agent ${ambientAgent}`) : fail(`ambient: could not create ${ambientAgent} (${JSON.stringify(created)})`);
  await seedPage.close();
  try {
    // No in/agent prompt, no correlation: a fully unprompted agent-first send.
    // `source: cron` declares the timer origin so the badge is honestly not "you".
    emitAs(ambientAgent, 'emit', 'in/human/owner', '--payload',
      JSON.stringify({ text: ambientText, session: ambientSession, source: 'cron' }));
  } catch (e) { fail(`ambient: seeding the unprompted send failed: ${e.message ?? e}`); }

  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');

  // The projection returns the ambient conversation, badged not-"you".
  const ambientConv = await page.evaluate(async (name) => {
    const r = await fetch(`/api/conversations?agent=${encodeURIComponent(name)}`);
    const j = await r.json().catch(() => ({}));
    const convs = j.conversations ?? [];
    return { ok: !!j.ok, count: convs.length, sources: convs.map((c) => c.source), sessions: convs.map((c) => c.session) };
  }, ambientAgent);
  ambientConv.ok && ambientConv.count >= 1 && ambientConv.sessions.includes(ambientSession)
    ? ok(`ambient: projection returns the unprompted conversation for ${ambientAgent}`)
    : fail(`ambient: projection missing the unprompted conversation (${JSON.stringify(ambientConv)})`);
  ambientConv.sources.every((s) => s !== 'you')
    ? ok(`ambient: source badge marks it agent-initiated, not "you" (${JSON.stringify(ambientConv.sources)})`)
    : fail(`ambient: an agent-initiated conversation was badged "you" (${JSON.stringify(ambientConv.sources)})`);

  // Select the agent, then open the ambient conversation from the nav.
  await waitFor(`ambient: ${ambientAgent} in nav`, async () => {
    const items = await page.$$('#nav-agents .nav-agent');
    for (const item of items) {
      if ((await item.getAttribute('data-sel')) === `agent:${ambientAgent}`) { await item.click(); return true; }
    }
    return false;
  }, 10000);
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  // The ambient conversation is listed with a non-"you" source badge. Scope to
  // THIS conversation by its unique title text — the nav lists every agent's
  // conversations, so a bare `.nav-conversation` would match a neighbour.
  await waitFor('ambient: nav lists the conversation with a non-"you" badge', async () => {
    const rows = await page.$$eval('#nav-agents .nav-conversation', (els, t) => els
      .filter((el) => (el.textContent || '').includes(t))
      .map((el) => (el.querySelector('.source-badge')?.textContent || '').trim()), ambientText).catch(() => []);
    return rows.length >= 1 && rows.every((b) => b && b !== 'you');
  }, 10000);
  await waitFor('ambient: open the unprompted conversation', async () => {
    const rows = await page.$$('#nav-agents .nav-conversation');
    for (const row of rows) {
      const txt = (await row.textContent()) || '';
      if (txt.includes(ambientText)) { await row.click(); return true; }
    }
    return false;
  }, 10000);
  // The thread is not empty — the agent-first send renders as a turn.
  await waitFor('ambient: opens a replyable thread with the agent-first message', async () => {
    const msgs = await page.$$eval('.conv-feed .msg', (els) => els.length).catch(() => 0);
    const text = await page.$eval('#conv-holder', (el) => el.textContent).catch(() => '');
    return msgs >= 1 && text.includes(ambientText);
  }, 10000);

  // Replying into it appends and threads onto the conversation's session — no dup.
  const publishes = [];
  page.on('request', (req) => {
    if (!req.url().endsWith('/api/publish') || req.method() !== 'POST') return;
    try { publishes.push(JSON.parse(req.postData() || '{}')); } catch {}
  });
  const reply = `on it — thanks beacon (${Date.now().toString(36)})`;
  await page.fill('#compose-input', reply);
  await page.click('#compose-send');
  await waitFor('ambient: reply appends to the thread', async () =>
    (await page.$eval('#conv-holder', (el) => el.textContent)).includes(reply), 8000);
  await waitFor('ambient: reply threads onto the ambient conversation session', async () =>
    publishes.length >= 1 && publishes.some((p) => p.payload?.session === ambientSession && p.correlation), 8000);
  // No duplicate of the just-sent reply (idempotent optimistic insert ⊕ echo).
  const replyCount = await page.$$eval('#conv-holder .msg', (els, r) => els.filter((el) => (el.textContent || '').includes(r)).length, reply).catch(() => 0);
  replyCount === 1
    ? ok('ambient: the reply renders exactly once (no duplicate)')
    : fail(`ambient: the reply rendered ${replyCount} times (expected 1)`);
  await page.close();
}

// ── flow 6f: timers — a scheduled self-wake fires, and its message is replyable ─
// docs/handoffs/timers.md M5 (the sanctioned split). Two halves, provider-free:
//  (ledger) `elanus schedule` inserts a one-shot row; the LIVE daemon's
//    tick_schedules fires it once into in/agent/<agent> — schedule→fire proven
//    on the running stack, not just a unit test.
//  (ui) the ambient message a woken agent would send (send_message → in/human/
//    <owner> carrying the run's session, source=timer) renders as a replyable,
//    agent-initiated thread in #view-converse — the wake→send→render outcome.
// The wake TURN itself (the LLM call between fire and send) is out of scope here
// exactly as the handoff prescribes: it needs a model, which this stack lacks.
{
  const timerAgent = 'chrono';
  const timerSession = `run-${timerAgent}-${Date.now().toString(36)}`;
  const timerText = `reminder: the kettle is done (${Date.now().toString(36)})`;
  const emitAs = (actor, ...a) =>
    execFileSync(path.join(BIN, 'elanus'), a, { env: { ...ENV, ELANUS_ACTOR: actor }, encoding: 'utf8' });

  const seedPage = await newPage();
  await seedPage.goto('/');
  const created = await seedPage.evaluate(async (name) => {
    const r = await fetch('/api/admin/agents', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    return r.json().catch(() => ({}));
  }, timerAgent);
  created.ok ? ok(`timers: created agent ${timerAgent}`) : fail(`timers: could not create ${timerAgent} (${JSON.stringify(created)})`);
  await seedPage.close();

  // (ledger) The CLI schedules a one-shot wake; the running daemon fires it.
  // --in 0 is due immediately; the next tick emits in/agent/chrono exactly once.
  try {
    elanus('schedule', '--agent', timerAgent, '--in', '0', '--message', 'wake up and post the reminder');
    ok('timers: elanus schedule inserted a one-shot wake');
  } catch (e) { fail(`timers: elanus schedule failed: ${e.message ?? e}`); }
  const mailbox = `in/agent/${timerAgent}`;
  await waitFor('timers: the live daemon fires the scheduled wake onto the mailbox', async () => {
    // `elanus events` reads the ledger the daemon writes; the fired one-shot
    // shows up as an in/agent/<agent> row (state is irrelevant — no LLM here).
    try { return elanus('events', '--limit', '50').includes(mailbox); }
    catch { return false; }
  }, 10000);

  // (ui) The message the woken agent would send renders as a replyable thread.
  try {
    emitAs(timerAgent, 'emit', 'in/human/owner', '--payload',
      JSON.stringify({ text: timerText, session: timerSession, source: 'timer' }));
  } catch (e) { fail(`timers: seeding the woken send failed: ${e.message ?? e}`); }

  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents .nav-item');
  const timerConv = await page.evaluate(async (name) => {
    const r = await fetch(`/api/conversations?agent=${encodeURIComponent(name)}`);
    const j = await r.json().catch(() => ({}));
    const convs = j.conversations ?? [];
    return { ok: !!j.ok, sessions: convs.map((c) => c.session), sources: convs.map((c) => c.source) };
  }, timerAgent);
  timerConv.ok && timerConv.sessions.includes(timerSession) && timerConv.sources.every((s) => s !== 'you')
    ? ok(`timers: the wake's message is an agent-initiated conversation (${JSON.stringify(timerConv.sources)})`)
    : fail(`timers: the wake's message did not surface replyably (${JSON.stringify(timerConv)})`);

  await waitFor(`timers: ${timerAgent} in nav`, async () => {
    const items = await page.$$('#nav-agents .nav-agent');
    for (const item of items) {
      if ((await item.getAttribute('data-sel')) === `agent:${timerAgent}`) { await item.click(); return true; }
    }
    return false;
  }, 10000);
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  await waitFor('timers: the wake message opens a replyable thread', async () => {
    const rows = await page.$$('#nav-agents .nav-conversation');
    for (const row of rows) {
      const txt = (await row.textContent()) || '';
      if (txt.includes(timerText)) { await row.click(); return true; }
    }
    return false;
  }, 10000);
  await waitFor('timers: the thread carries the agent-first message and a compose box', async () => {
    const msgs = await page.$$eval('.conv-feed .msg', (els) => els.length).catch(() => 0);
    const text = await page.$eval('#conv-holder', (el) => el.textContent).catch(() => '');
    const canReply = await page.$('#compose-input');
    return msgs >= 1 && text.includes(timerText) && !!canReply;
  }, 10000);
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
  const outlineAfterKeyboardFocus = async (selector) => {
    await page.keyboard.press('Tab');
    return await page.$eval(selector, (el) => {
      el.focus();
      const o = getComputedStyle(el);
      return {
        width: o.outlineWidth,
        style: o.outlineStyle,
        visible: o.outlineStyle !== 'none' && parseFloat(o.outlineWidth) > 0,
      };
    });
  };
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])', { timeout: 5000 });
  await waitForConfigureLoaded(page);
  await waitFor('a11y: configure model control enabled', async () => {
    return await page.$eval('#cfg-model', (el) => !el.disabled).catch(() => false);
  }, 8000);
  const cfgOutline = await outlineAfterKeyboardFocus('#cfg-model');
  cfgOutline.visible
    ? ok(`a11y: configure input keeps keyboard focus outline (${cfgOutline.width} ${cfgOutline.style})`)
    : fail(`a11y: configure input stripped keyboard focus outline (${JSON.stringify(cfgOutline)})`);
  await page.click('[data-tab="converse"]');
  await page.waitForSelector('#view-converse:not([hidden])', { timeout: 5000 });
  const composeOutline = await outlineAfterKeyboardFocus('#compose-input');
  composeOutline.visible
    ? ok(`a11y: compose input keeps keyboard focus outline (${composeOutline.width} ${composeOutline.style})`)
    : fail(`a11y: compose input stripped keyboard focus outline (${JSON.stringify(composeOutline)})`);
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
  await page.click('#vocabulary-toggle');
  await waitFor('identity: cockpit toggle changes panel-head noun', async () => {
    const h = await page.$eval('.nav .panel-head h2', (el) => el.textContent.trim()).catch(() => '');
    return h && h !== warmHead;
  });
  await page.click('#vocabulary-toggle');
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
  // The compose button is an icon (➤); its accessible name carries the
  // vocabulary word ("Send" warm / "transmit" cockpit) via aria-label.
  const sendLabel = await page.$eval('#compose-send', (el) => (el.getAttribute('aria-label') || '').trim()).catch(() => '');
  /^send$/i.test(sendLabel) ? ok('language: compose button labelled "Send"') : fail(`language: compose button labelled "${sendLabel}"`);
  await page.click('#vocabulary-toggle');
  await waitFor('language: cockpit mode changes compose button to "transmit"', async () => {
    const label = await page.$eval('#compose-send', (el) => (el.getAttribute('aria-label') || '').trim()).catch(() => '');
    return /^transmit$/i.test(label);
  });
  await page.click('#vocabulary-toggle');
  await waitFor('language: warm mode restores compose button to "Send"', async () => {
    const label = await page.$eval('#compose-send', (el) => (el.getAttribute('aria-label') || '').trim()).catch(() => '');
    return /^send$/i.test(label);
  });
  await page.click('[data-tab="sessions"]');
  await page.waitForSelector('#view-sessions:not([hidden])', { timeout: 5000 });
  await waitFor('language: history view resolved', async () => {
    const t = await page.$eval('#sessions-pane', (el) => el.textContent);
    return !/asking the history view/i.test(t);
  }, 12000);
  const historyHeaders = await page.$$eval('.sess-head span', (els) => els.map((el) => el.textContent.trim())).catch(() => []);
  const sessionHeaderGone = !historyHeaders.some((h) => /^session$/i.test(h));
  sessionHeaderGone
    ? ok(`language: history column header avoids "session" (${historyHeaders.join('|') || 'no rows'})`)
    : fail(`language: history column header still says "session" (${historyHeaders.join('|')})`);
  const visibleHistoryIds = await page.$$eval('.sess-row:not(.sess-head) .sess-id', (els) => els.map((el) => el.textContent.trim()).filter((t) => /^(web|code)-/.test(t))).catch(() => []);
  visibleHistoryIds.length === 0
    ? ok('language: history rows do not show raw web-/code- ids')
    : fail(`language: history rows show raw ids: ${visibleHistoryIds.join(', ')}`);
  await page.close();
}


// ── flow 11: agent-comms-ui — comms view, blocks, estimate, signal lamp ─────
// The human's seat for the cross-agent comms plane (docs/handoffs/agent-comms-ui.md).
// M2 comms traffic, M3 rooms panel, M4 block inspector, M5 estimate-vs-actual,
// M6 signal lamp. These routes (/api/comms/*, /api/blocks, /api/estimate/*) are
// web.rs-only, so this flow is most meaningful in ELANUS_UI_SPEC_RUST=1 mode; it
// degrades to empty-state assertions against the node server.
{
  // Seed agent-to-agent mail: a normal delivery and a high-priority one (the
  // projection threads `in/agent/<noun>/<code-session>` events). `elanus emit`
  // writes them to the same ledger the projection reads.
  try {
    elanus('emit', 'in/agent/claude-code/code-uimail01', '--payload', JSON.stringify({ prompt: 'please run the tests' }));
    elanus('emit', 'in/agent/claude-code/code-uimail02', '--payload', JSON.stringify({ prompt: 'URGENT: prod is down' }), '--priority', '9');
  } catch (e) { fail(`seeding comms mail failed: ${e.message ?? e}`); }
  // Seed a durable session-scope block the inspector should list (owner code-agent
  // is the inspector's fallback owner when a session has no record).
  try {
    elanus('block', 'set', 'identity', 'I am the worker.', '--owner', 'code-agent', '--session', 'code-uiblk01', '--scope', 'session');
  } catch (e) { fail(`seeding a block failed: ${e.message ?? e}`); }
  // Seed an estimate for a session so the runs detail shows the estimate group.
  try {
    elanus('estimate', 'set', '--session', 'code-uiest01', '--dollars', '0.40', '--turns', '8', '--tokens', '1000');
  } catch (e) { fail(`seeding an estimate failed: ${e.message ?? e}`); }

  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents', { timeout: 10000 });

  // M2 — open the comms view from the nav.
  await page.click('[data-sel="comms"]');
  await page.waitForSelector('#view-comms:not([hidden])', { timeout: 5000 });
  ok('comms: view opens from the nav');

  // The mail list shows at least one row with a priority chip (against seeded
  // mail), or the documented empty-state copy (node server, no routes). The
  // /api/comms/mail fetch is async — wait until the view has settled to EITHER
  // a rendered row or the empty-state, so we never read the transient pre-fetch
  // state (which would self-mask by landing on the empty branch every time).
  await page.waitForFunction(() => document.querySelector('.comms-row') || document.querySelector('.cm-empty'), null, { timeout: 8000 });
  const commsState = await page.evaluate(() => {
    const rows = [...document.querySelectorAll('.comms-row')];
    const chips = rows.map((r) => !!r.querySelector('.cm-chip'));
    const high = !!document.querySelector('.cm-high');
    const empty = !!document.querySelector('.cm-empty');
    return { rowCount: rows.length, anyChip: chips.some(Boolean), high, empty };
  });
  // The Rust server serves the seeded-mail projection (/api/comms/mail), so M2
  // MUST render rows here — the empty-state fallback is NOT acceptable. Require
  // rows with chips AND the high-priority chip from the seeded priority-9 mail.
  commsState.rowCount > 0 && commsState.anyChip
    ? ok(`comms: ${commsState.rowCount} mail row(s) with priority chips render`)
    : fail(`comms: seeded mail did not render rows with chips (${JSON.stringify(commsState)})`);
  commsState.high
    ? ok('comms: a high-priority delivery shows the high chip')
    : fail('comms: seeded high-priority mail did not show a high chip');

  // M3 — the rooms panel is present (empty-state or a .comms-room). Seeding live
  // room membership needs a real session, so this asserts the panel renders; the
  // roster/claim shape is covered by the Rust recent_rooms unit test.
  const roomsPanel = await page.$eval('.cm-rooms', (el) => el.querySelector('h3')?.textContent ?? '').catch(() => '');
  /rooms/i.test(roomsPanel) ? ok('comms: rooms & shared channels panel renders') : fail('comms: rooms panel missing');

  // M6 — the signal lamp lights when a high-priority in/agent delivery crosses the
  // live stream. Publish one over /api/publish (the lamp is wired in App.tsx and
  // clears on click) and assert it gains the `lit` class.
  await page.evaluate(async () => {
    await fetch('/api/publish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ topic: 'in/agent/claude-code/code-uilamp01', payload: { prompt: 'urgent live', priority: 9, event_id: 99001 } }),
    });
  });
  const lampLit = await waitFor('comms: signal lamp lights on a high-priority in/agent event', async () =>
    page.$eval('#signal-lamp', (el) => el.classList.contains('lit')).catch(() => false), 8000);
  if (lampLit) {
    await page.click('#signal-lamp');
    const cleared = await page.$eval('#signal-lamp', (el) => !el.classList.contains('lit')).catch(() => false);
    cleared ? ok('comms: signal lamp clears on click') : fail('comms: signal lamp did not clear on click');
  }

  // M4 + M5 — the block inspector and the estimate group live in the runs detail.
  // Open runs and select a seeded session; the projection only shows sessions the
  // daemon has observed, so we assert the panels render when the session is
  // present, and tolerate its absence (no projected run) without failing the suite.
  await page.click('[data-sel="code-sessions"]');
  await page.waitForSelector('#view-comms', { state: 'hidden', timeout: 5000 }).catch(() => {});
  // Drive the block/estimate routes directly to confirm the web layer serves them
  // (the projection-gated UI panels depend on a projected run row, which the spec
  // does not stand up). These assertions are skipped on the node server.
  if (USE_RUST) {
    const blocks = await page.evaluate(async () => {
      const r = await fetch('/api/blocks?session=code-uiblk01');
      return r.ok ? await r.json() : null;
    });
    Array.isArray(blocks) && blocks.some((b) => b.name === 'identity')
      ? ok('blocks: /api/blocks lists the seeded durable block by name with its scope')
      : fail(`blocks: inspector route did not return the seeded block (${JSON.stringify(blocks)})`);

    // The block-inspector INLINE EDITOR (the documented follow-on to M4). A DURABLE
    // block (it carries an owner) is editable: a save POSTs `/api/blocks`, which —
    // through the origin_ok CSRF guard — shells `elanus block set ... --by ui` and
    // re-reads the persisted value. Drive the editor's exact POST and confirm the
    // new content persists on a fresh read.
    const durable = blocks.find((b) => b.name === 'identity');
    const editRes = await page.evaluate(async (blk) => {
      const r = await fetch('/api/blocks', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          session: 'code-uiblk01',
          name: blk.name,
          owner: blk.owner,
          scope: blk.scope,
          placement: blk.placement,
          priority: blk.priority,
          content: 'I am the worker. (edited from the UI)',
        }),
      });
      const body = await r.json().catch(() => ({}));
      return { status: r.status, body };
    }, durable);
    editRes.status === 200 && editRes.body && editRes.body.ok === true
      ? ok('blocks: editing a durable block POSTs through the origin_ok guard and succeeds')
      : fail(`blocks: durable-block edit did not succeed (${JSON.stringify(editRes)})`);

    // The edit persists: a fresh read shows the new content (the route re-read the
    // durable blocks and the write went through `elanus block set --by ui`).
    const afterEdit = await page.evaluate(async () => {
      const r = await fetch('/api/blocks?session=code-uiblk01');
      return r.ok ? await r.json() : null;
    });
    Array.isArray(afterEdit) && afterEdit.some((b) => b.name === 'identity' && /edited from the UI/.test(b.content))
      ? ok('blocks: the edited durable-block content persists on a fresh read')
      : fail(`blocks: the edit did not persist (${JSON.stringify(afterEdit)})`);

    // A cross-origin POST to the write route is refused by origin_ok (CSRF guard) —
    // the same boundary every /api/admin mutation enforces. The browser forbids
    // overriding the `Origin` header on a same-page fetch, so this is driven from
    // the Node side (a hostile-page / rebinding request): a request whose Origin
    // host is not local must be rejected (403), never written. (A LOCAL cross-port
    // Origin — the Vite dev proxy — is allowed; only a foreign host is refused.)
    const crossOriginRes = await fetch(`${BASE}/api/blocks`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'Origin': 'http://evil.example' },
      body: JSON.stringify({ session: 'code-uiblk01', name: 'identity', owner: 'code-agent', scope: 'session', content: 'pwned' }),
    });
    crossOriginRes.status === 403
      ? ok('blocks: a cross-origin write POST is refused by the origin_ok guard (403)')
      : fail(`blocks: cross-origin write was not refused (status ${crossOriginRes.status})`);
    // (The non-local-Host / DNS-rebinding leg of origin_ok can't be exercised here —
    // undici forbids overriding the Host header — so it's covered by the Rust
    // `origin_guard_allows_local_cross_port_refuses_foreign` unit test instead.)
    // And the rejected write left no trace — the content is still the edited value.
    const stillEdited = await page.evaluate(async () => {
      const r = await fetch('/api/blocks?session=code-uiblk01');
      const rows = r.ok ? await r.json() : [];
      return rows.find((b) => b.name === 'identity')?.content ?? '';
    });
    /edited from the UI/.test(stillEdited) && !/pwned/.test(stillEdited)
      ? ok('blocks: the refused cross-origin write did not mutate the block')
      : fail(`blocks: cross-origin write may have leaked through (${stillEdited})`);

    // An EPHEMERAL block is owner-less (computed each turn, never stored) and is NOT
    // editable: the write route rejects it. Seed unseen mail so the live inbox block
    // appears, confirm it is ephemeral with no owner, and confirm a write is refused.
    try {
      elanus('emit', 'in/agent/code-agent/code-uiblk01', '--payload', JSON.stringify({ prompt: 'a message for the worker' }));
    } catch (e) { fail(`seeding inbox mail failed: ${e.message ?? e}`); }
    const withEph = await page.evaluate(async () => {
      const r = await fetch('/api/blocks?session=code-uiblk01');
      return r.ok ? await r.json() : [];
    });
    const eph = withEph.find((b) => b.ephemeral === true);
    eph && !eph.owner
      ? ok('blocks: the ephemeral inbox block is owner-less and marked ephemeral (no editor)')
      : fail(`blocks: expected an owner-less ephemeral inbox block (${JSON.stringify(withEph.map((b) => ({ name: b.name, ephemeral: b.ephemeral, owner: b.owner })))})`);
    if (eph) {
      // Driven from the Node side so the intentional 400 doesn't surface as a
      // browser console.error (the UI never POSTs an ephemeral block — there is no
      // editor on it — so this probes the server guard directly).
      const ephWrite = await fetch(`${BASE}/api/blocks`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ session: 'code-uiblk01', name: eph.name, owner: eph.owner, scope: eph.scope, placement: eph.placement, content: 'should not persist' }),
      });
      ephWrite.status === 400
        ? ok('blocks: writing an ephemeral (owner-less) block is refused (400, not editable)')
        : fail(`blocks: an ephemeral-block write was not refused (status ${ephWrite.status})`);
    }

    const est = await page.evaluate(async () => {
      const r = await fetch('/api/estimate/code-uiest01');
      return r.ok ? await r.json() : null;
    });
    est && est.session === 'code-uiest01' && est.turns && est.turns.estimate === 8
      ? ok('estimate: /api/estimate returns the seeded estimate with a variance')
      : fail(`estimate: report route did not return the seeded estimate (${JSON.stringify(est)})`);

    const none = await page.evaluate(async () => {
      const r = await fetch('/api/estimate/code-noestimate');
      return { status: r.status, body: await r.json().catch(() => undefined) };
    });
    none.status === 200 && none.body === null
      ? ok('estimate: a session with no estimate returns 200 null (group omitted, no crash)')
      : fail(`estimate: no-estimate session did not return null (${JSON.stringify(none)})`);
  }
  await page.close();
}


// ── flow 12: model providers (model-providers M4) ────────────────────────────
// The named, encrypted credential vault surface: the /api/admin/providers
// endpoints (list/add/test/rm), the Providers page, the ModelField "set up a
// provider →" link, a provider-sourced model dropdown, and the no-warning-for-
// native fix. These routes are web.rs-only.
{
  const page = await newPage();
  await page.goto('/');
  await page.waitForSelector('#nav-agents', { timeout: 10000 });

  // -- the backend endpoints (driven directly, like the comms/blocks flow) --
  // add an api-key provider; the KEY rides the POST body and is piped on the
  // CLI's stdin by the backend — it must NEVER come back in list output.
  const addApi = await page.evaluate(async () => {
    const r = await fetch('/api/admin/providers', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'ui-deepseek', kind: 'apikey', wire: 'anthropic', base_url: 'https://api.deepseek.com/anthropic', key: 'sk-ui-secret-xyz' }),
    });
    return { status: r.status, body: await r.json().catch(() => ({})) };
  });
  addApi.status === 200 && addApi.body.ok === true
    ? ok('providers: POST add (api-key) succeeds')
    : fail(`providers: api-key add failed (${JSON.stringify(addApi)})`);

  // add a native-login provider (no secret).
  const addNative = await page.evaluate(async () => {
    const r = await fetch('/api/admin/providers', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'ui-claude-oauth', kind: 'native', tool: 'claude' }),
    });
    return { status: r.status, body: await r.json().catch(() => ({})) };
  });
  addNative.status === 200 && addNative.body.ok === true
    ? ok('providers: POST add (native login) succeeds')
    : fail(`providers: native add failed (${JSON.stringify(addNative)})`);

  // list returns metadata only; the secret is shown as a redaction, never the bytes.
  const listed = await page.evaluate(async () => {
    const r = await fetch('/api/admin/providers');
    return await r.json().catch(() => ({}));
  });
  const apiRow = (listed.providers ?? []).find((p) => p.name === 'ui-deepseek');
  const nativeRow = (listed.providers ?? []).find((p) => p.name === 'ui-claude-oauth');
  apiRow && nativeRow && apiRow.base_url === 'https://api.deepseek.com/anthropic'
    ? ok('providers: GET list returns both providers with metadata')
    : fail(`providers: list missing rows (${JSON.stringify(listed.providers)})`);
  apiRow && apiRow.secret && !/sk-ui-secret/.test(JSON.stringify(listed))
    ? ok('providers: the api key is redacted in list output (never the bytes)')
    : fail(`providers: the secret may have leaked into list output (${JSON.stringify(listed)})`);

  // test a native-login provider: it has no /models endpoint — the response is
  // explicitly NOT an error (native:true), which is what suppresses the warning.
  const testNative = await page.evaluate(async () => {
    const r = await fetch('/api/admin/providers/test?name=ui-claude-oauth');
    return await r.json().catch(() => ({}));
  });
  testNative.ok === true && testNative.native === true
    ? ok('providers: test on a native-login provider reports native (nothing to probe, not an error)')
    : fail(`providers: native test did not report native (${JSON.stringify(testNative)})`);

  // -- the Providers page (nav + list render) --
  await page.click('[data-sel="providers"]');
  await page.waitForSelector('#view-providers', { timeout: 5000 });
  ok('providers: the Providers page opens from the nav');
  await waitFor('providers: the page lists the added providers', async () =>
    page.evaluate(() => {
      const rows = [...document.querySelectorAll('#view-providers [data-provider]')].map((el) => el.getAttribute('data-provider'));
      return rows.includes('ui-deepseek') && rows.includes('ui-claude-oauth');
    }), 8000);

  // the page's test button surfaces the native result inline.
  await page.click('[data-test-provider="ui-claude-oauth"]');
  await waitFor('providers: the page test button surfaces a result', async () =>
    page.evaluate(() => {
      const el = document.querySelector('[data-test-result="ui-claude-oauth"]');
      return el && /native login/i.test(el.textContent);
    }), 8000);

  // -- the ModelField "set up a provider →" link (the literal #4 ask) --
  // The spec stack has no ambient model list, so the new-agent model field shows
  // the empty-list state with the link; clicking it navigates to the Providers page.
  await page.click('[data-sel="setup"]');
  await page.waitForSelector('#view-setup:not([hidden])', { timeout: 5000 });
  const naLink = await page.$('#view-setup [data-providers-link]');
  if (naLink) {
    ok('providers: the new-agent model field shows the "set up a provider →" link');
    await naLink.click();
    await page.waitForSelector('#view-providers', { timeout: 5000 });
    ok('providers: the link navigates to the Providers page');
  } else {
    fail('providers: the "set up a provider →" link is missing from the empty model field');
  }

  // -- the no-warning-for-native fix in an agent's configure provider section --
  await page.click('#nav-agents .nav-item');
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])', { timeout: 5000 });
  await waitForConfigureLoaded(page);
  // The configure provider dropdown is populated from the vault.
  await page.click('#cfg-section-advanced summary').catch(() => {});
  const hasProviderOpt = await page.evaluate(() => {
    const sel = document.querySelector('#cfg-provider');
    return sel ? [...sel.options].some((o) => o.value === 'ui-claude-oauth') : false;
  });
  hasProviderOpt
    ? ok('providers: the configure provider dropdown is sourced from the vault')
    : fail('providers: the configure provider dropdown did not list the native provider');
  // Selecting the native-login provider suppresses BOTH the model list and the
  // "provider list unavailable" warning (the real fix for the spurious warning).
  await page.selectOption('#cfg-provider', 'ui-claude-oauth').catch(() => {});
  const nativeNoWarn = await waitFor('providers: selecting a native-login provider shows no "unavailable" warning', async () =>
    page.evaluate(() => {
      const warn = document.querySelector('#cfg-section-model .cfg-field-warn');
      return !warn || !/provider list unavailable/i.test(warn.textContent || '');
    }), 5000);
  if (!nativeNoWarn) fail('providers: native-login still showed the spurious warning');

  // The dispatcher path depends on the *profile* naming the provider, not just
  // on the model id. This catches a prior regression where `profile get` omitted
  // `provider`, so configure reloads lost the selected provider and the next chat
  // fell back to stale inline `api_key_env` resolution (e.g. DEEPSEEK_API_KEY).
  await page.selectOption('#cfg-provider', 'ui-deepseek');
  await page.fill('#cfg-model', 'deepseek-v4-flash');
  const providerProfile = await page.$eval('#cfg-file', (el) => (el.textContent || '').replace(/\s+settings$/, '').trim());
  await page.click('#cfg-provider-save');
  await waitFor('providers: configure save persists the named provider in profile JSON', async () =>
    page.evaluate(async (profile) => {
      const r = await fetch(`/api/admin/profile?name=${encodeURIComponent(profile)}`);
      const j = await r.json().catch(() => ({}));
      return j.ok === true
        && j.profile?.provider === 'ui-deepseek'
        && j.profile?.model === 'deepseek-v4-flash'
        && /\bprovider\s*=\s*"ui-deepseek"/.test(j.toml || '');
    }, providerProfile), 8000);
  await page.reload();
  await page.waitForSelector('#nav-agents', { timeout: 10000 });
  await page.click('#nav-agents .nav-item');
  await page.waitForSelector('#agent-tabs', { state: 'visible' });
  await page.click('[data-tab="configure"]');
  await page.waitForSelector('#view-configure:not([hidden])', { timeout: 5000 });
  await waitForConfigureLoaded(page);
  await page.click('#cfg-section-advanced summary').catch(() => {});
  await waitFor('providers: configure reload shows the saved named provider', async () =>
    page.evaluate(() => {
      const provider = document.querySelector('#cfg-provider')?.value;
      const model = document.querySelector('#cfg-model')?.value;
      return provider === 'ui-deepseek' && model === 'deepseek-v4-flash';
    }), 8000);

  // -- rm --
  const removed = await page.evaluate(async () => {
    const r = await fetch('/api/admin/providers/rm', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'ui-deepseek' }),
    });
    const body = await r.json().catch(() => ({}));
    const list = await (await fetch('/api/admin/providers')).json().catch(() => ({}));
    return { ok: body.ok, names: (list.providers ?? []).map((p) => p.name) };
  });
  removed.ok && !removed.names.includes('ui-deepseek')
    ? ok('providers: POST rm deletes the provider')
    : fail(`providers: rm did not delete the provider (${JSON.stringify(removed)})`);

  // a cross-origin add POST is refused by the same origin_ok CSRF guard.
  const crossOrigin = await fetch(`${BASE}/api/admin/providers`, {
    method: 'POST', headers: { 'content-type': 'application/json', 'Origin': 'http://evil.example' },
    body: JSON.stringify({ name: 'evil', kind: 'native' }),
  });
  crossOrigin.status === 403
    ? ok('providers: a cross-origin add POST is refused by the origin_ok guard (403)')
    : fail(`providers: cross-origin add was not refused (status ${crossOrigin.status})`);

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

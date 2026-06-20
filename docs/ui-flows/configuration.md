# Web UI — configuration flow catalog

This is the canonical catalog of the elanus web UI's **configuration** and
**configuration-verification** user flows. It drives the web-qa (Playwright MCP)
harness: each flow is a self-contained, executable script — concrete gestures
keyed on real selectors, plus the observable a user (and an assertion) can hang
their confidence on.

The organizing question is not just "how do you set X" but **"how do you, by
your own observation, confirm X took effect?"** Every config write below pairs a
gesture with a *verification* — preferring **durable** feedback (a value that
survives a reload, a nav row that re-renders, a status banner that persists
across the pane re-render) over **flashing** feedback (a button label that
reverts after 1.4s, a note cleared by the next selection).

Two durability facts to keep in mind throughout:

- **`#setup-status`** (`.status-ok` / `.status-err`) is the durable banner in
  the add-ons pane. `loadSetup()` re-renders `#setup-kits`, `#setup-configs`,
  and `#setup-pending` after add, save, accept, or decline actions, which would
  wipe a button's transient label. The banner lives *outside* those containers,
  so it is the confirmation that survives the re-render. Assert on it, not on
  button labels.
- **`#cfg-note`** (configure) and **`#na-note`** (new agent) are transient. The
  truth of a config write is the **reloaded form value** and the **raw
  settings file**, not the note. Tests assert the note for liveness but verify
  persistence by reloading.

Selectors are kept stable in `ui/web/src/App.tsx`, with admin/history behavior
behind `ui/web/server.mjs`; existing working flows are in
`ui/web/test/ui.spec.mjs`.

---

## create

### create-1 — Create an agent from the setup pane

**Goal.** Make a new agent (a profile + mailbox) and land somewhere that proves
it exists.

**Preconditions.** Stack up; web UI loaded (boots on `#view-welcome`).

**Steps.**
1. Click `.nav-setup` (opens `#view-setup`), or `#nav-new-agent` /
   `#welcome-new` (both call `selectSetup()` then focus `#na-name`).
2. Type a name into `#na-name` (e.g. `kestrel`).
3. Optionally type a model into `#na-model` (datalist `#model-suggestions`;
   default is `claude-sonnet-4-6`).
4. Click `#na-create`.

**Observable expectation.** On success the app does NOT leave you on setup with a
flash — it **routes to the converse view for the new agent** (`selectAgent(name,
'converse')`). The compose input targets that agent, `#conv-configure-hint`
quietly says configure is available later, and the new agent appears as a
`#nav-agents .nav-item` (nav re-renders from disk). On failure, `#na-note` shows
the error (`name it first` for empty, else the server error / `unreachable`) and
you stay put.

**How to verify (Playwright sketch).**
```js
await page.click('.nav-setup');
await page.fill('#na-name', 'kestrel');
await page.click('#na-create');
// durable: landed in conversation with the new agent
await page.waitForSelector('#view-converse:not([hidden])');
await expect(page.locator('#compose-input')).toHaveAttribute('aria-label', 'message kestrel');
await expect(page.locator('#conv-configure-hint')).toContainText('Tune kestrel anytime in configure');
// durable: nav re-rendered with the new identity
await expect(page.locator('#nav-agents .nav-item')).toContainText('kestrel');
```

### create-2 — Create-agent input validation (empty name)

**Goal.** Confirm the UI refuses an unnamed agent without a server round-trip.

**Preconditions.** On `#view-setup`, `#na-name` empty.

**Steps.**
1. Leave `#na-name` blank.
2. Click `#na-create`.

**Observable expectation.** `#na-note` immediately reads `name it first`; no
network call fires, no nav change. This is transient text but it is the *only*
feedback for a rejected gesture, so the verification is simply that it appears
and nothing else changed.

**How to verify.**
```js
await page.fill('#na-name', '');
await page.click('#na-create');
await expect(page.locator('#na-note')).toHaveText('name it first');
await expect(page.locator('#view-setup')).toBeVisible(); // no navigation
```

---

## configure

### configure-1 — Edit identity via the form, then verify by reload

**Goal.** Set model / max run steps / working dir / package visibility through
the structured form and confirm every field stuck.

**Preconditions.** A profile-backed agent exists (e.g. `harrier`). Open it, then
the configure tab.

**Steps.**
1. Select the agent (`#nav-agents .nav-item` whose text contains the name).
2. Click tab `[data-tab="configure"]` → `#view-configure` shows.
3. **Wait for `loadConfigure` to finish** — the form is populated async; it is
   done when `#cfg-model` is non-empty (or `#cfg-note` says `no profile`).
   Filling before this races the on-disk default (e.g. haiku's `max_turns`).
4. Confirm `#cfg-section-essentials` is the first visible section and contains
   name, model, max run steps, autonomy, and working directory. Parent/path
   plumbing is not in that first section.
5. Fill `#cfg-model`, `#cfg-turns`, and `#cfg-workdir`.
6. Use the visible add-on toggle controls under `#cfg-package-configs` to change
   what the agent can use. `#cfg-include` / `#cfg-exclude` are hidden durable
   storage mirrors, not direct user controls.
7. Click `#cfg-save`.

**Observable expectation.**
- *Liveness (transient):* `#cfg-note` goes `saving…` → `saved — applies on the
  next run`.
- *Durable truth:* the values survive a full page reload. The visible
  `#cfg-turns` label is "max run steps" because this caps one activation's
  model/tool loop, not a conversation lifetime; until the agent-config
  migration, the saved key remains `model.max_turns`. `skills.include` /
  `skills.exclude` are sent as real arrays (comma-split, trimmed,
  empties dropped); an empty include is coerced to `['#']` (everything), and an
  empty exclude is *always sent* so clearing it actually clears. Re-opening
  configure after reload shows the same `#cfg-model` / `#cfg-turns`, and the
  hidden `#cfg-include` / `#cfg-exclude` mirrors match the visible add-on state.
- The header `#cfg-file` names the agent settings file the edit lands in
  (comments survive).
- The per-agent cost summary at `.cfg-cost-summary` names this agent's model,
  hard run-step ceiling, and autonomy. The model field includes an honest
  cost/performance hint (`cheap`, `balanced`, `powerful`, or `unknown`).
- `#cfg-autonomy-consequence` changes when `#cfg-autonomy` changes and states
  what the level lets agent-proposed setting changes do without asking.

**How to verify.** Save, then reload and re-read the fields — never trust the
note alone:
```js
await page.click('[data-tab="configure"]');
await page.waitForSelector('#view-configure:not([hidden])');
await waitForConfigureLoaded(page);            // #cfg-model non-empty
await expect(page.locator('#cfg-section-essentials')).toContainText(/name|model|max run steps|autonomy|working directory/);
await expect(page.locator('#cfg-section-essentials')).not.toContainText(/parent|prepend path|effective path/);
await page.fill('#cfg-model', 'claude-haiku-4-5-20251001');
await page.fill('#cfg-turns', '7');
await expect(page.locator('.cfg-cost-summary')).toContainText('7 run steps');
await expect(page.locator('.cfg-cost-summary')).toContainText(/cheap|unknown/);
await page.locator('.cfg-package-card[data-package="history"] .cfg-package-disable').click();
await expect(page.locator('#cfg-exclude')).toHaveValue(/history/);
await page.click('#cfg-save');
await expect(page.locator('#cfg-note')).toContainText('saved');     // liveness
await page.reload();                                                // DURABLE check
// re-select agent → configure → waitForConfigureLoaded, then:
await expect(page.locator('#cfg-model')).toHaveValue(/haiku/);
await expect(page.locator('#cfg-turns')).toHaveValue('7');
await expect(page.locator('#cfg-include')).toHaveValue(/#/);
await expect(page.locator('#cfg-exclude')).toHaveValue(/history/);
```

### configure-2 — Clearing the working directory (empty-string is a real save)

**Goal.** Confirm that emptying `#cfg-workdir` actually clears `sandbox.workdir`
rather than silently keeping the old value.

**Preconditions.** An agent whose profile has a non-empty `workdir`. On
configure, form loaded.

**Steps.**
1. Clear `#cfg-workdir` (leave it blank — placeholder reads `(harness root)`).
2. Click `#cfg-save`.

**Observable expectation.** Save succeeds (`#cfg-note` → `saved — applies on the
next run`). `sandbox.workdir` is the one key `prunedSet()` keeps even when empty,
so the clear is written. After reload, `#cfg-workdir` is empty.

**How to verify.**
```js
await page.fill('#cfg-workdir', '');
await page.click('#cfg-save');
await expect(page.locator('#cfg-note')).toContainText('saved');
await page.reload(); /* re-open configure, waitForConfigureLoaded */
await expect(page.locator('#cfg-workdir')).toHaveValue('');
```

### configure-3 — Traffic-only agent shows a guard, not a broken form

**Goal.** Confirm an agent that exists only as bus traffic (no settings file)
tells the user so instead of silently failing to save.

**Preconditions.** Select an agent that has no settings file (traffic-only),
open configure.

**Steps.**
1. Select the traffic-only agent → `[data-tab="configure"]`.

**Observable expectation.** `#cfg-note` reads `no settings file for <name> —
this agent only exists as traffic; create an agent here to configure it`.
`#cfg-toml` is empty. This is the durable state of the pane until settings exist.

**How to verify.**
```js
await page.click('[data-tab="configure"]');
await expect(page.locator('#cfg-note')).toContainText('only exists as traffic');
await expect(page.locator('#cfg-toml')).toHaveValue('');
```

### configure-4 — Rename the agent and watch the nav follow

**Goal.** Rename via `#cfg-agent` and confirm the new mailbox name is reflected
in the nav and the selection.

**Preconditions.** A settings-backed agent (e.g. `harrier`), configure loaded.

**Steps.**
1. Edit `#cfg-agent` to the new name (e.g. `falcon`).
2. Click `#cfg-save`.

**Observable expectation.** On a successful rename while that agent is selected,
the app calls `selectAgent(newName, 'configure')` — the **stage title
`#stage-title` updates to the new name**, the configure tab stays open, and after
a reload the new name appears as a `#nav-agents .nav-item` (and the old name no
longer does). `#cfg-note` flashes `saved — applies on the next run` but is then
cleared by the re-select, so it is NOT the thing to assert — assert the nav and
title. Renaming changes the mailbox to `in/agent/<name>` going forward; old-name
history stays in the ledger (per the in-pane note).

**How to verify.** Verify by the durable nav/title, not the note:
```js
await page.fill('#cfg-agent', 'falcon');
await page.click('#cfg-save');
await expect(page.locator('#stage-title')).toHaveText('falcon');   // selection followed
await page.reload();
await expect(page.locator('#nav-agents .nav-item')).toContainText('falcon');
// (the spec also asserts via a direct POST to /api/admin/agents/set because the
//  note timing is fragile — the nav/title is the user-facing durable proof.)
```

### configure-5 — Raw settings-file edit (the escape hatch) + reload-back verify

**Goal.** Edit the literal TOML and confirm it is written and re-read.

**Preconditions.** A settings-backed agent, configure loaded. Expand the
`<details>` whose summary reads `the raw settings file`.

**Steps.**
1. Open the raw details; `#cfg-toml` holds the current file text.
2. Edit `#cfg-toml`.
3. Click `#cfg-toml-save`.

**Observable expectation.** `#cfg-toml-note` shows `saving…` → `saved` (or `save
failed`). On success the handler calls `loadConfigure(sel.agent)`, which
**re-fetches the file and re-populates both the raw textarea AND the structured
form fields** — so a successful raw edit is visibly reflected back up into
`#cfg-model` / `#cfg-turns` / etc. without a manual reload. The deepest durable
proof is a page reload: `#cfg-toml` re-loads the saved bytes.

**How to verify.**
```js
await page.click('text=the raw settings file');          // expand <details>
const toml = await page.inputValue('#cfg-toml');
await page.fill('#cfg-toml', toml + '\n# qa-marker\n');
await page.click('#cfg-toml-save');
await expect(page.locator('#cfg-toml-note')).toHaveText('saved'); // liveness
// durable: reload re-reads the file
await page.reload(); /* re-open configure + raw details */
await expect(page.locator('#cfg-toml')).toContainText('# qa-marker');
```

### configure-6 — Package visibility lives in configure; add-ons are shared

**Goal.** Document that the package list gives each agent a labeled
enable/disable control backed by `#cfg-exclude`, while add-ons themselves are
managed from the shared add-ons view.

**Preconditions.** Configure loaded.

**Steps.**
1. Expand a package row under `#cfg-package-configs`.
2. Click its `disable` button and confirm `#cfg-exclude` includes the package
   name.
3. Click its `enable` button and confirm `#cfg-exclude` no longer includes the
   package name.

**Observable expectation.** Same durable proof as configure-1: the
comma-separated lists round-trip through reload (`include` defaults to `#` when
emptied; `exclude` clears when emptied).

**How to verify.** Covered by the configure-1 reload assertions for `#cfg-include`
/ `#cfg-exclude`.

### configure-7 — Advanced context parameters stay out of the default path

**Goal.** Keep legacy `[vars]` editable without presenting arbitrary key/value
pairs as normal agent configuration.

**Preconditions.** Configure loaded.

**Steps.**
1. Confirm the configure index has no `vars` entry and there is no standalone
   `#cfg-section-vars`.
2. Open `#cfg-section-advanced`, then `#cfg-section-raw` → `advanced context
   parameters`.
3. Add a key/value row in `#cfg-vars`.
4. Click the main `#cfg-save` button.

**Observable expectation.** The controls are labeled as advanced context /
template parameters, not agent identity. Save succeeds through the normal form
save, reload preserves the row, and the raw settings file contains the matching
stored value.

### configure-8 — Package settings declare scope and effective value

**Goal.** Package rows expose typed, documented add-on context parameters
without hiding whether a save changes one agent or every agent.

**Preconditions.** Configure loaded, package tree rendered.

**Steps.**
1. Expand the `window` package row under `#cfg-package-configs`.
2. Click its `settings` button.
3. Inspect the `Window rows` control.
4. Save `70` with the `every agent` button.
5. Save `60` with the `this agent` button for the selected agent.

**Observable expectation.** Before expanding the package row, its summary says
settings can be saved for every agent or for the selected agent only. The
opened setting row says it comes from `agent context window`, shows
`type: number`, includes the manifest help text, and renders the numeric default
`80`. It also says the edited value is the shared default for every agent and
shows `effective here` plus the source (`from the package default`, `from the
shared default`, or `overridden here for <agent>`). The `every agent` button
calls `POST /api/admin/configs/set` and backend logs show `elanus config set`.
The `this agent` button calls `POST /api/admin/agents/set` and backend logs show
`elanus profile set <profile> vars.<key>=...`. After reload, the selected agent
shows the one-agent override, while a second agent with the package sees the
shared value. The same declared parameter is also available in the matching
context-step tile, but that tile says it applies to the selected agent only;
legacy raw values remain available only through configure-7's advanced context
parameters.

**How to verify.** Covered by `ui/web/test/ui.spec.mjs`.

### configure-9 — Context program policy is an agent setting

**Goal.** Configure exposes the agent's context-program policy without making
the user edit raw TOML for the common fields.

**Preconditions.** Configure loaded.

**Steps.**
1. Open `#cfg-section-advanced`.
2. Inspect `#cfg-section-context`.
3. Set `program` to `default`.
4. Set `max context ms` to `12000`.
5. Inspect `#cfg-context-chain` and find the `window/window` context-step tile.
6. Change its `timeout ms` value to `9000`.
7. Change its declared `Window rows` setting to `60`.
8. Use the tile move controls when more than one context step is visible.
9. Click the main `#cfg-save` button.

**Observable expectation.** Save succeeds, reload preserves both controls, and
the raw settings file contains `[context] max_total_ms = 12000` plus a
`context.stage` array entry for `window/window` with `timeout_ms = 9000`. The
edited `Window rows` tile setting is labeled as applying to this agent only and
persists as `vars.window_rows = "60"` for this agent. The UI presents context
steps as an ordered chain, not as a singleton object.

**How to verify.** Covered by `ui/web/test/ui.spec.mjs`.

**How to verify.**
```js
await expect(page.locator('.cfg-index')).not.toContainText(/\bvars\b/i);
await expect(page.locator('#cfg-section-vars')).toHaveCount(0);
await page.click('#cfg-section-advanced > summary');
await page.click('text=advanced context parameters');
await page.fill('#cfg-vars .cfg-var-key', 'window_rows');
await page.fill('#cfg-vars .cfg-var-value', '50');
await page.click('#cfg-save');
await expect(page.locator('#cfg-note')).toContainText('saved');
await page.reload(); /* re-open configure + advanced context parameters */
await expect(page.locator('#cfg-vars .cfg-var-key')).toHaveValue('window_rows');
await expect(page.locator('#cfg-vars .cfg-var-value')).toHaveValue('50');
await expect(page.locator('#cfg-toml')).toContainText('[vars]');
```

---

## add-ons

### add-ons-1 — Browse the catalog and expand details

**Goal.** See the available add-ons and read what one would do before adding it.

**Preconditions.** `#view-setup` open; at least one add-on resolvable under
`<root>/kits` (seeded: dev / core / funnel).

**Steps.**
1. Click `.nav-setup`.
2. In `#setup-kits`, find a `.setup-kit` whose `.setup-kit-name` matches (e.g.
   `dev`).
3. Click that row's details button (`button.ghost`).

**Observable expectation.** `#setup-kits` lists a non-installable
`#coding-agent-entry` first, explaining in future-tense language that
Codex/Claude Code support is coming and that the value will be sandbox,
recording, and cost control. It must not claim the integration is configured
today. Other add-ons list with name + hook;
an already-added row carries a `.badge` reading `installed` and its action button
reads `add again`. Clicking details toggles a `.setup-readme <pre>` from hidden
to shown, lazily fetching the text (shows `fetching...` then content). Empty
catalog shows a dim product-language note.

**How to verify.**
```js
await page.click('.nav-setup');
await page.waitForSelector('#view-setup:not([hidden])');
await expect(page.locator('#coding-agent-entry')).toContainText(/Codex|Claude Code/);
await expect(page.locator('#coding-agent-entry')).toContainText(/sandbox|recording|cost control/);
await expect(page.locator('#coding-agent-entry')).toContainText(/coming|not configured/);
await expect(page.locator('#setup-kits')).toContainText(/dev|core|funnel/);
const devRow = page.locator('.setup-kit', { hasText: 'dev' });
await devRow.locator('button.ghost').click();
await expect(devRow.locator('.setup-readme')).toBeVisible();
await expect(devRow.locator('.setup-readme')).not.toBeEmpty();
```

### add-ons-2 — Add once; verify by durable banner + installed settings

**Goal.** Add an add-on as one human action and confirm it took without relying
on a button label that the re-render destroys.

**Preconditions.** `#view-setup` open, target `.setup-kit` visible.

**Steps.**
1. In the target `.setup-kit`, click its add button (`button:not(.ghost)`,
   labeled `add` or `add again`).
2. The button shows `adding...` and disables, then `loadSetup()` re-renders the
   whole pane.

**Observable expectation.** The transient button label is destroyed by the
re-render. The durable confirmation is **`#setup-status`** going `.status-ok`
with text `added <name>.` (or `.status-err` on failure). Simultaneously
`#setup-configs` shows the installed add-ons, including typed settings controls
for packages that declare configurable values. The banner persists because it
lives outside the re-rendered lists.

**How to verify.**
```js
const devRow = page.locator('.setup-kit', { hasText: 'dev' });
await devRow.locator('button:not(.ghost)').click();
await expect(page.locator('#setup-status')).toBeVisible();
await expect(page.locator('#setup-status')).toHaveClass(/status-ok/);
await expect(page.locator('#setup-status')).toContainText('added dev');
await expect(page.locator('#setup-configs')).toContainText(/git-protect|window|recent-history/i);
```

### add-ons-3 — Save typed package settings and read them back

**Goal.** Give shared package configuration a visible home and prove writes
survive a reload.

**Preconditions.** An add-on is installed and visible in `#setup-configs`.

**Steps.**
1. In the installed add-on card, click `settings`.
2. Edit a typed setting row rendered from the add-on description, such as
   `Window rows` under `window`.
3. Click that row's `save` button.
3. Expand `current settings`.

**Observable expectation.** The card says these settings apply to every agent
that uses the add-on. The row uses the declared input type from the add-on
description rather than a raw TOML value box. The inline note reads
`saved and reloaded`; expanding current settings fetches the raw TOML from
`elanus config list <package>` and shows the saved key/value. The backend log
shows `elanus config set ...`, and the change is committed on `config/live`.

**How to verify.**
```js
const card = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'window' });
await card.locator('button', { hasText: 'settings' }).click();
await card.locator('.cfg-config-row', { hasText: 'Window rows' }).locator('input[type="number"]').fill('72');
await card.locator('button', { name: 'save window.window_rows for every agent' }).click();
await expect(card).toContainText('saved and reloaded');
await card.locator('summary', { hasText: 'current settings' }).click();
await expect(card.locator('pre')).toContainText('window_rows = 72');
```

### add-ons-4 — Turn off a linked kit

**Goal.** Give an installed linked kit a reversible off switch without claiming
that review records or copied package files were erased.

**Preconditions.** A linked kit such as `dev` has been added and one of its
packages, such as `git-protect`, appears in `#setup-configs`.

**Steps.**
1. In the installed package card, click `turn off`.
2. Read the confirmation.
3. Confirm `turn off <kit>`.

**Observable expectation.** The confirmation says this removes the kit from this
installation's add-on path and that the review record stays. On success,
`#setup-status` says `turned off <kit>...`, the package disappears from
`#setup-configs` after reload, and the backend log shows `elanus kit unlink
<kit>`. This is a disable via unlink, not a hard uninstall.

Copied kits do not show the `turn off` button because `kit unlink` cannot remove
copied package files. Their cards say removal is not supported here yet.

**How to verify.**
```js
const card = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'git-protect' });
await card.locator('button', { hasText: 'turn off' }).click();
await expect(card).toContainText('review record stays');
await card.locator('.setup-confirm button').click();
await expect(page.locator('#setup-status')).toContainText('turned off dev');
await expect(page.locator('#setup-configs')).not.toContainText('git-protect');
// also assert ELANUS_WEB_LOG contains: elanus kit unlink dev
const copied = page.locator('#setup-configs .setup-pending-pkg', { hasText: 'harness-doctrine' });
await expect(copied).toContainText('Copied into this installation');
await expect(copied.locator('button', { hasText: 'turn off' })).toHaveCount(0);
```

---

## agent requests

### requests-1 — Resting state

**Goal.** Confirm there is no intimidating technical queue when no agent has
asked for a settings change.

**Observable expectation.** `#setup-pending` reads `no agent requests`.

### requests-2 — Accept or decline an agent settings change

**Goal.** Show an agent-started config proposal as a plain request.

**Preconditions.** `elanus config proposals` returns at least one proposal.

**Steps.**
1. Open `.nav-setup`.
2. In `#setup-pending`, read the request card: `<agent> wants to change settings`.
3. Optionally click `show change` to reveal the diff.
4. Click `accept` or `decline`.

**Observable expectation.** Accept calls `elanus config accept <id>` and shows
`accepted the change.`; decline calls `elanus config decline <id>` and shows
`declined the change.` The card disappears after the re-render.

**How to verify.**
```js
const card = page.locator('#setup-pending .setup-pending-pkg').first();
await expect(card).toContainText(/wants to change settings/);
await card.locator('button', { hasText: 'show change' }).click();
await expect(card.locator('pre')).toContainText(/diff --git/);
await card.locator('button', { hasText: 'accept' }).click();
await expect(page.locator('#setup-status')).toContainText('accepted the change');
```

---

## history

### history-1 — Transcript view degraded state

**Goal.** Confirm that whether transcripts are browsable is observable, and that
the unavailable state is surfaced instead of silently breaking.

**Preconditions.** History view NOT running.

**Steps.**
1. Select an agent → tab `[data-tab="sessions"]` (`#view-sessions`).
2. Observe `#sessions-pane`; observe footer `#history-hint`.

**Observable expectation.** `#sessions-pane` shows the live-only note
(`transcripts unavailable — live view only.` plus the add-ons hint). After the probe settles,
`#history-hint` becomes visible (its `hidden` flips off via `setHistoryOk(false)`
on a 503/504 from `/api/history`). The welcome `#welcome-hint` likewise says
transcripts are unavailable until the history view is on. This is the durable
degraded state.

**How to verify.**
```js
await page.click('[data-tab="sessions"]');
await page.waitForSelector('#view-sessions:not([hidden])');
await expect(page.locator('#sessions-pane')).toContainText(/transcripts unavailable|live view only/i);
await expect(page.locator('#history-hint')).toBeVisible(); // hidden flipped off
```

### history-2 — Sessions view populates once transcripts are running (healed state)

**Goal.** Confirm that after the transcript view is running, the sessions view
lists recorded sessions for the agent.

**Preconditions.** The history view is serving `/api/history`; the agent has at
least one recorded session.

**Steps.**
1. Select the agent → `[data-tab="sessions"]`.
2. `loadSessions(agent)` queries `kind=sessions`; on success a `.sess-list`
   table renders (`.sess-row` per session: id, first/last ts, msgs, events).
3. Click a `.sess-row` → `openTranscript` renders the `.tr-feed`.

**Observable expectation.** The live-only note is GONE; `#sessions-pane` holds a
`.sess-list` with rows (or `no recorded sessions for this agent yet.` if the
agent truly has none). `#history-hint` is hidden (`setHistoryOk(true)`). The same
selectors that showed the degraded note in history-1 now show data.

**How to verify.**
```js
await page.click('[data-tab="sessions"]');
await page.waitForSelector('#view-sessions:not([hidden])');
await expect(page.locator('#history-hint')).toBeHidden();           // healed
await expect(page.locator('#sessions-pane .sess-list, #sessions-pane'))
  .toContainText(/session|no recorded sessions/i);
```

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

- **`#setup-status`** (`.status-ok` / `.status-err`) is the NEW durable banner in
  the kits/review pane. `loadSetup()` re-renders `#setup-kits` and
  `#setup-pending` on every stage/approve, which would wipe a button's transient
  `staged ✓`. The banner lives *outside* those two containers, so it is the
  confirmation that survives the re-render. Assert on it, not on button labels.
- **`#cfg-note`** (configure) and **`#na-note`** (new agent) are transient. The
  truth of a config write is the **reloaded form value** and the **raw
  profile.toml**, not the note. Tests assert the note for liveness but verify
  persistence by reloading.

Selectors are taken verbatim from `ui/web/public/index.html` and the behavior
from `ui/web/public/app.js`; existing working flows are in
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
flash — it **routes to the configure tab of the new agent** (`selectAgent(name,
'configure')`) and writes a durable, explanatory line into `#cfg-note`:
`created <name> — set its identity below, then converse`. The new agent also
appears as a `#nav-agents .nav-item` (nav re-renders from disk). On failure,
`#na-note` shows the error (`name it first` for empty, else the server error /
`unreachable`) and you stay put.

**How to verify (Playwright sketch).**
```js
await page.click('.nav-setup');
await page.fill('#na-name', 'kestrel');
await page.click('#na-create');
// durable: landed on configure for the new agent
await page.waitForSelector('#view-configure:not([hidden])');
await expect(page.locator('#cfg-note')).toContainText('created kestrel');
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

**Goal.** Set model / max turns / working dir / skills include / skills exclude
through the structured form and confirm every field stuck.

**Preconditions.** A profile-backed agent exists (e.g. `harrier`). Open it, then
the configure tab.

**Steps.**
1. Select the agent (`#nav-agents .nav-item` whose text contains the name).
2. Click tab `[data-tab="configure"]` → `#view-configure` shows.
3. **Wait for `loadConfigure` to finish** — the form is populated async; it is
   done when `#cfg-model` is non-empty (or `#cfg-note` says `no profile`).
   Filling before this races the on-disk default (e.g. haiku's `max_turns`).
4. Fill `#cfg-model`, `#cfg-turns`, `#cfg-workdir`, `#cfg-include`,
   `#cfg-exclude`.
5. Click `#cfg-save`.

**Observable expectation.**
- *Liveness (transient):* `#cfg-note` goes `saving…` → `saved — applies on the
  next run`.
- *Durable truth:* the values survive a full page reload. `skills.include` /
  `skills.exclude` are sent as real arrays (comma-split, trimmed,
  empties dropped); an empty include is coerced to `['#']` (everything), and an
  empty exclude is *always sent* so clearing it actually clears. Re-opening
  configure after reload shows the same `#cfg-model` / `#cfg-turns` /
  `#cfg-include` / `#cfg-exclude`.
- The header `#cfg-file` shows `profiles/<profile>/profile.toml` — the file the
  edit lands in (comments survive).

**How to verify.** Save, then reload and re-read the fields — never trust the
note alone:
```js
await page.click('[data-tab="configure"]');
await page.waitForSelector('#view-configure:not([hidden])');
await waitForConfigureLoaded(page);            // #cfg-model non-empty
await page.fill('#cfg-model', 'claude-haiku-4-5-20251001');
await page.fill('#cfg-turns', '7');
await page.fill('#cfg-include', '#');
await page.fill('#cfg-exclude', 'notes');
await page.click('#cfg-save');
await expect(page.locator('#cfg-note')).toContainText('saved');     // liveness
await page.reload();                                                // DURABLE check
// re-select agent → configure → waitForConfigureLoaded, then:
await expect(page.locator('#cfg-model')).toHaveValue(/haiku/);
await expect(page.locator('#cfg-turns')).toHaveValue('7');
await expect(page.locator('#cfg-include')).toHaveValue(/#/);
await expect(page.locator('#cfg-exclude')).toHaveValue(/notes/);
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

### configure-3 — No-profile agent shows a guard, not a broken form

**Goal.** Confirm an agent that exists only as bus traffic (no profile file)
tells the user so instead of silently failing to save.

**Preconditions.** Select an agent that has no `profile.toml` (traffic-only),
open configure.

**Steps.**
1. Select the traffic-only agent → `[data-tab="configure"]`.

**Observable expectation.** `#cfg-note` reads `no profile file for <name> — this
agent only exists as traffic; create a profile to configure it`. `#cfg-toml` is
empty. This is the durable state of the pane until a profile exists.

**How to verify.**
```js
await page.click('[data-tab="configure"]');
await expect(page.locator('#cfg-note')).toContainText('only exists as traffic');
await expect(page.locator('#cfg-toml')).toHaveValue('');
```

### configure-4 — Rename the agent and watch the nav follow

**Goal.** Rename via `#cfg-agent` and confirm the new mailbox name is reflected
in the nav and the selection.

**Preconditions.** A profile-backed agent (e.g. `harrier`), configure loaded.

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

### configure-5 — Raw profile.toml edit (the escape hatch) + reload-back verify

**Goal.** Edit the literal TOML and confirm it is written and re-read.

**Preconditions.** A profile-backed agent, configure loaded. Expand the
`<details>` whose summary reads `the raw profile.toml`.

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
await page.click('text=the raw profile.toml');           // expand <details>
const toml = await page.inputValue('#cfg-toml');
await page.fill('#cfg-toml', toml + '\n# qa-marker\n');
await page.click('#cfg-toml-save');
await expect(page.locator('#cfg-toml-note')).toHaveText('saved'); // liveness
// durable: reload re-reads the file
await page.reload(); /* re-open configure + raw details */
await expect(page.locator('#cfg-toml')).toContainText('# qa-marker');
```

### configure-6 — Skills include/exclude live in configure but are root-managed

**Goal.** Document that per-agent `#cfg-include` / `#cfg-exclude` set this
agent's *visibility* of root-wide kits/grants, and where to actually manage the
kits.

**Preconditions.** Configure loaded.

**Steps.**
1. Read the `kits & capabilities` block note: kits/grants are root-wide, managed
   under `⚒ kits & review`; this agent's view is filtered by skill visibility.
2. Set `#cfg-include` (placeholder `# (everything)`) / `#cfg-exclude`
   (placeholder `(nothing)`), click `#cfg-save`.

**Observable expectation.** Same durable proof as configure-1: the
comma-separated lists round-trip through reload (`include` defaults to `#` when
emptied; `exclude` clears when emptied). The note steers the user to
`.nav-setup` for the actual capability grants.

**How to verify.** Covered by the configure-1 reload assertions for `#cfg-include`
/ `#cfg-exclude`.

---

## kits

### kits-1 — Browse the kit catalog and expand a readme

**Goal.** See the available kits and read what one would do before staging.

**Preconditions.** `#view-setup` open; at least one kit resolvable under
`<root>/kits` (seeded: dev / core / funnel).

**Steps.**
1. Click `.nav-setup`.
2. In `#setup-kits`, find a `.setup-kit` whose `.setup-kit-name` matches (e.g.
   `dev`).
3. Click that kit's readme button (`button.ghost` in the row).

**Observable expectation.** `#setup-kits` lists each kit with name + hook; an
already-applied kit carries a `.badge` reading `installed` and its stage button
reads `stage again`. Clicking readme toggles a `.setup-readme <pre>` from hidden
to shown, lazily fetching the text (shows `fetching…` then content). Empty
catalog shows a dim note explaining why.

**How to verify.**
```js
await page.click('.nav-setup');
await page.waitForSelector('#view-setup:not([hidden])');
await expect(page.locator('#setup-kits')).toContainText(/dev|core|funnel/);
// expand the dev readme
const devRow = page.locator('.setup-kit', { hasText: 'dev' });
await devRow.locator('button.ghost').click();
await expect(devRow.locator('.setup-readme')).toBeVisible();
await expect(devRow.locator('.setup-readme')).not.toBeEmpty();
```

### kits-2 — Stage a kit; verify by the durable banner + pending queue

**Goal.** Stage a kit's grants as *pending* and confirm it took, without relying
on a button label that the re-render destroys.

**Preconditions.** `#view-setup` open, target `.setup-kit` visible.

**Steps.**
1. In the target `.setup-kit`, click its stage button (`button:not(.ghost)`,
   labeled `stage` or `stage again`).
2. The button shows `staging…` and disables, then `loadSetup()` re-renders the
   whole pane.

**Observable expectation (durable-vs-flash is the whole point).** The stage
button's `staging…` is destroyed by the re-render — do NOT assert on it.
The durable confirmation is **`#setup-status`** going `.status-ok` with text
`✓ staged <kit> — its grants are in "pending review" below; approve to commit.`
(or `.status-err` `✕ couldn't stage …` on failure). Simultaneously
`#setup-pending` fills with `.setup-pending-pkg` cards, one per package, each
listing `.setup-grant` lines and an approve button. The banner persists across
subsequent re-renders because it lives outside `#setup-kits` / `#setup-pending`.

**How to verify.** Assert the durable banner AND the populated queue:
```js
const devRow = page.locator('.setup-kit', { hasText: 'dev' });
await devRow.locator('button:not(.ghost)').click();
await expect(page.locator('#setup-status')).toBeVisible();
await expect(page.locator('#setup-status')).toHaveClass(/status-ok/);
await expect(page.locator('#setup-status')).toContainText('staged');
await expect(page.locator('#setup-pending')).toContainText(/approve|git-protect/i);
```

---

## grants

### grants-1 — Approve a pending package; verify by the durable banner + drain

**Goal.** Commit a staged grant (decided_by=ui) and confirm it left the queue.

**Preconditions.** At least one `.setup-pending-pkg` in `#setup-pending` (run
kits-2 first).

**Steps.**
1. In a pending card, click the approve button (`#setup-pending button`, labeled
   `approve <pkg>`).
2. It shows `approving…` then `loadSetup()` re-renders.

**Observable expectation.** Like staging, the button's transient label is wiped
by the re-render; the durable proof is **`#setup-status`** → `.status-ok`
`✓ approved <pkg> — grant committed (decided_by=ui).` (or `.status-err`). The
approved package's card disappears from `#setup-pending`; once the last one is
approved, `#setup-pending` shows the dim resting note
`nothing pending — the ledger is at rest`. The decision is the same gesture as
the terminal — each card shows a copyable `.setup-cmd` `elanus approve <pkg>`
(click copies to clipboard).

**How to verify.**
```js
const card = page.locator('.setup-pending-pkg').first();
const pkg = await card.locator('.setup-kit-name').textContent();
await card.locator('button').click();                 // approve
await expect(page.locator('#setup-status')).toHaveClass(/status-ok/);
await expect(page.locator('#setup-status')).toContainText('approved');
// durable drain: that package's card is gone
await expect(page.locator('.setup-pending-pkg', { hasText: pkg })).toHaveCount(0);
```

### grants-2 — Drain the whole pending queue to the resting state

**Goal.** Approve every pending package and confirm the queue reaches its
empty/resting marker (a staged kit may produce several packages).

**Preconditions.** One or more pending cards.

**Steps.**
1. Repeatedly: query `#setup-pending button`; if present, click it and let
   `loadSetup()` re-render; if absent, read `#setup-pending` text.

**Observable expectation.** Each approval re-renders the pane and drops one card;
when none remain, `#setup-pending` reads `nothing pending — the ledger is at
rest`. Always query a fresh `#setup-pending button` per iteration — handles go
stale across the re-render (this is why the spec uses `page.click` not a held
element ref).

**How to verify.**
```js
for (;;) {
  const btn = page.locator('#setup-pending button').first();
  if (!(await btn.count())) break;
  await btn.click();
  await page.waitForTimeout(400); // let loadSetup re-render
}
await expect(page.locator('#setup-pending')).toContainText(/nothing pending|at rest/i);
```

### grants-3 — The terminal-equivalence affordance (copyable command)

**Goal.** Confirm the UI advertises that approving is identical to the CLI and
offers the exact command.

**Preconditions.** A pending card present.

**Steps.**
1. Read the pending-review block note: `approving here is the same gesture as the
   terminal (ledger trail: decided_by=ui)`.
2. Click the `.setup-cmd` `<code>` in a card.

**Observable expectation.** `.setup-cmd` reads `elanus approve <pkg>`, has the
title `the same gesture from a terminal — click to copy`, and clicking writes it
to the clipboard. (Clipboard read may be unavailable headless — assert the text
and title rather than the paste.)

**How to verify.**
```js
const cmd = page.locator('.setup-cmd').first();
await expect(cmd).toHaveText(/^elanus approve /);
await expect(cmd).toHaveAttribute('title', /click to copy/);
```

---

## history

### history-1 — History package drives the sessions view (degraded state)

**Goal.** Confirm that whether transcripts are browsable is an *observable*
consequence of the history package being approved/running, and that its absence
is surfaced — not silent.

**Preconditions.** History package NOT running (fresh root, before the package is
approved).

**Steps.**
1. Select an agent → tab `[data-tab="sessions"]` (`#view-sessions`).
2. Observe `#sessions-pane`; observe footer `#history-hint`.

**Observable expectation.** `#sessions-pane` shows the live-only note (`history
package not running — live view only.` plus `approve the history package under
"kits & review" to browse transcripts.`). After the probe settles,
`#history-hint` becomes visible (its `hidden` flips off via `setHistoryOk(false)`
on a 503/504 from `/api/history`). The welcome `#welcome-hint` likewise reads
`transcripts are off until you approve the history package`. This is the durable
degraded state.

**How to verify.**
```js
await page.click('[data-tab="sessions"]');
await page.waitForSelector('#view-sessions:not([hidden])');
await expect(page.locator('#sessions-pane')).toContainText(/live view only|history/i);
await expect(page.locator('#history-hint')).toBeVisible(); // hidden flipped off
```

### history-2 — Sessions view populates once history is running (healed state)

**Goal.** Confirm that after approving/running the history package, the sessions
view lists recorded sessions for the agent — the verification that the config
(grant) took effect end-to-end.

**Preconditions.** History package approved (grants flows) and the package
serving `/api/history`; the agent has at least one recorded session.

**Steps.**
1. Select the agent → `[data-tab="sessions"]`.
2. `loadSessions(agent)` queries `kind=sessions`; on success a `.sess-list`
   table renders (`.sess-row` per session: id, first/last ts, msgs, events).
3. Click a `.sess-row` → `openTranscript` renders the `.tr-feed`.

**Observable expectation.** The live-only note is GONE; `#sessions-pane` holds a
`.sess-list` with rows (or `no recorded sessions for this agent yet.` if the
agent truly has none). `#history-hint` is hidden (`setHistoryOk(true)`). This
healed state is the cross-package proof that the grant approval took — the same
selectors that showed the degraded note in history-1 now show data.

**How to verify.**
```js
await page.click('[data-tab="sessions"]');
await page.waitForSelector('#view-sessions:not([hidden])');
await expect(page.locator('#history-hint')).toBeHidden();           // healed
await expect(page.locator('#sessions-pane .sess-list, #sessions-pane'))
  .toContainText(/session|no recorded sessions/i);
```

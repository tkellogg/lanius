# Handoff: configuration UX — altitude and scope

Status: M1 implemented and verified; M2-M4 planned. Intended implementer: Codex.

This handoff asks for a focused product-layer pass on the web UI's
**configuration** surfaces — the setup view (instance config) and the configure
tab (agent config) — plus the getting-started landing that leads into them. It is
not a kernel change and not the Codex/Claude-Code integration; it is making the
config surfaces behave the way a sensible person expects.

The whole pass has one through-line: **altitude**. The config UI today treats a
person's simplest first intention and the deepest builder knob as equals, and it
treats an instance-wide setting and a single-agent setting as identical. Both are
altitude problems. Fix altitude and one pane serves all four characters
(docs/journeys/characters.md) without any of them having to think hard.

## Read these first

Design intent (read before touching anything):

- `docs/layering.md` — the kernel/building-block/product split and the hard rule:
  internal vocabulary (grant, stage, topic, mailbox, ledger, pending) must never
  appear in the product interface. Translate at the boundary every time.
- `docs/config.md` — the configuration model: packages vs configuration,
  proposals as Git branches, autonomy levels, the protected stdlib kit, and the
  "What the interface shows" section that governs the config surface.
- `docs/journeys/06-configuration.md` — the character-level *why* for this whole
  handoff. Each milestone below maps to a journey in that file.
- `docs/journeys/characters.md`, `01-setup.md`, `03-cost-visibility.md`,
  `04-risk-and-trust.md` — Lily, Daniel, Ganesh, Tim; cost-language rules; the
  cheap risk/trust surfaces Ganesh needs.

The surfaces you'll change:

- `ui/web/src/App.tsx` — the entire dashboard. Key pieces:
  - `SetupView` (instance config: catalog, installed capabilities, cost, trust,
    agent requests) and its children `SetupKit`, `SetupPackageConfig`,
    `ProposalCard`.
  - `ConfigureView` (agent config: agent / model / context / sandbox / packages /
    throttle / raw) and its children `PackageTree`, `PackageCard`,
    `ContextStageTile`, `ConfigInputRow`, `KitModal`/`KitAddRow`.
  - `createAgent` (the guided-wizard create path) and `loadConfigure` /
    `saveConfigure`.
  - Helpers: `costSummary`, `riskBadges`, `grantState`, `declaredConfigParams`,
    `prunedSet`.
- `ui/web/server.mjs` — admin endpoints. The ones that matter here:
  `POST /api/admin/configs/set` → `elanus config set` (shared/instance write),
  `POST /api/admin/agents/set` → profile write (per-agent), `kits/add`,
  `GET/PUT /api/admin/profile`, `GET /api/status`.
- `ui/web/src/api.ts` — the relay helpers.

Proof and conventions (you must keep these honest):

- `docs/ui-flows/configuration.md` — the **canonical flow catalog**. Any flow you
  change must be updated here, and new behavior gets a new flow entry with a
  durable observable.
- `ui/web/test/ui.spec.mjs` — the Playwright regression suite. Extend it; don't
  break it.
- `.claude/skills/web-qa/SKILL.md` — how to QA the real UI against an isolated
  live stack. Verify durable state (reloaded values, nav rows, persisted banners,
  backend log lines), not flashing notes.

If a change implies a precedence or data-model question (especially M1's
"effective value"), confirm it in the Rust before building UI on top of an
assumption: `src/config_repo.rs`, `src/configcli.rs`, `src/context.rs`,
`src/exec.rs`, `src/profile.rs`.

## The findings (the why, condensed)

1. **Config has no honest sense of scope (highest leverage).** A package
   "setting" changed from inside an agent's configure tab is written via
   `POST /api/admin/configs/set` → `elanus config set` → the shared config repo,
   so it is **instance-wide** (`App.tsx` `PackageCard.saveRow`, and the
   setup-pane `SetupPackageConfig.save`; relay at `server.mjs` `configs/set` →
   `cli(['config','set',…])`). The context-stage tile an inch away writes `vars.*`
   onto the agent's profile via `agents/set`, so it is **per-agent**
   (`App.tsx` `ContextStageTile` → `cfgContextVarEdits` → `saveConfigure`). The
   `configure-8` flow even notes the *same* declared param (e.g. `window_rows`)
   appears in both controls. Same package, identical-looking controls, opposite
   blast radius, nothing on screen says which. Lily edits one agent and another
   moves; Ganesh can't tell the effective value or who set it.

2. **Getting started drops Lily into the cockpit.** After the guided wizard,
   `createAgent` routes to `configure` — the densest surface — which is the
   overwhelm moment `01-setup.md` predicts. The wizard already collected what's
   needed; she wanted to talk to the agent she just made.

3. **Daniel's front door doesn't speak to him.** He came to add a coding agent;
   the catalog and welcome present a chat-partner product. The coding-agent path
   is now designed (`02-claude-code.md`) and planned
   (`../handoffs/coding-agents.md`) but not yet built, so the catalog can at least
   acknowledge the intent now so he doesn't bounce in 30 seconds.

4. **Agent config has no altitude.** `ConfigureView` stacks seven peer sections;
   `parent` / `prepend path` / `effective path` (builder plumbing) sit in the
   very first section a newcomer sees. Daniel reads the first screenful, sees
   plumbing, leaves.

5. **Autonomy is a bare dropdown** (`off/manual/assisted/autonomous`) with no
   statement of what each level accepts without asking — `config.md` says the
   interface should show the rules in plain language. Ganesh can't sign off on a
   word.

6. **Ganesh has no off switch and no drift signal.** There is no
   uninstall/disable endpoint for an installed instance capability (`kits/add`
   exists; nothing removes), and `riskBadges` knows `approved`/`pending` but not
   "changed since approval" — both are named in `04-risk-and-trust.md`.

7. **Cost is in the wrong place and the wrong agent.** `costSummary` in the setup
   panel reflects the *default* profile (`primaryProfile`), and there is no
   per-agent cost echo in the configure tab, even though `costSummary` accepts any
   profile. Model pickers carry no cost/perf hint (`03-cost-visibility.md` wants
   one). The setup-pane `SetupPackageConfig` save box is a raw key/value with a
   "value, using TOML for arrays or numbers" placeholder — a builder affordance in
   a product surface — while the configure-tab package cards already render typed,
   manifest-declared inputs.

## Plan

Milestones are ordered by leverage and dependency. Do them in order; each is
shippable on its own. The "shape" bullets are the intent and the constraints; the
exact components, copy, and layout are yours to design — stay inside the
guardrails at the end.

### M1 — Scope honesty (instance vs per-agent)

The point: a person must never be unsure whether a config control changes one
agent or all of them.

Shape:
- First, confirm the runtime precedence between a shared package setting
  (`config set`) and a per-agent `vars.*` override (check `src/context.rs`,
  `src/config_repo.rs`, `src/exec.rs`). The UI's scope claims must match what the
  kernel actually does. Record what you find in this doc.
- Every config control on both surfaces declares its scope in plain language —
  e.g. "applies to every agent" vs "applies to **<agent>** only." No internal
  words.
- Where a setting can be both (shared default + per-agent override), show the
  **effective value** for the current agent and where it comes from (shared
  default, or overridden here). Let a person move a value between scopes
  deliberately rather than by accident.
- Resolve the duplicate `window_rows`-style control: the same parameter should
  not silently exist as two unlabeled controls with different scopes.

Acceptance criteria:
- On the configure tab, for any package setting, a person can read — without
  expanding anything — whether saving it affects one agent or all agents.
- Editing a shared setting and reloading shows the new value reflected for a
  *second* agent that uses the package; editing a per-agent override and
  reloading shows the first agent changed and the second unchanged. (Provable in
  web-qa against two agents; assert reloaded values + the backend log line
  distinguishing `config set` from the profile write.)
- The flow catalog gains/updates entries describing the scope label and the
  effective-value display; `ui.spec.mjs` asserts the durable reloaded values for
  both scopes.
- No new internal vocabulary appears on the surface.

### M2 — Getting-started landing and front door

Shape:
- After a successful guided-wizard create, land in **converse** with the new
  agent, with a quiet, durable pointer that configure exists ("tune <name>
  anytime in configure"). The wizard already collected name/model/budget/workdir,
  so configure is no longer the required next stop. Keep the non-wizard/escape
  paths intact.
- Acknowledge the coding-agent intent on the front door or in the capability
  catalog — a single honest entry that explains the envelope value (sandbox,
  recording, cost control) and says the integration is coming, without pretending
  it's wired. Do not build the integration.

Acceptance criteria:
- Creating an agent through the wizard lands on `#view-converse` for that agent
  (durable: the converse view is visible and the compose box targets the new
  agent), and the new agent appears in the nav. A visible pointer to configure is
  present.
- The catalog/front door shows a coding-agent entry whose copy is plain language
  and makes no false claim of being configured.
- `docs/ui-flows/configuration.md` `create-1` is updated to the new landing;
  `ui.spec.mjs` follows.

### M3 — Agent-config altitude (essentials vs advanced)

Shape:
- Reorganize `ConfigureView` so the essentials a normal person needs — name,
  model, run-step cap (spend ceiling), autonomy, working directory — are immediate
  and the rest (context program, sandbox prefixes, throttle, package path,
  raw file, advanced context parameters) live under one honest "advanced" fold
  that a person never has to open. Keep the section anchors/index working for
  builders.
- Move `parent` / `prepend path` / `effective path` out of the first thing a
  newcomer sees (they belong with packages or under advanced).
- Give `autonomy` a one-line consequence that updates with the selection
  (what this level lets the agent's own setting changes do without asking).
- Echo this agent's cost at the top of its configure tab (reuse `costSummary`
  with this agent's profile): model, spend ceiling, autonomy. Frame the run-step
  cap as the hard activation ceiling it is.
- Add a cost/perf hint to the model picker (cheap / balanced / powerful, or
  whatever honest signal the model list affords — keep `03-cost-visibility.md`'s
  "unknown beats fake precision" rule).

Acceptance criteria:
- A person opening configure sees name, model, spend cap, autonomy, and workdir
  without scrolling past plumbing; `parent`/path controls are not in the first
  section.
- Changing the autonomy select changes a visible plain-language consequence line.
- The configure tab shows this agent's cost summary (not the default agent's),
  and it updates when you switch agents. Provable by reload + switching agents.
- All current `configure-*` flows still pass; the catalog reflects the new
  layout (essentials vs advanced) and the autonomy consequence line.

### M4 — Instance-config completeness for Ganesh

Shape:
- Give an installed instance capability an **off** affordance — at minimum a
  reversible disable, ideally remove — with an app-store-style confirmation of
  what's being turned off. This likely needs a new CLI/relay path; scope it
  honestly (a clean "disable" via config may be more tractable than a hard
  uninstall — decide and record why). If a true uninstall is out of reach this
  pass, ship disable and note the gap.
- Add a "changed since approval" signal to the risk badges where the data
  supports it (compare current manifest/grants against what was approved). If the
  backend can't yet answer this, say so in the doc rather than faking it.
- Replace the setup-pane `SetupPackageConfig` raw key/value box with the same
  typed, manifest-declared inputs the configure-tab `PackageCard` already uses, so
  instance package config isn't a builder affordance.
- Make the setup cost panel name the agent it describes (or follow selection),
  so it stops silently showing the default agent's model.

Acceptance criteria:
- From the installed-capabilities surface, a person can turn a capability off and
  confirm it durably (reload shows it off / removed; backend log shows the call).
  If disable-not-uninstall, the label says so plainly.
- The "changed since approval" signal appears only when backend data supports
  it; otherwise this sub-item is explicitly deferred in this doc with the reason.
- Instance package settings render typed inputs from the manifest, not a raw TOML
  value box; saving and reloading round-trips the value.
- The setup cost panel unambiguously names whose cost it shows.
- New/updated flows in `docs/ui-flows/configuration.md` and assertions in
  `ui.spec.mjs`.

## Guardrails

- **Layering rule is absolute.** No grant/stage/topic/mailbox/ledger/pending or
  raw addresses/URLs on product surfaces. When you need an internal concept,
  translate it.
- **Durable over flashing.** Confirmations must survive a re-render/reload —
  follow the durability facts at the top of `docs/ui-flows/configuration.md`
  (`#setup-status` persists; `#cfg-note`/`#na-note` are liveness only).
- **The flow catalog is the contract.** Every behavior change updates
  `docs/ui-flows/configuration.md` and `ui/web/test/ui.spec.mjs`. A change without
  a durable observable isn't done.
- **QA the real UI.** Use the `web-qa` skill against an isolated live stack;
  assert reloaded state and read the backend log. Don't declare a milestone done
  on a green you didn't watch.
- **Don't guess the data model.** If scope/precedence/drift needs a backend fact,
  confirm it in the Rust and record it here; defer honestly if the backend can't
  answer yet.

## Non-goals

- The Codex / Claude Code integration itself (`02-claude-code.md`,
  `../handoffs/coding-agents.md`) — M2 only acknowledges the intent.
- Real billing/pricing, usage history, or budget alerts — cost stays "honest
  labels, no fake precision."
- Kernel/bus/identity redesign; the live signals and telemetry views.

## Log (fill in as you go)

- Precedence finding (shared `config set` vs per-agent `vars.*`): confirmed in
  `src/context.rs`. A context-stage declared setting starts from the manifest
  default, then a shared package setting from `config/packages/<pkg>.toml`
  replaces that default. During assembly, the agent profile's `vars.*` values
  are already in `doc.meta.vars`, and shared/stage values are inserted only when
  the key is absent. Effective order for same-named keys is therefore:
  per-agent profile value wins, otherwise shared package setting, otherwise the
  package default.
- "changed since approval" backend support: _TODO_
- Off-switch decision (disable vs uninstall) and why: _TODO_

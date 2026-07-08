---
status: done
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-08
---
# Handoff: package dependencies — light, versionless validity + a remediation report a tiny model can execute

Tim's intent, verbatim: *"We probably need package dependencies. Keep it LIGHT.
Probably NO versioning — modifiable skills make versioning really weird. Just some
light checks to deterministically figure out if an agent configuration is valid,
and a solid error-reporting system that prompts even a tiny LLM into getting into
a good state."*

Three deliberately-minimal parts:

1. **A dependency declaration** — a package names the other packages it needs, by
   name, in its `lanius.toml`. No version constraints (see "why no versioning"
   below).
2. **A deterministic validity check** — given a profile's package set + config,
   decide with NO LLM whether every declared dependency is satisfied (required
   package present *and* approved; each package's own required config keys set),
   plus cycle detection. It is a graph/set check.
3. **A remediation report that steers even a tiny LLM into a good state** — the
   distinctive part. When validation fails, emit a structured, self-contained
   report where **each problem is paired with its exact fix command**, shaped so a
   small model reading it can mechanically run the fixes and re-check.

**The state of the world this closes.** There is no dependency mechanism today —
the helper handoff records it plainly: *"a kit is also our only grouping mechanism
(no package dependencies — the journey accepts this)"*
(`agentic-configuration.md:33`). Kits group packages by co-location on the path,
but nothing checks that `recall` is useless without `phonebook`, or that
`telegram` needs its token set. Handoff C's M5 does that wiring **by hand**
("approve `phonebook` and `recall`… seed `owner`"). This handoff turns those
manual, remembered steps into a declared, checkable fact with a printed fix.

## Why NO versioning (the load-bearing design call — confirm)

A package/skill is **editable in place**. The config model (`docs/config.md`) and
modifiable skills (the `self-modify` skill, the KB `kb write` runtime path) mean a
package's bytes change under a running instance — that is a *feature*, and the
grants ledger already tracks it by hashing the current content
(`manifest.rs:447-464`: `hash` = manifest bytes folded with `code_hash`; any edit
detaches approvals). So a package's **identity is its name plus its current
content**, pinned by that hash — not a frozen semver. A dependency like
`phonebook >= 1.2` would be a **lie**: there is no immutable `1.2` to point at;
the `phonebook` on the path is whatever it is right now, and the ledger already
re-reviews it when it changes. Pinning a version would invent an identity the
system deliberately refuses to have. So: **depend on a name; the hash/approval
machinery already handles "the content changed."** No solver, no lockfile, no
transitive version resolution. This is also *why the check can be so light* — "is
the dependency satisfied" is a set/graph question, not a constraint-solving one.

## Decisions to confirm / wonky bits (my judgment calls flagged)

1. **What can be depended on. Recommend: package names as the core; reuse the
   EXISTING `[config] keys required` for the config half; DEFER an explicit
   `[requires] grants`.**
   - **Packages by name** — the new `[requires] packages = ["phonebook"]` (M1).
     This is the MVP and covers `recall→phonebook`, `comms→history`,
     `telegram→{phonebook,recall}`.
   - **Required config keys — do NOT invent a new field.** A package already
     declares its own required config keys via `[config] keys` with
     `required = true` (`manifest.rs:105-131`), and the kb-groundskeeper setup gate
     already treats an unset required key as "inert, here's why"
     (`groundskeeper.rs:398-448`). The validity check should **reuse that
     declaration** — check each visible package's own required keys with
     `config_repo::get_key` (`config_repo.rs:361`) — not add a parallel
     `[requires] config`. This catches the real `telegram.TELEGRAM_TOKEN`-unset
     failure with zero new surface, and it makes an already-enforced gate
     *legible* rather than adding new enforcement.
   - **Required grants — recommend DEFER (do not build in MVP).** "The dependency
     is approved" already means *its own* grants are held (`is_granted`,
     `packages.rs:152`). The one case that looks like a cross-package grant dep —
     "telegram needs the `dm` grammar" (Handoff B) — is actually satisfied by
     **telegram's own** approved `publish in/dm/telegram/#` grant, which the
     `[requires] packages` + "approved" check already covers. I found no failure
     mode a separate `[requires] grants` catches that the package-present-and-
     approved check doesn't. Leave a one-line note in M1 for a future `grants`
     sub-key if a real case appears; don't speculatively build it.

2. **No versioning at all (above). Confirm** we ship `[requires] packages =
   ["name", …]` — a flat list of names, nothing else.

3. **When it runs. Recommend: on-demand `elanus packages check [--profile]` as the
   primary surface, PLUS a load-time/approve-time WARN that never refuses.** A
   half-configured instance (mid-setup, dependency not yet approved) must **not**
   be bricked by a hard refuse — that is exactly the "oh shit" moment the helper
   charter forbids. So: the daemon and `approve` *warn* and surface the report;
   they do not block. (Detail in M4.) Confirm the non-refusing posture.

4. **Cycle handling. Recommend: detect and REPORT a cycle as a problem (with the
   cycle path in the message); do not try to auto-break it.** A cycle among
   `[requires] packages` is a config bug the human/agent fixes by editing a
   manifest; the report names the loop. Light DFS, no SCC machinery.

5. **Reuse `discover`, don't reinvent "how to enable".** `discover.rs` already
   computes, for a package the profile can't see, the enable path
   (`enable_guidance`, `discover.rs:257`) and the "present on the instance but off
   this profile's path" vs "not installed at all" taxonomy (mirrored in
   `agentcli::validate_with_packages`, `agentcli.rs:332-370`, which does the exact
   three-way classification launch-time). The report's "missing package" branch
   **reuses that machinery** — same guidance text, same three outcomes — rather
   than writing a second copy. Confirm we route the missing-package remediation
   through the existing enable-guidance rather than duplicating it.

6. **Backward compatibility is non-negotiable.** A package with no `[requires]`
   table and no `required` config keys has **no dependencies and is always valid**.
   Every existing package (none declare `[requires]` today) stays valid. `[requires]`
   defaults to empty; absence is not a failure.

## Milestones

### M1 — the dependency declaration: `[requires] packages`

Add an optional `[requires]` table to the manifest (`src/manifest.rs`), with one
field for the MVP: `packages: Vec<String>` (names, one topic level each, validated
like the other name fields — `topic::valid_name`, no `/ + #`). Absent table =
empty vec = no deps. Add it to `Manifest` (with `#[serde(default)]`) and to the
`packages`/`discover --json` output so a reader (and the UI/helper) can see a
package's declared deps. Reserve — but do NOT implement — a future `grants`
sub-key (wonky bit 1); document the reservation in a comment so nobody repurposes
`[requires]` for something else.

Wire the two motivating declarations so later milestones have something real to
check: add `[requires] packages = ["phonebook"]` to `packages/recall/lanius.toml`
(recall calls the phonebook at runtime but declares nothing today — B/C confirm
this is the true dependency), and, when Handoff C lands `packages/telegram`, it
declares `[requires] packages = ["phonebook", "recall"]` + its own
`[config] keys` with `TELEGRAM_TOKEN` `required = true` (this **replaces C M5's
by-hand "approve phonebook and recall" step** with a declared, checked fact — note
the seam in C).

- **Acceptance:** a manifest with `[requires] packages = ["phonebook"]` parses and
  the name surfaces in `elanus packages --json`. A manifest with **no** `[requires]`
  table parses and reports empty deps (backward compat asserted). An invalid dep
  name (`"a/b"`, `"#"`) is refused at load like the other name fields. Editing
  `[requires]` moves the manifest hash (it rides `raw`, so a dependency change
  re-enters review like any manifest edit — assert the hash moves).

### M2 — the deterministic validity check (the graph/set engine)

Add `packages::validate(root, conn, profile) -> ValidityReport` — a **pure,
no-LLM** function returning a list of structured `Problem`s. It composes only
existing reads:

- **visible set** = `discover_for_profile(root, profile)` (`packages.rs:71`) — what
  this profile can see on its path.
- **universe** = `discover(root)` (`packages.rs:67`) — everything installed on the
  instance.
- For each **visible** package that declares `[requires] packages`, for each dep:
  classify with the same three-way logic as `agentcli::validate_with_packages`
  (`agentcli.rs:349-368`):
  1. dep **not in universe** → `PackageNotInstalled` (needs installing).
  2. dep in universe but **not in the visible set** → `PackageOffPath` (needs
     enabling; remediation reuses `discover`'s enable-guidance, wonky bit 5).
  3. dep visible but **not `is_granted`** (`packages.rs:152`) → `PackageNotApproved`.
     A dep that is visible AND granted is satisfied — no problem.
- For each **visible** package, for each of its own `[config] keys` with
  `required = true` (`manifest.rs:116-131`), if `config_repo::get_key(root, pkg,
  key)` is `None` → `ConfigKeyUnset` (reuses the existing setup-gate condition,
  wonky bit 1).
- **Cycle detection** over the `[requires] packages` edges (restricted to the
  universe): a simple DFS with a recursion stack; a back-edge yields one
  `DependencyCycle` problem carrying the loop path (`a → b → a`). Light — no SCC
  algorithm, no version graph.

Each `Problem` carries: the offending package, a machine kind (the enum above), a
one-line human description, and — the load-bearing field for M3 — an **exact fix
command string** (or, for `PackageOffPath`/`PackageNotInstalled`, the reused
enable/install guidance). No `Problem`s = valid.

- **Acceptance (the worked example, no LLM):** on a scratch root with `recall`
  declaring `[requires] packages = ["phonebook"]` and both on the profile's path
  but `phonebook` **unapproved**: `validate` returns exactly one problem,
  `PackageNotApproved` for `phonebook`, whose fix command is
  `elanus approve phonebook`. Approve `phonebook` → `validate` returns no problems
  (valid). Make `phonebook` off-path → `PackageOffPath` with the enable-guidance
  remediation (not the approve command). A required config key unset on a visible
  package → `ConfigKeyUnset` with `elanus config set <pkg>.<key> …`. A manifest
  pair `a`⇄`b` requiring each other → one `DependencyCycle` naming the loop. A
  package with no `[requires]` and no required keys → never appears. All computed
  deterministically (run twice, byte-identical).

### M3 — the remediation report: `elanus packages check` (the tiny-LLM-steering format)

**This is the part Tim cares most about — spend the design here.** Render the M2
`ValidityReport` as a structured, **self-contained** report where every problem is
one numbered item paired with its exact fix, shaped like a good remediation
prompt: a small model can read top-to-bottom, run each `fix:` line, and re-run the
re-check command. Add `elanus packages check [--profile <p>] [--json]` (a new
`Cmd::PackagesCheck`, or a `--check` flag on `Cmd::Packages`). Human output and a
stable `--json` (same shape the helper/UI relays).

The format, concretely (worked telegram + recall example):

```
$ elanus packages check --profile default
FAIL — 3 problems in profile "default". Fix each, in order, then re-run the
re-check command at the bottom.

[1] package `recall` requires `phonebook`, which is installed but not approved.
    fix: elanus approve phonebook

[2] package `telegram` requires `recall`, which is installed but not approved.
    fix: elanus approve recall

[3] package `telegram` needs config `telegram.TELEGRAM_TOKEN`, which is unset.
    fix: elanus config set telegram.TELEGRAM_TOKEN <your-bot-token>

re-check: elanus packages check --profile default
```

A valid config prints a single clear line: `OK — profile "default": all N
packages' dependencies satisfied.` An off-path dependency prints the reused
enable-guidance as its `fix:` (proposal / add-to-profile), not a bare command. A
cycle prints the loop and says which edge to remove.

Design rules that make it executable by a tiny model (state these in the handoff
so the implementer keeps them): (a) **one problem, one fix line** — never bundle;
(b) the `fix:` is a **literal command** wherever one exists, copy-pasteable, with
`<placeholders>` only for values the human must supply (tokens); (c) the report is
**self-contained** — it names the profile, ends with the exact re-check command,
and needs no other context to act on; (d) **ordered** so running the fixes top-to-
bottom converges (approve deps before the dependents; set config last). This is
deterministic to compute but reads like a remediation prompt — that is the whole
ask.

- **Acceptance:** the worked example above produces the numbered problem+fix report
  (assert each `fix:` line is the exact command that resolves that problem — e.g.
  running `elanus approve phonebook` then re-checking drops item [1]). `--json` is
  machine-stable (each item = `{package, requires?, kind, message, fix,
  recheck}`). A fully-satisfied profile prints the single OK line and exits 0; a
  failing one exits non-zero (so a script/agent can branch on it). Feeding the
  human report to a small model and having it execute the `fix:` lines reaches a
  valid state (a manual once-through is acceptable evidence — this is the
  "prompts even a tiny LLM" bar).

### M4 — when it runs: on-demand + a non-refusing warn at approve and load

Wire the check into the moments it helps, **always as a warning, never a refusal**
(wonky bit 3):

- **At `approve`** (`packages::decide`, `main.rs:1479`): after flipping a
  package's grants, run `validate` for **that package's** declared deps and, if any
  are unmet, print the M3-shaped nudge — e.g. approving `recall` prints
  `recall approved — but it requires phonebook, which is not yet approved. fix:
  elanus approve phonebook`. This is the ergonomic that replaces "remember to also
  approve phonebook."
- **At load / daemon tick:** where the dispatcher already syncs drifted manifests
  (`sync_if_drifted`, `packages.rs:605`), additionally run `validate` for the
  active profile(s) and **log** any problems (a warn line, once per changed state —
  don't spam every tick). The daemon keeps running; a temporarily-invalid config is
  surfaced, not fatal.
- **Helper / UI surface:** expose the M3 `--json` report so the agentic-
  configuration helper (`agentic-configuration.md`, `helper-m4-…`) and the web
  Packages view can render "3 things to fix" with the fix commands — the helper's
  charter is exactly "get you set up," and this report is the checklist it works.

- **Acceptance:** `elanus approve recall` with `phonebook` unapproved prints the
  dependency nudge (assert the `elanus approve phonebook` line appears) and still
  approves recall (non-refusing — recall's own grants flip). A daemon started with
  an invalid config logs the warning and does **not** exit or refuse to dispatch
  (assert it stays up). The `--json` report is reachable for the UI/helper (a
  route or command the web server can relay).

## Read these first

- `src/manifest.rs:17-96` (the `Manifest` struct — where `[requires]` is added),
  `:105-131` (`ConfigDecl`/`ConfigKeyDecl` — the `required` config keys the check
  REUSES), `:447-464` (why identity is name+content-hash, not a version — the "no
  versioning" grounding).
- `src/packages.rs:67` (`discover` — the universe), `:71` (`discover_for_profile` —
  the visible set), `:152` (`is_granted` — the "approved" test), `:476-538`
  (`decide` — where the approve-time nudge hooks in), `:605` (`sync_if_drifted` —
  the daemon tick the load-time warn rides).
- `src/agentcli.rs:332-370` (`validate_with_packages` — the exact three-way
  installed / off-path / not-approved classification M2 generalizes from
  launch-time-required to declared-dependency).
- `src/discover.rs:81` (`scan`) and `:257` (`enable_guidance` — the "how to enable
  an off-path package" text the report reuses instead of duplicating).
- `src/groundskeeper.rs:398-448` (`required_keys` / `load_config` — the living
  precedent for "a required config key is unset → inert, with the reason and the
  `lanius config set …` cure"; the config half of the check is this pattern,
  generalized to any package).
- `src/config_repo.rs:361` (`get_key` — how a config value is read; `None` = unset).
- `docs/config.md` (the editable-in-place config model — why versioning is a lie)
  and the `self-modify` skill / `kb write` runtime path (modifiable skills, same
  reason).
- Sibling handoffs for the worked deps: `dm-channel-grammar.md` (B — the `dm`
  grammar telegram's own grant satisfies), `agent-dm-relay.md` (C — its **M5**
  by-hand "approve phonebook + recall" is what this handoff's declared deps
  replace), `comms-package.md` (D — `comms`/history requires `history`).
- `docs/handoffs/agentic-configuration.md:33,55` + `helper-m4-harness-backed-turns.md`
  (the tiny-LLM remediation audience — the helper surfaces this report).

## Residuals / gating

- **No versioning, by design** (the top section). If a real need for content-
  pinning ever appears, it is the manifest hash the ledger already computes, not a
  semver — a separate, later decision.
- **`[requires] grants` is deferred** (wonky bit 1) — the package-present-and-
  approved check plus each package's own grant approval covers every failure mode
  found; add it only against a concrete case.
- **Transitive depth is shallow by intent** — the check walks the declared-`packages`
  edges for presence/approval/cycles, but there is no version resolution, no
  lockfile, no auto-install. "Light" (Tim, twice) is the gate on any temptation to
  grow a resolver.
- **Non-refusing is a safety rail** (wonky bit 3): the check warns and reports; it
  never bricks a mid-setup config. A hard-refuse mode, if ever wanted, is a later
  opt-in, not the default.
- **The config-key half adds no new enforcement** — it surfaces the *existing*
  setup-gate condition (`groundskeeper.rs`) with a fix command; it does not newly
  fail packages that ran before. Confirm with the config/groundskeeper owner that
  making the gate legible here is welcome (it should be).
- **Depends on nothing to start** — M1/M2/M3 stand alone on the current worktree.
  M4's helper/UI surface soft-depends on the helper handoffs being present. The
  telegram worked example depends on Handoff C landing `packages/telegram`; until
  then, `recall→phonebook` and a synthetic pair are enough to exercise the engine.

## Log

- 2026-07-08 — planner drafted from the worktree. **Grounding:** no dependency
  mechanism exists today (helper handoff: *"no package dependencies — the journey
  accepts this"*, `agentic-configuration.md:33`); kits are the only grouping, and
  Handoff C M5 wires `recall`/`phonebook` by hand. Chose **name-only deps, no
  versioning** — a package's identity is name + current content-hash
  (`manifest.rs:447`), editable in place, so a semver would be a lie; the ledger
  already re-reviews on content change, which is what makes the check a pure
  set/graph question. Reused, not reinvented: `discover_for_profile`/`discover`
  (visible vs universe), `is_granted` (approved test), `agentcli::validate_with_packages`
  (the installed/off-path/not-approved three-way), `discover::enable_guidance` (the
  enable text), and the **existing** `[config] keys required` setup gate
  (`groundskeeper.rs:398`) for the config half — so the config check adds no new
  field and no new enforcement, only legibility. Recommended **on-demand `elanus
  packages check` + non-refusing approve/load warns** (never brick a mid-setup
  config), and a **one-problem-one-fix, self-contained, ordered** report format as
  the tiny-LLM-steering deliverable. Deferred `[requires] grants` (no failure mode
  the present+approved check misses) and any resolver/lockfile ("light", per Tim).

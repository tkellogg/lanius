---
status: planned
author: Opus (planner)
last-updated: 2026-07-06
---

# Rename: elanus → Lanius (binary/crate `lanius`)

A purely mechanical naming pass. The product, the crate, and the binary become
**Lanius / `lanius`**. Nothing about behavior changes. This handoff is the
complete accounting of *where the old name is baked in* and *how to change it
without stranding Tim's live install*.

**Explicitly out of scope** (Tim's ruling): the Lily-targeted visual pass and
the SVG logos. Those are separate items. This is the boring, safe, mechanical
rename only.

There is real precedent to copy: the project already renamed itself once
(`harness` → `elanus`), and left behind two working migration shims —
`src/envcompat.rs` (env-var back-compat) and `src/db.rs::migrate_db_filename`
(ledger auto-rename at open). We extend those exact patterns rather than invent
new ones.

---

## Wonky bits / decisions to confirm (read these first)

These are the calls that change the *shape* of the work. Flagged for Fable.

1. **The `harness-*` names STAY.** The four adapter binaries
   (`harness-claude`, `harness-codex`, `harness-opencode`, `harness-acp`), the
   `harness-doctrine` package, and the word "harness" throughout the docs are a
   **deliberate keep** — "harness" is a real concept in this system, not a
   brand. This matches the prior ruling (see the elanus-project memory and
   `src/manifest.rs`'s comment that the tool-named manifest file is "the settled
   exception"). Only the **main binary/crate `elanus` → `lanius`** and the
   *product word* "elanus" change. `[[bin]] name = "harness-acp"` in `Cargo.toml`
   and `src/bin/harness-*.rs` are untouched.

2. **Env vars: chain the back-compat, don't collapse it.** `src/envcompat.rs`
   today reads `ELANUS_*` then falls back to `HARNESS_*`. We make canonical
   `LANIUS_*` and chain: **read prefers `LANIUS_` → `ELANUS_` → `HARNESS_`**; the
   set-side writes **all three** (`env_dual` becomes `env_triple`). Rationale:
   installed package scripts and adapters in Tim's live root read `ELANUS_*` from
   their environment; if we only rename the *set* side they break. Keeping all
   three names on child commands is cheap and fully back-compatible. See
   "Env-var reality" below — most `ELANUS_*` are internal launch-contract vars
   set by the parent and read by the child (both ends rename together), so only
   the *user/script-facing* ones strictly need the shim, but dual/triple-setting
   everything is the simplest safe rule.

3. **Package manifest filename `elanus.toml` → `lanius.toml`: do it, with a
   read-time fallback.** Every package's manifest is `elanus.toml`
   (`src/manifest.rs::load`, line 419 — `pkg_dir.join("elanus.toml")`). For a
   *consistent* rename this should become `lanius.toml`. Risk: this is the one
   rename that touches **files on disk in Tim's live packages** and in the repo
   (`packages/*/elanus.toml`, `kits/**/packages/*/elanus.toml`). Decision:
   **loader prefers `lanius.toml`, falls back to `elanus.toml`**; rename in-repo
   files; the upgrade script renames installed ones. **Grants caveat:** grants
   are pinned to the manifest *content* hash, not the filename, so renaming the
   *file* does not re-trigger approval — but if we also edit manifest *contents*
   (they don't currently contain "elanus"), the hash changes and the package
   re-enters pending. Keep manifest contents byte-identical across the rename.
   *If Fable prefers to minimize disk churn, the fallback-only option is: keep
   reading `elanus.toml`, never rename it — but that leaves a visible
   inconsistency. Recommend the rename.*

4. **Default root `~/.elanus/root` → `~/.lanius/root`: honor both.**
   `src/paths.rs::default_root` hard-codes `~/.elanus/root`. Decision:
   **prefer `~/.lanius/root`, fall back to `~/.elanus/root` if it exists** (and
   `resolve()` already checks for the db marker). The upgrade script physically
   moves the old root to the new path so nothing is stranded. Honoring both means
   a user who never runs the script still works.

5. **`elanus_session` (DB column + JSON wire field) is a KEEP — out of scope.**
   `code_projection.rs` and `codesession.rs` use `elanus_session` as a SQL column
   name and the "stable wire id the UI keys on"
   (`src/code_projection.rs:97`, consumed as `elanus_session` in
   `ui/web/src/CodeSessions.tsx`). Renaming it is a **schema + wire migration**,
   not a mechanical string swap — it would break the CLI↔web JSON contract and
   need a table migration. It is an internal identifier, not brand. **Leave it
   `elanus_session`.** State this in the handoff so nobody "helpfully" renames it.

6. **Doc policy: rename living commands, preserve historical records.** In docs,
   **update copy-pasteable command examples and living reference docs** (README,
   `docs/runtime.md`, `docs/config.md`, and every `SKILL.md` — agents *shell*
   those, so they must say `lanius`). **Do NOT rewrite the narrative/Log prose of
   historical handoff and journey docs** (`docs/handoffs/*.md`,
   `docs/journeys/*.md`) — they are records of what happened, and they said
   "elanus" at the time. Rule of thumb: if a reader would copy a line into a
   shell, rename it; if it's a record of a past decision, leave it. This is a
   judgment call — Fable, confirm the boundary.

7. **Transition alias: YES, one cycle.** Keep an `elanus` name pointing at
   `lanius` so muscle memory and old scripts survive one deprecation cycle. The
   upgrade script creates a symlink `~/.cargo/bin/elanus → lanius`. The CLI is
   argv[0]-agnostic (clap only uses the name cosmetically in help), so the alias
   just works. Remove it next cycle.

8. **This lands on a CLEAN tree, as a focused sweep.** A rename touches nearly
   every file; doing it while siblings are mid-flight would create enormous
   conflicts. The implementer must **verify `git status` is clean** (all sibling
   work committed) before starting, and land the rename as one coherent commit
   (or a very small number). See M0.

---

## Env-var reality (grounding for M2)

`rg -o "ELANUS_[A-Z_]+" src/` yields ~80 distinct names. They fall in three
buckets:

- **User/script-facing** (a human or a package script may set/read these): must
  keep back-compat. `ELANUS_ROOT` (`src/harness.rs:8`, read via
  `envcompat::read("ROOT")` in `src/paths.rs:89`), and the dispatcher→package
  vars a `scripts/main` reads: `ELANUS_PACKAGE`, `ELANUS_ACTOR`,
  `ELANUS_SCRATCH`, `ELANUS_STAGE`, `ELANUS_EVENT_ID`, `ELANUS_CONFIG_DIR`,
  `ELANUS_PROFILE`, `ELANUS_TOOL`, `ELANUS_DISPATCH_ID`.
- **Kernel→adapter launch contract** (`src/harness.rs` `ENV_*` consts,
  `src/codeagent.rs` `ELANUS_CODE_*`): parent `elanus` sets them, child adapter
  reads them — both ends are our code and rename together. Dual/triple-setting is
  harmless insurance for a live root whose adapters predate the rename.
- **Provider creds** `ELANUS_PV_*` (`src/provider.rs`): user-facing (documented
  in `.env`). Keep back-compat.

**Recommended approach:** make `LANIUS_*` canonical everywhere in code; extend
`envcompat.rs` `read()` to try `LANIUS_` → `ELANUS_` → `HARNESS_`; extend the set
helper to write `LANIUS_` + `ELANUS_` (drop `HARNESS_` from the set side — nothing
reads it anymore, but keep it in the read chain). Route **every** kernel-set
child env var through the helper so no name is set only under the old spelling.

---

## Milestones

Each milestone is independently reviewable, but they SHIP TOGETHER as one
rename (partial renames leave the tree in a broken half-state). Order is
build-order: code first, then on-disk/data shims, then the sweep, then the
upgrade script.

### M0 — Preconditions
- The implementer runs on a **clean working tree** (`git status --porcelain`
  empty). If not clean, STOP and surface it — do not rename over uncommitted
  sibling work.
- Read this whole doc, `src/envcompat.rs`, `src/db.rs`, `src/paths.rs`.
- **Acceptance:** `git status --porcelain` prints nothing before any edit.

### M1 — Crate + binary rename
- `Cargo.toml`: `name = "lanius"`, `default-run = "lanius"`, update
  `description`. **Leave** `[[bin]] name = "harness-acp"` and the `include`
  allowlist as-is (paths unaffected by the crate name).
- `src/bin/harness-*.rs` filenames and bin names: **unchanged** (deliberate
  keep).
- `src/initcmd.rs::STOCK_HARNESS_PACKAGES`: the `dir`/`binary` fields
  (`harness-claude` etc.) **stay**; `seed_stock_harness_packages` copies
  `harness-<tool>` from `exe_dir` — still correct after the main binary renames,
  because those adapter bins keep their names.
- README build/install lines updated (`cargo build --release` still works;
  binary is now `target/release/lanius`; `cargo install --path .` installs
  `lanius`). Note the crates.io name would be `lanius` if ever published.
- **Acceptance:** `cargo build --release` produces `target/release/lanius` plus
  the four `harness-*` bins; `cargo test` compiles; `./target/release/lanius
  --help` runs.

### M2 — Env vars: `ELANUS_*` → `LANIUS_*` with chained back-compat
- Extend `src/envcompat.rs`: `read(suffix)` tries `LANIUS_` → `ELANUS_` →
  `HARNESS_`; add `env_triple` (or rename `env_dual`) that sets `LANIUS_` +
  `ELANUS_`. Keep the module doc honest about the chain.
- Flip every `ELANUS_*` literal and `ENV_*` const in code to `LANIUS_*`
  canonical (`src/harness.rs`, `src/codeagent.rs`, `src/dispatcher.rs`,
  `src/bus.rs`, `src/exec.rs`, `src/context.rs`, `src/pkgtool.rs`,
  `src/provider.rs`, `src/web.rs`, `src/dev.rs`, `src/kb.rs`, `src/main.rs`,
  `src/paths.rs`, `src/secrets.rs`, `src/agentcli.rs`, `src/events.rs`,
  `src/manifest.rs`, `src/acp.rs`, tests).
- Ensure **every kernel-set child env var** goes through the set helper (audit
  every `.env("ELANUS_…")` / `.env_dual(…)` call site) so the old spelling is
  still present for pre-rename package scripts and adapters.
- **Acceptance:** `cargo test` green; a package `scripts/main` that reads
  `ELANUS_ACTOR` still sees a value when launched by the new binary (covered by
  an added regression test, mirroring `envcompat.rs`'s existing tests);
  `LANIUS_ROOT` and `ELANUS_ROOT` both resolve the root in `paths::resolve`.

### M3 — Ledger filename `elanus.db` → `lanius.db`
- `src/paths.rs`: `Root::db()` → `lanius.db`; add the chain so `legacy_db()`
  covers **both** `elanus.db` and `harness.db` as migration sources.
- `src/db.rs::migrate_db_filename`: extend to migrate `harness.db` **or**
  `elanus.db` → `lanius.db` (WAL/SHM siblings too), idempotent, no-brick, at the
  single `open()` chokepoint — mirror the existing function exactly.
- `src/paths.rs::resolve` and the web error strings: accept `lanius.db` /
  `elanus.db` / `harness.db` as root markers.
- Update the fixed strings in `src/profile.rs` (kernel-churn exclude list),
  `src/sandbox.rs` (deny-list + tests), `src/web.rs` (history error text).
- **Acceptance:** the existing `migrates_legacy_db_filename` test is extended
  (or a sibling added) proving `elanus.db` → `lanius.db` preserves data and is
  idempotent; `cargo test` green.

### M4 — Package manifest filename `elanus.toml` → `lanius.toml`
- `src/manifest.rs::load`: prefer `lanius.toml`, fall back to `elanus.toml`
  (both parse identically). Every other `join("elanus.toml")` in non-test code
  and the `include_str!("../packages/*/elanus.toml")` roots in `src/initcmd.rs`
  move to `lanius.toml`.
- Rename in-repo manifest files: `packages/*/elanus.toml`,
  `kits/**/packages/*/elanus.toml` → `lanius.toml` (content **byte-identical** —
  do not touch the bytes, only the name, to preserve grant hashes).
- Update tests that write/read `elanus.toml`.
- **Acceptance:** `cargo test` green; a package dir containing only a legacy
  `elanus.toml` still loads (fallback path); a freshly `init`'d root writes
  `lanius.toml` manifests.

### M5 — Default root `~/.elanus/root` → `~/.lanius/root` (honor both)
- `src/paths.rs::default_root` → `~/.lanius/root`; `resolve()` falls back to
  `~/.elanus/root` when the new path has no root but the old one does (check the
  db marker there).
- `src/kit.rs`: user-level kit dir `~/.elanus/kits` → `~/.lanius/kits` (honor
  both if cheap; otherwise document the move in the upgrade script).
- Sweep the `~/.elanus` literals in doc/comment strings (`src/dev.rs` messages,
  `src/kit.rs` comments, error text in `paths.rs::resolve`).
- **Note the stranding risk:** the root path is embedded in (a) the codex hook
  config (`src/codeagent.rs::codex_hook_config`, `<exe> -C <root> code hook
  PostToolUse`) — but that config is written into an **ephemeral per-session
  CODEX_HOME under the run dir and regenerated every launch**, so it self-heals;
  (b) the copied adapter binaries (no embedded path — plain copies). So the only
  persistent embedded old-root references are a user's shell env (`ELANUS_ROOT`)
  and any launchd/supervisor config the user wrote by hand. The upgrade script
  handles the move and flags anything it can't safely rewrite.
- **Acceptance:** with only `~/.elanus/root` present, `lanius packages` resolves
  it (fallback); after the upgrade script moves it, `lanius packages` resolves
  `~/.lanius/root`.

### M6 — CLI-string & brand sweep
Agents **shell** these strings, so they must be correct. In each area, replace
the *command* `elanus` with `lanius` and the product word "elanus"/"Elanus" with
"lanius"/"Lanius":
- `kits/**/SKILL.md` and any package `scripts/` (~56 files under `kits/`).
- `.claude/skills/*` in this repo (the debug skills, handoff-workflow, web-qa,
  etc.) — including the `elanus code …` invocation strings.
- `.claude` permission rules / allowed-command lists: add `lanius …` variants
  wherever `elanus …` is allowed, and keep the `elanus …` variants for the
  transition alias window.
- Living docs: `README.md`, `docs/runtime.md`, `docs/config.md`, and other
  reference docs. **Per M6 policy (wonky bit #6), leave historical handoff/
  journey narrative prose alone** — rename only copy-pasteable commands there.
- Web UI user-facing strings in `ui/web/`: page `<title>`, the `elanus.theme`
  localStorage key (renaming it silently resets a user's saved theme — acceptable,
  note it), `package.json` name/description, the "Run `elanus code project`"
  empty-state hint. **Do NOT rename the `elanus_session` field** (wonky bit #5).
- In-binary help/hint text: `DISPATCH_HINT` and the `elanus code` usage strings
  in `src/codeagent.rs` and `src/main.rs`.
- The elanus supervisor injects an `[elanus]` dispatch tip each turn
  (`src/codeagent.rs:111`) — rename its command examples to `lanius code …`.
- **Acceptance:** `rg -n "\belanus\b" kits/ .claude/ README.md docs/runtime.md
  docs/config.md ui/web/src` returns only deliberate keeps (historical prose,
  `elanus_session`, the `ELANUS_*` back-compat names, "harness" concept text);
  every remaining hit is justified in the commit message.

### M7 — Transition alias binary
- The upgrade script creates `~/.cargo/bin/elanus` as a symlink to `lanius`
  (see M8). No second `[[bin]]` needed. Document that the `elanus` name is
  deprecated and will be removed next cycle.
- **Acceptance:** after the upgrade script runs, both `lanius --version` and
  `elanus --version` work and print the same version.

### M8 — `scripts/upgrade-to-lanius.sh` (HARD DELIVERABLE)
An **idempotent** bash script that upgrades Tim's *live* install in place. Sits
in `scripts/` beside `new-worktree.sh`. Requirements, all mandatory:

- `set -euo pipefail`. Every step is **guarded**: if already done, log
  "already X — skipping" and continue. Re-running is always safe.
- **Refuses nothing silently:** every skip, every fallback, every thing it
  chooses not to auto-fix prints a line explaining why.
- **Resolve paths:** `OLD_ROOT=${ELANUS_ROOT:-$HOME/.elanus/root}`,
  `NEW_ROOT=${LANIUS_ROOT:-$HOME/.lanius/root}`. Takes the repo path as `$1`
  (default: the script's own repo, derived from its location).
- **Step 1 — back up the ledger FIRST.** Before touching anything, copy the db
  and its WAL/SHM siblings to a timestamped backup
  (`<db>.bak-<epoch>`; take the timestamp from `date +%s`). If no db exists yet,
  say so and continue.
- **Step 2 — stop the daemon cleanly.** There is **no pidfile** (grounded:
  `docs/runtime.md` — the daemon is `elanus serve` supervising
  `elanus … daemon --interval-ms …` + the web server). Find it with
  `pgrep -f 'daemon --interval-ms'` and the serve/web processes, send `SIGTERM`,
  wait a bounded few seconds, escalate to `SIGKILL` only if still alive, log each
  action. No match ⇒ "daemon not running — nothing to stop."
- **Step 3 — install the new binary.** `cargo install --path "$REPO" --force`
  (installs `lanius` + the `harness-*` adapter bins into `~/.cargo/bin`). Log the
  resulting `lanius --version`.
- **Step 4 — migrate the root.** If `NEW_ROOT` doesn't exist and `OLD_ROOT`
  does, create `~/.lanius` and `mv "$OLD_ROOT" "$NEW_ROOT"`. If both exist,
  refuse to clobber — print a clear message and leave both. Idempotent: if only
  `NEW_ROOT` exists, "already migrated."
- **Step 5 — migrate the ledger filename.** Inside `NEW_ROOT`, if `lanius.db`
  missing and `elanus.db` (or `harness.db`) present, `mv` it and its `-wal`/`-shm`
  siblings. (The binary also does this at first `open()`, so this is belt-and-
  suspenders — say so.)
- **Step 6 — migrate manifest filenames.** For every
  `"$NEW_ROOT"/packages/*/elanus.toml` (and any user kit manifests), if no
  sibling `lanius.toml`, `mv` it. Content untouched (preserves grant hashes).
- **Step 6b — migrate user-level kits.** If `~/.lanius/kits` is absent and
  `~/.elanus/kits` exists, create `~/.lanius` and `mv ~/.elanus/kits
  ~/.lanius/kits`. If both exist, refuse to merge automatically and print both
  paths for manual reconciliation. Then run the Step 6 manifest rename over the
  moved user kits too.
- **Step 7 — refresh adapters by NEW INODE.** For each
  `"$NEW_ROOT"/packages/harness-*/bin/adapter`: **`rm -f` then `cp`** the matching
  fresh `~/.cargo/bin/harness-<tool>` into place, then `chmod +x`. **Never
  `cp` over the existing inode** — on macOS an in-place overwrite of a running/
  signed Mach-O gets `SIGKILL`'d; a fresh inode avoids it. Log each refreshed
  adapter and assert the new inode differs.
- **Step 8 — rewrite/flag stale generated configs.** The codex hook config is
  per-session and ephemeral (regenerated each launch) — note that and move on.
  Then `grep -rl "$HOME/.elanus" "$NEW_ROOT"` for any *persistent* file still
  embedding the old root path (e.g. a hand-written supervisor snippet); rewrite
  the ones the script safely can (simple path substitution into a temp file then
  move), and **loudly flag** anything ambiguous for Tim to fix by hand rather
  than guessing.
- **Step 9 — create the transition alias** (M7): symlink
  `~/.cargo/bin/elanus → lanius` if absent.
- **Step 10 — restart the daemon** the way it was running (background it with
  `nohup … &`/`disown`, or, if the script couldn't determine how it was
  launched, print the exact `lanius serve` command and skip auto-start). Only
  auto-restart if Step 2 actually stopped one.
- **Step 11 — print a verification checklist**, not just "done":
  `lanius --version`; `NEW_ROOT` exists and holds `lanius.db`; each adapter is
  executable with a fresh inode; daemon up (`pgrep`); a smoke `lanius packages`;
  the db backup path; and the reminder that the `elanus` alias is deprecated.
- **Acceptance:** shellcheck-clean; running it twice in a row is a no-op the
  second time (every step logs "already…"); dry-run reasoning walked through in
  the verify step against a *copy* of a root (NOT Tim's live root — see
  containment).

### M9 — Verification
- `cargo build --release` and `cargo test` green.
- The three data-migration paths each have a test (db filename, manifest
  filename fallback, env back-compat) — reuse/extend the existing precedent
  tests.
- `lanius init <tmp>` in a scratch dir produces a root with `lanius.db`,
  `lanius.toml` manifests, and executable `harness-*/bin/adapter`s; `lanius
  packages` lists them.
- A root pre-seeded the *old* way (`elanus.db`, `elanus.toml`, `~/.elanus/root`)
  is transparently resolved and auto-migrated by the new binary.
- **Acceptance:** all of the above pass; the CLI-string sweep grep (M6) is clean
  modulo documented keeps.

---

## Deliberate KEEPS (do NOT rename)

- `harness-claude` / `harness-codex` / `harness-opencode` / `harness-acp` binary
  names and `src/bin/harness-*.rs`.
- The `harness-doctrine` package and the concept-word "harness" in docs/skills.
- Git history and commit messages.
- Historical **narrative/Log prose** in `docs/handoffs/*.md` and
  `docs/journeys/*.md` (rename only copy-pasteable commands there).
- The `elanus_session` SQL column and JSON wire field (schema/wire migration,
  not mechanical — out of scope).
- The `ELANUS_*` (and `HARNESS_*`) env-var **spellings in the back-compat read
  chain** — they must keep resolving.

---

## Honest residuals

- **`elanus_session` stays.** A future, deliberate schema+wire migration could
  rename it with a serde alias and a column migration; it is not part of this
  mechanical pass. Left as a known inconsistency.
- **localStorage `elanus.theme` → `lanius.theme`** silently resets each user's
  saved theme once. Acceptable; noted.
- **User shell env / hand-written supervisors** that set `ELANUS_ROOT` or embed
  `~/.elanus` keep working (back-compat), but the *canonical* names have moved;
  the upgrade script flags what it finds but can't rewrite a user's `.zshrc` for
  them.
- **The visual pass and SVG logos are not here** (Tim's ruling) — a follow-up.
- **crates.io:** if the crate was ever published as `elanus`, `lanius` is a new
  crate name; no automated redirect. Not relevant unless/until published.

## Read these first

- `src/envcompat.rs` — the env back-compat pattern to extend (M2).
- `src/db.rs::migrate_db_filename` + `src/paths.rs` — the ledger auto-migration
  to mirror (M3).
- `src/manifest.rs::load` (line 419) — the manifest filename touchpoint (M4).
- `src/initcmd.rs::STOCK_HARNESS_PACKAGES` / `seed_stock_harness_packages`
  (line 107, 723) — how adapters are seeded/copied (M1, M8 step 7).
- `src/codeagent.rs::codex_hook_config` (line 1493) — the ephemeral embedded
  root path (M5, M8 step 8).
- `docs/runtime.md` — how the daemon is launched and reaped (M8 step 2).
- `scripts/new-worktree.sh` — the existing bash style to match (M8).

## Log

- 2026-07-06 (Opus, planner): wrote the handoff. Grounded every claim in the
  repo. Key calls: keep `harness-*` names + `elanus_session`; chain env
  back-compat (LANIUS→ELANUS→HARNESS); rename `elanus.db`→`lanius.db` and
  `elanus.toml`→`lanius.toml` with read-time fallbacks mirroring the existing
  db-filename migration; default root honors both `~/.lanius` and `~/.elanus`;
  transition `elanus`→`lanius` symlink for one cycle; doc policy = rename living
  commands, preserve historical prose. Awaiting Fable review before dispatching
  implementation. Impl must run on a clean tree (siblings active now).

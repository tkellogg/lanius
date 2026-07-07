---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: codex-cage — elanus's own fence around the one ungated cell (interim)

Small, interim. While the [codex-app-server.md](codex-app-server.md) driver is
spiked and soaks, headless codex still runs through `codex exec` with **its
vendor sandbox deliberately OFF** — `-c sandbox_mode=danger-full-access` — because
that is the floor an `exec` MCP tool call needs to complete
([../security.md](../security.md) entry 24). That leaves exactly one cell in the
matrix running with **no sandbox at all**. This handoff applies **elanus's own
cage** (the write fence + `network = loopback`, both already built and live-tested
in [single-cage-macos.md](single-cage-macos.md)) to the **headless codex spawn
path only**. The interactive TUI is untouched (a human approves there). This is
the "deliberate step" single-cage explicitly deferred — arrived now, for the one
cell where the vendor gate is off.

## Why this is exactly single-cage's deferred step (not a new decision)

[single-cage-macos.md](single-cage-macos.md) put "bypassing the coding tools' own
sandboxes" out of scope, saying elanus's cage replaces a tool's sandbox only
"after this increment has soaked, as its own deliberate step. Do not touch the
coding spawn paths." That general step is still deferred for claude/opencode
(their own sandboxes are ON). But for **headless codex specifically**, the vendor
sandbox is **already off** (entry 24) — so there is nothing to bypass; there is a
**hole to fill**. Applying elanus's cage here is not a general flip; it is closing
the one cell single-cage's soak was waiting to justify. The mechanism is entirely
reused: `Cage::from_roots_with_policy` with `NetworkPolicy::Loopback` and the
`Protect` fence (`src/sandbox.rs:186-216`, `:279-289`, `:113-140`), proven by
`seatbelt_actually_cages` (`src/sandbox.rs:590`).

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **Scope: the headless codex spawn ONLY.** `run_codex_capture`
   (`src/codeagent.rs:4168`) builds `Command::new(&program)` (`:4209`) with **no
   cage** today. `run_codex_tui_import` (`:4326`) stays untouched (the human
   approves). No other harness/mode changes. *Confirm the narrow scope.*

2. **Cage shape: write = workdir + generated CODEX_HOME, network = loopback.**
   Write roots = the session's `workdir` (a headless worker's whole job is its
   workdir) plus the per-session generated `CODEX_HOME` (codex >=0.142 writes
   startup state there); the `Protect` fence keeps the ledger + secrets
   un-writable/un-readable even inside a root. `network = loopback` (not `none`)
   because the codex child must reach the broker + local HTTP read planes + any
   loopback-HTTP MCP daemon. **My call: `{ write_roots: [workdir,
   codex_home], network: loopback }`, `Protect::for_root`.**
   *Confirm — `none` would cut MCP/HTTP; `open` would defeat egress control.*

3. **macOS-only enforcement, honest off-macOS.** `can_enforce`
   (`src/sandbox.rs:203`, `enforcement_available` `:603`) gates real Seatbelt;
   off macOS the cage is camera-scope only (warned, never silent) — identical
   honesty to single-cage. **My call: enforce on macOS, warn elsewhere.**

4. **`sandbox-exec` wrapper vs `timeout_wrap` + provider injection.** The headless
   spawn wraps the program in a `timeout` (`timeout_wrap`, `src/codeagent.rs:4201`)
   and threads provider-injection env. `Cage::command()` returns a `Command`
   running `sandbox-exec -p <profile> <program>` (`src/sandbox.rs:219-228`). The
   **`sandbox-exec` wrapper must be the outermost layer** so the timeout and the
   real `codex` both run caged. **My call: build the caged command via
   `cage.command()` and thread the existing args/timeout/env onto it**, verifying
   the ordering doesn't drop the timeout or the env. Flag as the one fiddly bit —
   the implementer must check the composed argv, not assume it.

5. **CODEX_HOME / auth reads.** The per-session `CODEX_HOME`
   (`build_codex_skills_home`, symlinks the user's real auth) and the codex
   binary's own reads (config, credentials) must remain **readable** under the
   cage. The default cage is read-open (single-cage did not add read denial by
   default), so this is satisfied as long as we do **not** set `fs_read_allow`
   here. **My call: write + network policy only; no read scoping** (that is a
   later, separate step). Verify auth still resolves under the cage.

## Milestones

### M1 — cage the headless codex spawn
Wrap `run_codex_capture`'s `codex` spawn (`src/codeagent.rs:4209`) in a `Cage`
built with `write_roots = [workdir, codex_home]`, `NetworkPolicy::Loopback`, and
`Protect::for_root(root)`, enforced on macOS (camera-only elsewhere). The TUI
path (`run_codex_tui_import`) is untouched. Preserve the existing timeout wrap,
provider injection, stdin briefing, and env (wonky bit 4).

**Acceptance:** a headless codex worker **cannot write outside its workdir**
(a write to `$HOME/x` under the cage fails; a write inside the workdir succeeds);
the ledger + secrets remain fenced; the TUI spawn is byte-identical to before
(a test asserts the TUI command construction is unchanged). `cargo test` green.

### M2 — network fence + MCP-still-completes verification
Assert the loopback network fence and that it does **not** break MCP. Extend the
live-test discipline of `seatbelt_actually_cages` (`src/sandbox.rs:590`, macOS +
`sandbox-exec` gated).

**Acceptance (live, macOS):** a caged headless codex reaching an **external**
(non-loopback) address **fails**; reaching a **loopback** listener (stand in for
the broker / a `history`-style `http.json` daemon) **succeeds**; a **stdio MCP
server** (the `scratch_ping` pattern from
[mcp-on-launch.md](mcp-on-launch.md)) **completes its tool call** under the cage
— confirming loopback+stdio covers MCP (stdio servers are pipe children, not
sockets; loopback covers any HTTP-port MCP daemon). Skipped, not failed, where
`sandbox-exec` is absent.

### M3 — posture stamp reflects the cage
Update the `session/start` obs record (`src/codeagent.rs:3490-3519`) so that when
the headless codex cage is applied, the stamp records elanus's cage posture
(e.g. an `elanus_cage: { write: "workdir+codex-home", network: "loopback", enforced: <bool> }`
field) **alongside** the existing `approvals: "auto", sandbox:
"danger-full-access"` from `codex_headless_approval_posture` (`:4159`). A
session's authority stays fully reconstructable from its trace — now showing both
that codex's own gate is off **and** that elanus's cage is on.

**Acceptance:** a headless codex `session/start` obs shows the `elanus_cage`
posture (with `enforced` reflecting `can_enforce`); a unit test asserts the stamp
(mirroring the posture-stamp tests); off-macOS the stamp reports `enforced:
false` (never a silent "on"). `cargo test` green.

## Read these first
- The ruling that created the hole: [../security.md](../security.md) entry 24
  (headless codex at `danger-full-access`; the LATENT residual — shell commands
  also run ungated — which this cage narrows).
- The mechanism to reuse (built + live-tested): [single-cage-macos.md](single-cage-macos.md)
  (all of it — the rollout doctrine, wonky bits, and the deferred "deliberate
  step" this handoff is), and `src/sandbox.rs`: `Cage` `:23`, `Protect` `:113-140`,
  `from_roots_with_policy` `:186-216`, `NetworkPolicy`/`CagePolicy` `:37-89`,
  `command()` `:219-228`, `can_enforce`/`enforcement_available` `:203`/`:603`,
  `sbpl()` `:295`, the live test `seatbelt_actually_cages` `:590`.
- The spawn to cage: `src/codeagent.rs:4168-4296` (`run_codex_capture` — the
  `Command::new` at `:4209`, `timeout_wrap` at `:4201`, provider injection
  `:4233`, `CODEX_HOME` `:4262`), `:4326` (`run_codex_tui_import` — the untouched
  TUI), `:4090-4127` (the approval ruling + constants), `:3490-3519` +
  `:4159-4165` (the `session/start` posture stamp to extend).
- The MCP-under-cage question: [mcp-on-launch.md](mcp-on-launch.md) Log (the
  `scratch_ping` stdio server; codex loads MCP but exec cancels the CALL — the
  cage does not change that, it fences what the completed call can then do).
- The successor that eventually removes the bypass entirely:
  [codex-app-server.md](codex-app-server.md).

## Log
- 2026-07-07 — Confirmed shipped+merged on main (M1–M3 all landed, merged in
  `039d640` "s4-codex"); status flipped to `done` (was stale at `planned`).
- 2026-07-03 — Codex 0.142 startup write regression found in the caged headless
  paths: the generated session `CODEX_HOME` is not just read material; codex
  writes startup/client state there and exits under a read-only home. Adjusted
  the cage posture to allow writes to the session-local generated `CODEX_HOME`
  in addition to the workdir, and stamp it as `write: "workdir+codex-home"`.
  Auth symlinks inside that home still resolve to the user's real codex home,
  which remains outside the write roots; `Protect::for_root` still fences the
  ledger, bus file, config, profiles, and secrets. Verified with the focused
  live macOS cage tests and full `cargo test --lib`.
- 2026-07-02 — Decomposed from security.md entry 24 + single-cage-macos.md's
  deferred "deliberate step" by Opus (planner) under Fable. Grounded against the
  sprint-4 worktree: `run_codex_capture` (`src/codeagent.rs:4168`) spawns `codex`
  **uncaged** (`Command::new` at `:4209`); the cage mechanism
  (`Cage::from_roots_with_policy` + `NetworkPolicy::Loopback` + `Protect`) is
  already built and live-tested (`src/sandbox.rs`, `seatbelt_actually_cages`
  `:590`); the `session/start` posture stamp (`:3490-3519`, `:4159`) is where the
  cage posture is recorded. This is single-cage's deferred coding-agent step,
  applied ONLY to the one cell whose vendor sandbox is deliberately off. Judgment
  calls flagged: headless-codex-only scope (1), write=workdir + loopback (2),
  macOS-only enforce (3), sandbox-exec must wrap timeout+env (4), no read scoping —
  keep auth readable (5). Interim to codex-app-server, which removes the bypass
  (and thus the need for this cage) where the driver is active.

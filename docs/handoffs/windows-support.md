---
status: proposed
author: Fable (planner) in Claude Code on Elanus
last-updated: 2026-07-11
---

# Handoff: Windows support — compile + run lanius natively on Windows

GitHub issue [#1](https://github.com/tkellogg/lanius/issues/1) (HurricanKai):
"Lanius doesn't start on Windows. It uses some linux specific commands. I'm
trying it under WSL instead :)". Today WSL is the only Windows story. This
handoff ladders lanius to a **native Windows (MSVC)** build and run, fencing the
subsystems that stay Unix-only behind honest cfg gates and runtime errors rather
than silent wrong behavior.

The good news up front, from the spike: **nothing here is architecturally
Unix.** The daemon is not a classic double-fork daemon — it is a foreground
`std::process::Command`-spawning supervisor/tick loop. Every real blocker is a
libc call (`kill`/`killpg`/`setpgid`/`flock`/`signal`/`access`) or a bare
`std::os::unix::*` import at a countable set of call sites, plus a POSIX-shell
assumption in the package-script model and two ungated `symlink()` calls. A large
fraction of the process-lifecycle code (all of `dev.rs`, the config-repo compat
symlink, several `write_0600`/`set_executable` helpers) is **already**
`#[cfg(unix)]`/`#[cfg(not(unix))]` paired — so the surface is smaller than feared.

## Wonky bits / decisions to confirm

1. **TLS/crypto: drop `aws-lc-sys` on Windows (recommended) vs require MSVC+NASM.**
   The very first thing that breaks is not lanius source — it is the transitive C
   crypto crate `aws-lc-sys 0.41.0` (pulled in via `reqwest 0.13` →
   `rustls 0.23` / `hyper-rustls` → `aws-lc-rs`). A `cargo check
   --target x86_64-pc-windows-msvc` from macOS dies compiling
   `jitterentropy-base-windows.h` with `fatal error: 'windows.h' file not found`
   — it never reaches Rust. On a *native* Windows MSVC box `windows.h` exists, so
   aws-lc-sys does build there, **but it additionally needs NASM on PATH** and a
   full C toolchain — a heavy ask for a `cargo install lanius` user on Windows.
   The clean lever: configure `reqwest` to use **rustls with the `ring` backend**
   (`default-features = false`, `features = ["json", "rustls-tls"]` pinned to a
   ring-backed rustls) *or* the OS-native SChannel (`default-tls`). Either drops
   `aws-lc-sys` entirely, which both unblocks the cheap mac cross-check and spares
   Windows users the NASM/C-toolchain install. **Fable/Tim: confirm we may change
   the TLS backend.** `genai` and `ntex`/`ntex-mqtt` also pull rustls; verify the
   whole graph lands on one ring-backed rustls (run `cargo tree -i aws-lc-sys`
   after the change to prove it's gone). If we keep aws-lc, M1 acceptance must
   include "NASM documented as a Windows build prerequisite" and the CI Windows
   job must install it.

2. **One `platform` abstraction module, not cfg-gates sprinkled at 12 sites.**
   The process-lifecycle unix-isms (spawn-in-own-group, kill-the-whole-tree,
   is-this-pid-alive, set-executable, symlink-or-copy, writable-probe,
   null-device path, install-shutdown-handler) recur across `exec.rs`,
   `hooks.rs`, `dispatcher.rs`, `dev.rs`, `codesession.rs`, `packages.rs`,
   `web.rs`, `codeagent.rs`, `main.rs`. Rather than gate each inline, add
   `src/platform/mod.rs` with `#[cfg(unix)]` and `#[cfg(windows)]` sibling
   implementations behind a small portable API, and rewrite the call sites to it.
   This keeps the kernel discipline (one place owns "how we kill a tree") and
   makes the Windows impl reviewable in isolation. *Confirm this shape vs. inline
   gates.*

3. **Process groups → Windows Job Objects.** The whole worker lifecycle rests on
   "spawn the child in its own process group (`process_group(0)` / `setpgid`),
   then kill/​signal the *group* so the sandbox-exec wrapper and any shell
   descendants die too." Windows has no process groups in this sense; the
   equivalent is **`CreateJobObject` + `AssignProcessToJobObject`** with
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and **`TerminateJobObject`** to kill the
   tree. Liveness (`libc::kill(pid, 0)`) → `OpenProcess` + `GetExitCodeProcess ==
   STILL_ACTIVE`. Recommend the `windows` crate (or `winapi`) as a
   `[target.'cfg(windows)'.dependencies]`. `libc` can stay a `cfg(unix)`
   dependency. *Confirm `windows` crate is acceptable.*

4. **Package-script execution: require a POSIX shell on PATH (M3), don't rewrite
   to `cmd`.** Package tools are `scripts/*` files with `#!/bin/sh` shebangs, and
   the host `shell` exec tool is literally documented as "Run a shell command on
   the host via sh -c" (`exec.rs:1621`, `sandbox.rs:258`). Shebangs are inert on
   Windows and there is no `sh`. Cleanest first cut: **invoke scripts as `sh -c`
   / `sh <script>` against a POSIX shell we require on PATH** (Git-for-Windows
   bash, or the user's WSL `sh`) and fail with an honest "install Git Bash / a
   POSIX shell" error when absent — rather than porting every stdlib script to
   PowerShell. *Confirm the "require a shell" posture for M3; the alternative
   (dual-language scripts) is much larger and out of scope here.*

5. **Symlinks → symlink-with-copy-fallback.** Windows 10+ *can* make symlinks but
   only with Developer Mode or the `SeCreateSymbolicLink` privilege; directory
   **junctions** need no privilege. For the ephemeral, per-session skill links
   (`codeagent.rs:1475` `link_skill_packages`, `codeagent.rs:1513`
   `build_codex_skills_home`), prefer **copy** on Windows (they're regenerated
   each session and `remove_dir_all`'d at exit — losing live-reflect is
   acceptable), or a junction for the dir case. The config-repo `<root>/profiles`
   compat symlink (`config_repo.rs:163`) **already** has a `#[cfg(not(unix))]`
   copy fallback — leave it.

6. **What stays degraded on Windows, honestly.** (a) **The OS sandbox / cage is a
   no-op passthrough** — it only ever engages on macOS + `sandbox-exec`
   (`sandbox.rs:646`); on Windows it degrades exactly as it already does on Linux
   (unsandboxed, `Command::new(program)` directly). Not a regression, but say so.
   (b) The **cross-process budget `flock`** (`codesession.rs:2129`) is unix-only;
   M1 can degrade it to best-effort (or `LockFileEx` later) with a warning.
   (c) The **`ps -axo` stray-process sweep** (`dev.rs:734`, already
   `#[cfg(not(unix))]` no-op) stays Unix-only — the Job Object covers the
   daemon's own tree, which is what matters. WSL stays fully supported and is the
   recommended path until M2/M3 land.

## Tiered unix-ism inventory (spike, file:line anchored)

### Already cfg-gated (no work beyond a Windows impl where noted)
- `dev.rs`: `set_process_group` (767/780), `cleanup_root_processes` (709/731),
  `root_processes` (734), `signal_pids` (755), `terminate_child_group` (782/789),
  `kill_child_group` (796/803) — full `unix`/`not(unix)` pairs. **But
  `install_signal_handlers` (324) and its `request_shutdown` extern-C handler are
  NOT gated** (bare `libc::signal`) — see Tier A.
- `config_repo.rs` `ensure_agent_store` (163–215): `unix` symlink /
  `not(unix)` copy — done.
- `codeagent.rs`: `process_group` (1108, `#[cfg(unix)]`), `ExitStatusExt`
  (12395/12534, gated).
- `codesession.rs`: `BudgetLock` flock (2140, `#[cfg(unix)]`), `PermissionsExt`
  (2275, gated), `write_0600` (2610, gated).
- `provider.rs` `write_0600` (485), `secrets.rs` `write_0600` (155),
  `initcmd.rs` `set_executable` (854) — all `#[cfg(unix)]`.

### TIER A — trivially cfg-gateable (small, isolated; gate + tiny Windows shim)
- `main.rs:882` — `libc::signal(SIGPIPE, SIG_DFL)`. SIGPIPE doesn't exist on
  Windows. Gate out under `#[cfg(unix)]`.
- `dev.rs:324–338` `install_signal_handlers` + `dev.rs:15` `request_shutdown` —
  bare `libc::signal(SIGINT/SIGTERM/SIGHUP)`. Windows: `ctrlc` crate or
  `SetConsoleCtrlHandler`; small Tier-B for clean `serve` shutdown (see M2).
- `packages.rs:674` `make_executable` (`PermissionsExt` / `set_mode(|0o755)`) —
  bare `use std::os::unix::fs::PermissionsExt`. Gate; Windows no-op (exec bit is
  meaningless there — the shell strategy in M3 handles run-ability).
- `web.rs:1511–1515` — `libc::access(W_OK)` writability probe (mirrors Node's
  `fs.accessSync`). Gate; Windows impl = attempt-open / readonly-attr check.
- `git_hardened.rs:26,35,36` — `core.hooksPath=/dev/null`,
  `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`. On Windows the
  null device is `NUL`. Route through `platform::null_device()`.
- `codesession.rs:2560` `pid_alive` (`libc::kill(pid,0)` / `ESRCH`) — the fd
  struct above it is gated but this helper needs its own Windows impl
  (`OpenProcess`/`GetExitCodeProcess`). Small, via `platform::is_alive`.

### TIER B — needs a real Windows-equivalent implementation
1. **Process-group spawn + kill-tree + reap** (the inetd-style core). Bare,
   ungated:
   - `exec.rs:2454` `use CommandExt`, `2466` `process_group(0)`, `2495`
     `libc::killpg(pid, SIGKILL)` — the host `shell` tool's timeout kill.
   - `hooks.rs:120` `CommandExt`, `130` `process_group(0)`, `162`
     `libc::killpg(pid, SIGKILL)` — hook invocation timeout kill.
   - `dispatcher.rs:16` top-level `use CommandExt`, `491` `process_group(0)`
     (booting a package daemon actor), `393` `libc::killpg(child, SIGKILL)`
     (kill-on-config-reload), `594` `libc::kill(pid,0)` (lease liveness reap).
   Windows: `platform::spawn_in_group` (Job Object), `platform::kill_group`
   (`TerminateJobObject`), `platform::is_alive`. This is the load-bearing item —
   M2 hinges on it.
2. **`sh -c` shell + shebang package scripts.**
   - `sandbox.rs:250–262` `shell_command` (`sh -c cmd`, or under sandbox-exec on
     macOS), `exec.rs` `run_shell` (2447) + the `shell` tool description
     (`exec.rs:1621` "via sh -c").
   - `scripts/*` package tools ship `#!/bin/sh` shebangs
     (`packages.rs`, `manifest.rs`, `kit.rs`, `discover.rs`, `pkgtool.rs`,
     `hooks.rs` all write `#!/bin/sh` tool scripts). Windows can't honor a
     shebang or an exec bit.
   Windows: require a POSIX `sh` on PATH and invoke `sh -c` / `sh <script>`
   (decision #4). M3.
3. **Symlink materialization** (compile blockers — bare, ungated):
   - `codeagent.rs:1475` `link_skill_packages` — `std::os::unix::fs::symlink`.
   - `codeagent.rs:1513` `build_codex_skills_home` — symlinks codex
     `auth.json`/`version.json` into the ephemeral home.
   Windows: `platform::link_or_copy` (junction/dev-mode symlink, else copy). M4.
4. **`flock` budget lock** — `codesession.rs:2143` (`RawFd`), `2163`/`2179`
   `libc::flock(LOCK_EX/LOCK_UN)`. Already `#[cfg(unix)]`; needs a `#[cfg(windows)]`
   `LockFileEx` impl or an honest best-effort degrade (M1 can degrade + warn; M2+
   proper).
5. **TLS/crypto build dependency** — `aws-lc-sys` (decision #1). Not a source
   unix-ism but the *first* thing that fails a Windows build; belongs in M1.

### TIER C — architecturally Unix
**None.** Honest assessment: the daemon is spawn-based, not fork-based; process
groups map cleanly to Job Objects; the sandbox is already a passthrough off
macOS; the shell assumption is satisfiable with a required POSIX shell. The
closest to "won't be ported" is the **OS sandbox/cage**, which simply stays a
no-op on Windows exactly as on Linux — that's a documented degrade, not a Tier-C
impossibility.

## The daemon / "inetd" model today (so the Windows impl is grounded)

- **`lanius serve` (`dev.rs`)** is a *foreground* supervisor: it `Command::spawn`s
  service children, tees their stdout/stderr, restarts them on exit, installs
  SIGINT/SIGTERM/SIGHUP handlers to flip a shutdown flag, and on shutdown signals
  each child's process group then runs a `ps`-based stray sweep. Not a
  double-fork daemon.
- **The dispatcher (`dispatcher.rs run()`)** is the kernel tick loop: reconcile
  orphaned `running` rows, then each tick **boot** discovered `process.mode =
  "daemon"` package actors as **caged child processes in their own process group**
  (`process_group(0)`, `dispatcher.rs:491`), allocate each a `127.0.0.1:0` HTTP
  port, **kill+restart** an actor whose config fingerprint changed
  (`killpg(SIGKILL)`, `:393`), and **reap** dead leases/workers via signal-0
  liveness (`kill(pid,0)`, `:594`). Coding-session workers are spawned children;
  a dead wrapper is noticed by the same liveness probe and its claims reaped.
- **Windows translation:** spawn stays (`std::process::Command`); "own process
  group" → a per-child **Job Object**; "kill the group" → `TerminateJobObject`;
  "is pid alive" → `OpenProcess`/`GetExitCodeProcess`. One `platform` module
  covers every call site. No architectural change to the tick loop.

## Milestones

### M1 — Compiles on Windows (MSVC), degraded subsystems fenced
- Add `src/platform/{mod,unix,windows}.rs` exposing: `spawn_in_group`,
  `kill_group`, `is_alive`, `set_executable` (unix real / windows no-op),
  `link_or_copy`, `is_writable`, `null_device`, `install_shutdown_handler`.
- cfg-gate / route through `platform` every Tier-A and Tier-B(1,3,4) site above
  (`exec.rs`, `hooks.rs`, `dispatcher.rs`, `packages.rs`, `web.rs`, `main.rs`,
  `dev.rs` handlers, `codeagent.rs` symlinks, `codesession.rs` `pid_alive`/flock).
- Resolve TLS (decision #1): move `reqwest`/rustls off `aws-lc-sys` to ring or
  SChannel; prove with `cargo tree -i aws-lc-sys` (empty) — or document NASM.
- Make `libc` a `[target.'cfg(unix)'.dependencies]` entry and add the `windows`
  crate under `[target.'cfg(windows)'.dependencies]`.
- **Acceptance:** `cargo build --target x86_64-pc-windows-msvc` links (from mac
  once aws-lc is gone, or on a `windows-latest` runner), and `cargo test`
  compiles for the target. Every unsupported subsystem (cage, flock, `ps` sweep)
  returns an honest runtime error or a warned best-effort degrade — no silent
  wrong behavior. Unix `cargo test` count unchanged (610).

### M2 — Core daemon + `serve` run on Windows
- Job-Object process groups wired end-to-end: booting a package daemon actor,
  HTTP-port allocation, config-change kill+restart, dead-worker reap, and clean
  Ctrl-C shutdown of the whole tree (`install_shutdown_handler` via
  `SetConsoleCtrlHandler`/`ctrlc`).
- **Acceptance:** on Windows, `lanius init` succeeds; the dispatcher boots; a
  stdlib `process.mode="daemon"` package actor comes up and binds its loopback
  HTTP port; the web UI serves on `127.0.0.1`; Ctrl-C tears the tree down with no
  orphaned child processes (verify via Task Manager / `Get-Process`).

### M3 — Shell + package-script execution on Windows
- Adopt the "require a POSIX `sh` on PATH" posture (decision #4): route
  `run_shell`/`shell_command` and `scripts/*` tool invocation through `sh -c` /
  `sh <script>`; emit an honest "install Git Bash / a POSIX shell" error when
  absent.
- **Acceptance:** on a Windows box with Git Bash installed, a stdlib package
  `scripts/main` tool executes and returns output, and the host `shell` exec tool
  runs a command. On a box without a shell, the error names the fix.

### M4 — Skills / kit materialization on Windows
- `link_skill_packages` + `build_codex_skills_home` use `platform::link_or_copy`
  (junction/dev-mode symlink where available, copy otherwise).
- **Acceptance:** `lanius code` on Windows (where the underlying coding harness
  runs) materializes a profile's skill package and the worker sees it; the
  ephemeral session home is cleaned at exit.

### M5 — CI: a Windows job to prevent regression
- Extend `.github/workflows/ci.yml` (currently a single `ubuntu-latest` `cargo
  test` job) into a matrix adding `windows-latest`: `dtolnay/rust-toolchain@stable`,
  `Swatinem/rust-cache`, `actions/setup-node`, then `cargo build` (+ `cargo test`
  for the portable subset). If aws-lc is retained (decision #1 rejected), add
  `ilammy/setup-nasm` before the build. Keep the existing Ubuntu leg green.
- **Acceptance:** a green `windows-latest` leg on PRs; a reintroduced bare
  unix-ism fails the Windows build in CI.

## Residuals / deferred (be honest)
- **OS sandbox/cage:** stays a no-op passthrough on Windows (== Linux today). No
  Windows AppContainer/Job-Object-sandbox planned here.
- **`flock` budget lock:** M1 may ship a best-effort degrade with a warning;
  proper `LockFileEx` is a follow-up if contention matters.
- **`ps` stray-process sweep (`dev.rs`):** stays Unix-only; the Job Object covers
  the daemon's own tree.
- **WSL:** remains fully supported and is the recommended path until at least M2.

## Read these first
- Issue [#1](https://github.com/tkellogg/lanius/issues/1).
- The daemon/tick loop: [../../src/dispatcher.rs](../../src/dispatcher.rs)
  (`run()`, actor boot ~408–508, reload kill :393, lease reap :594).
- Supervisor + already-gated process helpers: [../../src/dev.rs](../../src/dev.rs)
  (:300–340, :709–808).
- Shell + cage: [../../src/exec.rs](../../src/exec.rs) (`run_shell` ~2447),
  [../../src/sandbox.rs](../../src/sandbox.rs) (`command`/`shell_command`
  234–263, `available()` :646).
- Symlink materialization: [../../src/codeagent.rs](../../src/codeagent.rs)
  (`link_skill_packages` 1467, `build_codex_skills_home` 1493).
- The compat-symlink precedent (unix/not-unix pattern to copy):
  [../../src/config_repo.rs](../../src/config_repo.rs) (`ensure_agent_store` 163).
- TLS graph: `Cargo.toml` deps + `cargo tree -i aws-lc-sys`.
- CI: [../../.github/workflows/ci.yml](../../.github/workflows/ci.yml).

## Log
- 2026-07-11 (Fable, planner): spike complete. Cross-check
  `cargo check --target x86_64-pc-windows-msvc` from macOS fails in
  `aws-lc-sys` C build (`windows.h` not found) before reaching lanius source —
  so no Rust-level lanius errors were collected; the blockers are enumerated
  statically and are all libc/`std::os::unix` call sites, the POSIX-shell script
  model, and two ungated `symlink()` calls. No source modified. Handoff proposed;
  chainlink issue + GH reply draft filed alongside. Key surprise: much of the
  process-lifecycle code is already `cfg(unix)`/`cfg(not(unix))` paired, and the
  daemon is spawn-based (no true fork), so no Tier-C blockers exist.

# Changelog

All notable changes to lanius are documented here.

## 0.1.2 — 2026-07-13

### Added
- **Coding-session DMs join the chat plane** (#4): workers message the owner with
  `lanius code send`, deliveries and replies fold into real chat threads with a
  "coding session" chip, and both the conversation list and the opened thread are
  sender-verified — a payload claiming a worker session renders only when the
  broker-verified sender IS that worker.
- **Worker legibility** (#8): Runs rows lead with each session's intent (model and
  effort spelled out, "? / ?" removed), sessions record who launched them, History
  status is honest three-state (reachable / unreachable / absent) with repair hints,
  workers with chat traffic open on Chat by default, and visible copy renames
  converse → Chat.
- **Windows support, first slice** (#7 M1+M5): the full crate compiles for
  x86_64-pc-windows-msvc behind a platform module that fences unix-isms; TLS is
  target-scoped (unix keeps static rustls, Windows uses SChannel via native-tls);
  CI gains a windows-latest leg and release binaries build natively per platform.

### Fixed
- **Session reliability** (#2): `lanius code whose` reports evidence-based
  Viewer/Other/Unknown instead of guessing "likely yours"; claim and unclaim resolve
  through one canonical path; a crashed session's advisory edit claims are reaped at
  the next roommate turn instead of haunting the room (#10 field evidence).
- **Resumed workers regain their identity** (#11): a driven resume now passes
  session/agent/root/bus-token to the child, so `lanius code inbox` and hooks work
  inside resumed turns.
- **Messages pane legibility** (#12): full message bodies expandable in place;
  readable worker names (Claude worker / Codex worker / System) with the raw id
  demoted; details popover and Open-run action.
- `lanius dev` shifts busy ports by default (`--fixed-ports` opts out), so a second
  dev instance just works (#2 Phase A).

## 0.1.1 — 2026-07-10

### Fixed

### Added

- **`--trusted-host` for the web UI** (`lanius serve` / `lanius web` /
  `lanius dev`): allow additional hostnames through the Host/Origin guard, so
  the dashboard can be reached by a name other than localhost (a LAN name, a
  tailnet name, a reverse proxy). Loopback names remain trusted by default;
  repeat the flag for multiple names.

### Changed
- broker: rate-limit repeated CONNECT refused logs (#14)

- `Cargo.toml` now declares `rust-version = "1.88"`, so installing with an
  older toolchain fails with a clear "requires Rust 1.88" message instead of a
  confusing mid-build error deep in a dependency.
- README: an Install → Prerequisites section documenting the two real
  `cargo install` requirements — Rust 1.88+ and a C compiler (lanius bundles
  SQLite and compiles it from source).

## 0.1.0 — 2026-07-09

Initial release. A local control plane for AI work: event-driven agent
orchestration with a flight recorder (inetd + cron + git hooks + sqlite).

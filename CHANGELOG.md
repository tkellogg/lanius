# Changelog

All notable changes to lanius are documented here.

## 0.1.1 — 2026-07-10

### Added
- Plan coding-session reliability sprint (#2)

- **`--trusted-host` for the web UI** (`lanius serve` / `lanius web` /
  `lanius dev`): allow additional hostnames through the Host/Origin guard, so
  the dashboard can be reached by a name other than localhost (a LAN name, a
  tailnet name, a reverse proxy). Loopback names remain trusted by default;
  repeat the flag for multiple names.

### Changed
- worker-dm-unification: unify coding-session DMs into the chat plane (#4)

- `Cargo.toml` now declares `rust-version = "1.88"`, so installing with an
  older toolchain fails with a clear "requires Rust 1.88" message instead of a
  confusing mid-build error deep in a dependency.
- README: an Install → Prerequisites section documenting the two real
  `cargo install` requirements — Rust 1.88+ and a C compiler (lanius bundles
  SQLite and compiles it from source).

## 0.1.0 — 2026-07-09

Initial release. A local control plane for AI work: event-driven agent
orchestration with a flight recorder (inetd + cron + git hooks + sqlite).

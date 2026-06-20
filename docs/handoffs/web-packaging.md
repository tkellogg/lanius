# Handoff: ship the web UI inside the Rust binary (`cargo install elanus`)

Make `cargo install elanus` produce a binary that serves the web UI with **no
Node.js, no npm, and no source tree at runtime** — so anyone who never cloned the
repo gets a working `elanus serve`. Today the web stack is bolted to the build
machine's checkout; this handoff folds it into the binary.

Answers Tim's question in `docs/_questions.md` ("Rust + web packaging").

## The bug today

`elanus serve` ([../src/dev.rs](../src/dev.rs) `serve()`) is unshippable off the
build host for three compounding reasons:

1. **Compile-time path.** `let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))`
   (dev.rs:143) bakes the *build* directory (`~/code/elanus`) into the binary and
   looks for `ui/web` there. On an installed machine that path doesn't exist.
2. **Node + npm at runtime.** It spawns `node ui/web/server.mjs` (dev.rs:172) and,
   if `dist/index.html` is missing, runs `npm run build` (`ensure_dist_built`,
   dev.rs:224). Two toolchains a `cargo install` user won't have.
3. **The SPA isn't even in the crate.** `ui/web/dist/` is gitignored
   (`.gitignore:18`) and `Cargo.toml` has no `include`, so `cargo publish` ships
   neither the built SPA nor `ui/web` source. The published crate has nothing to
   serve.

## Why this is very doable (no new heavy deps)

Everything `server.mjs` does has a first-party equivalent already in the
dependency tree — the port adds an HTTP module, not a new platform:

| `server.mjs` responsibility | Rust equivalent (already a dep) |
|---|---|
| Static file serving from `dist/` | `ntex::web` (ntex 3.9.6 is already in the tree for the broker) serving an **embedded** dist |
| Live bus → browser (SSE relay of `obs/# in/# signal/#`) | `rumqttc` client (the pattern is `src/buscli.rs`) fanned out to SSE responders |
| `POST /api/publish` (browser → bus) | same `rumqttc` client, publish with correlation/props |
| `/api/conversations*` (read `elanus.db`) | `rusqlite` (bundled) — the exact queries already exist in JS; this is a direct translation |
| `/api/history` proxy to the history package | `reqwest` (already a dep), or read sessions/transcripts straight from `elanus.db` in Rust and drop the proxy |
| `/api/admin/*` shelling to the `elanus` CLI | call the same Rust functions in-process (no subprocess) — or, phase 1, spawn `current_exe()` |
| `/api/status`, `/api/models`, owner credential | already Rust logic (`src/secrets.rs`, `src/models.rs`, `src/paths.rs`) |

Net: the browser bundle (`ui/web/src`) is unchanged — it still talks SSE + JSON
over HTTP. Only the *server* moves from `server.mjs` to a Rust module. Node stays
a **dev-only** dependency (Vite + `npm run dev`); it leaves the shipped artifact.

## Design decisions (recommendations)

- **Transport: keep SSE + POST, do not move the browser onto MQTT-over-WebSocket.**
  A WS-direct-to-broker browser would drop the relay, but it also drops the
  Origin/Host CSRF gate and the `decided_by=ui` ledger trail that `server.mjs`
  documents as the *only* real browser-specific boundary, and it still needs an
  HTTP server for static files + admin + conversations. Porting SSE+POST 1:1
  preserves the security model and changes zero browser code. (Revisit WS later
  only if relay latency ever matters.)
- **Admin: phase the shell-out away.** `server.mjs` shells `elanus <verb>` for
  every gesture. In-process Rust calls are the end state (faster, no subprocess,
  one code path), but they're a bigger refactor. Phase 1 can spawn
  `std::env::current_exe()` for admin to reach parity fast, then inline verb by
  verb. Keep `decided_by=ui` on every mutating route throughout.
- **Embedding: build at publish, embed at compile, serve from memory.** Use
  `include_dir!`/`rust-embed` over `$CARGO_MANIFEST_DIR/ui/web/dist`. Because
  `dist` is a build output, the **publish workflow** must (a) `npm run build`, (b)
  add `dist` to `Cargo.toml`'s `include = [...]` so it ships in the crate, (c)
  `cargo publish`. The compiled binary then carries the SPA; `cargo install`
  pulls it down with no Node, no npm, no checkout.

## Milestones

### M1 — Rust web module (parity, behind a flag)
A new `src/web.rs` on `ntex::web`: serve the embedded `dist`, an SSE endpoint
relaying a `rumqttc` subscription (`obs/# in/# signal/#`), `POST /api/publish`,
and the `/api/conversations*` + `/api/status` + `/api/models` endpoints. Admin and
history may still shell to `current_exe()` / proxy via `reqwest` this milestone.
Wire it as `elanus web --root <root> --port <n>` so it can run beside the existing
node server for A/B.
**Acceptance:** with the daemon up, `elanus web` serves the same SPA and the
Playwright suite (`ui/web/test/ui.spec.mjs`) passes against it with `node` not on
PATH for the server process.

### M2 — `serve` uses the Rust web server; drop the Node runtime dep
Replace the `node server.mjs` service in `serve()` (dev.rs:170) with the in-process
/ spawned Rust web server. Remove the `CARGO_MANIFEST_DIR` web-path dependency and
`ensure_dist_built`'s `npm` call (the SPA is embedded). `elanus dev` keeps Vite +
`npm` for the hot-reload dev loop.
**Acceptance:** `elanus serve` runs with no `ui/web` directory present and no
`node`/`npm` on PATH.

### M3 — Publishable crate
Add `include = ["ui/web/dist/**", ...]` to `Cargo.toml`; document/script the
publish order (build SPA → publish). Verify from a clean machine (or a scratch
`CARGO_HOME`) that `cargo install --path .` then a simulated registry install both
yield a working `elanus serve`.
**Acceptance:** a binary built with no sibling `ui/web` source serves the UI.

### M4 (optional) — inline admin + history; retire `server.mjs`
Translate the admin gestures and history reads to in-process Rust (direct config/
kit/profile calls; `elanus.db` reads). Delete `server.mjs` and `config.mjs` once
the Rust server is the only one. Keep `mqtt`/`node:sqlite`-based JS only if any
dev tooling still needs it.
**Acceptance:** no `.mjs` server in the shipped path; `npm` appears only under
`ui/web` dev scripts.

## Interim cheap mitigation (if a full port isn't scheduled yet)

Even before M1, `serve()` should stop trusting `CARGO_MANIFEST_DIR`: resolve web
assets from a runtime location (next to `current_exe()`, an `ELANUS_WEB_DIR`, or a
known share dir) and fail with a clear message if absent. This does **not** remove
Node — it only stops the binary from pointing at a build host that may not exist —
so treat it as a stopgap, not the fix.

## Read these first

- [../src/dev.rs](../src/dev.rs) — `serve()` (the bug) and the Service/CommandSpec
  supervision it reuses.
- [../ui/web/server.mjs](../ui/web/server.mjs) — the full set of responsibilities
  to port (static, SSE, publish, admin, history, conversations, status, models,
  the Origin/CSRF gate).
- [../src/buscli.rs](../src/buscli.rs) — the `rumqttc` client pattern the SSE
  relay and publish route reuse.
- [../docs/security.md](security.md) — entries on local-channel authority and the
  browser CSRF/Origin boundary the port must preserve.

## Log

- 2026-06-20 — Written from a code read after Tim flagged the packaging gap in
  `docs/_questions.md`. Confirmed: the blocker is `CARGO_MANIFEST_DIR` + the
  `node`/`npm` runtime + gitignored, unpublished `dist`. Key finding: a Node-free
  Rust server needs **no new heavy deps** — `ntex`, `rumqttc`, `rusqlite`, and
  `reqwest` are already present, and the just-landed `/api/conversations` sqlite
  endpoints prove the direct-`elanus.db` read pattern.

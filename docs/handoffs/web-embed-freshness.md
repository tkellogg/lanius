---
status: planned
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: the binary always embeds a fresh web UI (H1)

`elanus web` serves a SPA embedded at compile time via `include_dir!`
(src/web.rs:41,59; Cargo.toml:48). There is no build.rs anywhere, so `cargo
build` / `cargo install --path .` never rebuilds `ui/web/dist` — a UI change
followed by a rebuild silently ships the OLD interface. This has bitten twice
(a verify pass false-greened against a stale binary; see the memory note
"web embed staleness gotcha"). Tim's direct ask: building the binary must
always produce a fresh embed, without breaking builds on machines that have
no Node at all.

## Dependency edges

- Independent of every other handoff. Runs FIRST on the sprint branch so the
  "rebuild dist, touch web.rs, rebuild" ritual can be dropped from all later
  verification prompts.

## Read these first

1. src/web.rs:35-60 — the `include_dir!` embed and the comment around it.
2. Cargo.toml:11-19 (the manual build-ordering note) and :48 (`include_dir`).
3. ui/web/package.json — the `build` script (vite build → `ui/web/dist`).
4. The staleness trap this kills: after any UI change today you must run
   `cd ui/web && npm run build && touch src/web.rs && cargo build` or e2e
   tests exercise a stale SPA.

## Wonky bits / decisions (already made — do not relitigate)

1. **`ui/web/dist` stays committed.** Machines without Node (CI, a plain
   `cargo install`) must still build. The committed dist is the fallback
   artifact, not a mistake.
2. **No npm ⇒ loud warning, never a hard failure.** If `npm` is not on PATH
   (or `ui/web/src` does not exist — e.g. a source tarball without the UI
   tree), build.rs emits `cargo:warning=...` saying the committed dist is
   being embedded as-is and how to fix it, then succeeds. The runtime
   Node-free guarantee is untouched — Node is only ever a *build-time*
   convenience.
3. **Watch the source tree, not the output tree.** build.rs must emit
   `cargo:rerun-if-changed=ui/web/src` (plus `ui/web/package.json`,
   `ui/web/index.html`, `ui/web/vite.config.*`). It must NOT emit
   rerun-if-changed on `ui/web/dist`: build.rs writes into dist, and watching
   a directory you write causes rebuild-every-time. `include_dir!` re-reads
   dist bytes whenever the crate recompiles, and build.rs rerunning (because
   src changed) is exactly what forces that recompile — the chain closes
   without watching dist.
4. **Skip the npm run when nothing changed.** cargo already handles this via
   rerun-if-changed; do not add your own mtime comparison logic on top.
5. **Two distinct failure cases — do not conflate them.**
   - A failing `npm --prefix ui/web install` (dependency FETCH — airgapped
     machine, network hiccup, registry down) → WARN LOUDLY and fall back to
     the committed dist. The dist is a valid artifact; hard-failing
     `cargo install` on a network problem is worse than the warning.
   - A failing npm run BUILD with npm + deps present → panic with the npm
     output. A present-but-broken UI build is a real error; falling back
     silently would re-create the staleness bug with extra steps. The panic
     message must mention the `LANIUS_SKIP_UI_BUILD=1` escape hatch (wonky
     bit 7).
6. **Use `npm --prefix ui/web run build`** (no `cd`), and prefer
   `npm ci`-less operation: if `node_modules` is missing, run
   `npm --prefix ui/web install` first (warn that it is doing so).
7. **Escape hatch: `LANIUS_SKIP_UI_BUILD=1`.** When set, build.rs skips the
   npm step entirely and embeds the committed dist, with the same loud
   `cargo:warning`. Someone will need it (broken local Node, CI oddity),
   and it beats them hacking build.rs. Documented in the panic message and
   in the M2 doc updates.

## Milestones

### M1 — build.rs

Add `build.rs` at the repo root implementing the decision table above:

- `LANIUS_SKIP_UI_BUILD=1` set → skip the npm step, loud warning, succeed
  (and `cargo:rerun-if-env-changed=LANIUS_SKIP_UI_BUILD`).
- `ui/web/src` exists AND `npm` resolves on PATH → ensure deps (install if
  `node_modules` missing; a FAILED install warns loudly and falls back to
  the committed dist), then run the UI build into `ui/web/dist`; panic with
  output on a non-zero BUILD exit, naming `LANIUS_SKIP_UI_BUILD=1` as the
  escape hatch.
- otherwise → `cargo:warning=web UI: embedding the committed ui/web/dist
  as-is (npm not found / no ui source); install Node and rebuild to refresh
  the interface` and succeed.
- emit the rerun-if-changed lines from wonky bit 3 in both branches.

**Acceptance:**
- Edit any file under `ui/web/src` (e.g. add a comment), run `cargo build`,
  and verify the embedded output changed: `strings target/debug/lanius`
  (or serving `elanus web` and fetching the JS bundle) reflects the new
  dist hash — no manual `npm run build`, no `touch src/web.rs`.
- `cargo build` twice in a row with no changes does NOT re-run the npm build
  (check timestamps / build output).
- With npm hidden from PATH (`PATH=/usr/bin:/bin cargo build` or similar),
  the build succeeds and prints the warning.
- With `LANIUS_SKIP_UI_BUILD=1`, the build succeeds, skips npm entirely,
  and prints the warning.
- `cargo test` passes; `npm run test:ui` (full ui.spec.mjs) passes against a
  binary produced by a bare `cargo build`.

### M2 — retire the ritual in the docs

Update the staleness notes: Cargo.toml:11-19 ordering comment and any
mention of the manual `touch src/web.rs` step in docs/ now describe the
build.rs behavior instead, including the `LANIUS_SKIP_UI_BUILD=1` escape
hatch and the install-failure fallback.

**Acceptance:** `grep -rn "touch src/web.rs" docs/ Cargo.toml` returns only
historical references (handoff logs), no live instructions.

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Decisions fixed in round-2
  planning: committed dist stays, no-npm warns loudly, watch src not dist.

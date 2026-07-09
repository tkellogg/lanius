---
status: done
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

1. **`ui/web/dist` stays GITIGNORED** (decision REVERSED during
   implementation — planning assumed dist was committed; it never was, and
   committing a build artifact would poison every UI diff with churn). The
   fallback artifact is therefore *whatever dist already exists on disk*:
   a source tarball that ships a prebuilt dist (Cargo.toml's `include`
   list covers it), or a dev tree that has built before. A fresh git clone
   effectively requires Node to build the UI — and says so (bit 2c).
2. **No npm ⇒ honest, case-split behavior.** If `npm` is not on PATH (or
   `ui/web/src` does not exist — e.g. a source tarball without the UI
   tree):
   - (b) a dist EXISTS on disk → loud `cargo:warning` that it is being
     embedded as-is and how to refresh it, then succeed;
   - (c) NO dist exists → hard, CLEAR compile error: "the web UI needs
     Node to build from a fresh clone — install Node, or set
     LANIUS_SKIP_UI_BUILD=1 only if you have a prebuilt ui/web/dist."
     Saying so beats a mystery include_dir! failure.
   The runtime Node-free guarantee is untouched — Node is only ever a
   *build-time* convenience.
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
- 2026-07-08 — implemented (Opus worker): build.rs + Cargo.toml (comment +
  `build.rs` added to the include allowlist). All acceptance checks
  observed passing incl. full ui.spec.mjs against a bare cargo-built
  binary; staleness ritual retired.
- 2026-07-08 — VERIFIED (adversarial Opus, fresh context; the planned
  GPT-5.5 verifier channel was unavailable — `lanius code codex --headless`
  workers SIGKILLed in this environment, noted for Fable). Verdict:
  {pass:true, build_ok:true, tests_ok:true} — cargo test 596 pass, e2e ALL
  PASS, freshness/no-op-rebuild/skip-flag/no-npm all confirmed empirically.
  Issues: (1) MEDIUM "committed dist" messaging was false — dist is
  GITIGNORED in this repo; fixed by the planner post-verify: warnings now
  say "existing dist", and every fallback path checks dist/index.html
  exists, panicking with a clear message instead of letting include_dir!
  fail opaquely on a fresh no-Node clone. (2) LOW cosmetic stale-warning
  replay on no-op recompiles — accepted. RESIDUAL for Tim/Fable: decide
  whether to actually commit ui/web/dist (makes no-Node fresh-clone builds
  work; costs diff churn) — the wonky-bit-1 premise assumed it was already
  committed and it is not.
- 2026-07-09 — residual RESOLVED by Fable: dist stays gitignored (build
  artifacts don't get committed). Wonky bits 1-2 rewritten to the real
  semantics; case (c) (no npm + no dist) panic message aligned with the
  agreed copy. All three cases re-verified empirically: (a) npm → vite
  build runs; (b) no npm + dist present → warn + embed; (c) no npm + no
  dist → clear "needs Node" compile error naming the escape hatch.

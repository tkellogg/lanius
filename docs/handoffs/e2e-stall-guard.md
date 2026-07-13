---
status: planned
author: planner (Claude/Opus, chainlink #13)
last-updated: 2026-07-13
---

# e2e stall guard + provider-flow de-flake (chainlink #13)

Make `node ui/web/test/ui.spec.mjs` **incapable of hanging silently** and stop
the two (really: the whole provider flow's) load-sensitive assertion flakes.

## Read these first
- `.claude/skills/debug/stalled-tests.md` — the operator runbook this backs.
- chainlink #13 — the incident log + the planner's causal verdict and the
  live-sample evidence (scratchpad/stall-server-9787.txt).
- `ui/web/test/ui.spec.mjs` — the whole suite is ONE linear top-level-await
  script. Machinery: `waitFor` (line 26, bounded), stack spawn (lines 95-107),
  browser/ctx (110-111), teardown (3973-3977).
- `src/web.rs` — server under test. Shell-out `cli_owned` (2147) / `cli_stdin`
  (2173); the #8 history single-flight `history_liveness_with_probe` (574).

## Causal verdict (why runs stall) — established, not assumed
PRIMARILY A **DRIVER BUG**. The suite has no whole-suite deadline, no
per-assertion deadline, no Playwright default timeout, and uses
`page.evaluate(() => fetch(...))` whose in-page fetch has NO Playwright timeout.
So any single slow-or-lost server response is amplified into an infinite hang.
Teardown (kill server/daemon + pkill actors) sits at the END of the script with
no finally/trap/exit-handler, so a hung OR externally-killed driver leaks the
`lanius web` server and `/tmp/lanius-ui-spec.*` actors — proven by a days-old
orphan from the real 03:07 incident found still alive.

A live `sample` of that orphan showed a healthy-but-idle parked server (no stuck
blocking-pool thread, no in-flight reqwest, port already released) — so a
server-side deadlock is NOT proven for this incident. The server's unbounded
child shell-out (below, M4) is a plausible latent trigger but is defense-in-depth,
not the root cause.

The provider flakes are load-sensitive races across the whole provider flow's
short (5-8s) `waitFor`s: under 3x concurrent load a THIRD assertion
(`configure reload shows the saved named provider`, line 2844) failed, not the
two named in the issue. That is why the failing set shifts.

## Wonky bits to confirm
1. `process.on('exit')` runs synchronous handlers on every catchable exit
   (normal, `process.exit(n)`, uncaught throw) but NOT on SIGKILL. That is the
   right backstop: `server.kill`/`daemon.kill`/`pkill` are synchronous. SIGKILL
   of the driver stays the runbook's job — the driver cannot self-reap through it.
2. Do NOT tighten Playwright's default timeout so far it introduces NEW flakes.
   Use a generous default (20s); the stall watchdog (90s of no progress) is the
   silence-killer, not a tight per-call timeout.
3. `page.evaluate` cannot take a Playwright timeout. The inactivity watchdog is
   the ONLY thing that bounds an in-page `fetch` hang — it is load-bearing.

## Milestones

### M1 — Reap on every catchable exit path (the leak fix)
In ui.spec.mjs, extract teardown into one idempotent **synchronous** `reap()`:
kill `server` and `daemon` (SIGKILL), kill the chromium process
(`browser.process()?.pid` captured after launch) if still up, and
`execFileSync('pkill', ['-9', '-f', TMP])` in a try/catch. Guard with a
`reaped` flag so it runs once.
- Register `process.on('exit', reap)`.
- Register `process.on('SIGINT', () => process.exit(130))` and
  `process.on('SIGTERM', () => process.exit(143))` so external termination routes
  through the exit handler.
- Keep the normal end-of-script path calling `reap()` then `process.exit`.
**Acceptance:** after a run that is interrupted with SIGTERM mid-suite, no
`lanius web` / `lanius-ui-spec` processes remain (verify with `pgrep`).

### M2 — Inactivity watchdog + whole-suite deadline (the loud-fail fix)
- Track `lastProgress = Date.now()` and `lastStep`. Update BOTH inside `ok()`
  and `fail()` (they print every few seconds when healthy), and set `lastStep`
  to `desc` at the START of `waitFor` so a stall inside a wait names the right
  assertion.
- `setInterval` (~5s): if `Date.now() - lastProgress > STALL_MS` (90_000),
  print `FAIL: STALL DETECTED — no progress for 90s (last step: <lastStep>)`,
  call `reap()`, `process.exit(1)`. This is the only thing that bounds a
  `page.evaluate` in-page fetch hang.
- `setTimeout` whole-suite deadline (~12 min; baseline is well under 10):
  print `FAIL: SUITE DEADLINE EXCEEDED (12m; last step: <lastStep>)`, reap,
  exit(1). Backstop for slow-but-progressing pathologies.
- Clear both timers on the normal finish before `process.exit(0)`.
**Acceptance:** with a spawned `lanius web` paused mid-run (`kill -STOP <pid>`),
the suite prints the STALL FAIL naming the in-flight step and exits non-zero
within ~90s — it does NOT hang. No orphans remain afterward (M1).

### M3 — De-flake the provider flow (await the real signal)
- Line 2756: replace the bare synchronous `page.$('#view-setup
  [data-providers-link]')` with
  `await page.waitForSelector('#view-setup [data-providers-link]', { timeout:
  10000 }).catch(() => null)` — await the empty-list render actually completing,
  then branch. Apply the same to the configure-view link (line 2782).
- Add a generous Playwright default: after `ctx` is created (line 111),
  `ctx.setDefaultTimeout(20000)` and `ctx.setDefaultNavigationTimeout(20000)`.
- Raise the provider-flow `waitFor` per-assertion timeouts that flake under load
  to 12000 (lines ~646 cost-summary, ~2835 persists-provider, ~2844
  reload-shows-provider, and the other 8000s in that flow). The de-flake is
  awaiting the real value with headroom; the watchdog guarantees a generous
  timeout still can't hang silently.
- For the cost-summary assertion (646), keep the concrete main-agent checks
  (`text.includes(model)`, `includes(autonomy)`, `model !== haiku`,
  `turns !== '7'`) but raise the timeout to 12000. Do not remove the checks;
  just give the reload render room under load.
**Acceptance:** 3 consecutive full runs are ALL PASS, including under concurrent
load; if a provider assertion still loses a race it fails LOUDLY by name (never
silently), which M2 also guarantees.

### M4 — (SEPARATE, PRODUCT CODE, NEEDS TIM/FABLE'S EXPLICIT GO) server shell-out timeout
`cli_owned` (`.output()`, web.rs:2156) and `cli_stdin` (`.wait_with_output()`,
2193) block a bounded ntex blocking-pool thread on the child `lanius` with NO
timeout. A hung child can wedge a pool thread; enough of them starve the pool and
slow/lose every response (including `/api/status`'s history probe). Bound it:
spawn + `try_wait()` poll loop (or a helper-thread + `recv_timeout`) with a
wall-clock cap (~30s); on expiry kill the child and return an error `CliOut`.
This is defense-in-depth — the driver fix (M1-M3) already removes the silent
hang — so it is OUT OF SCOPE for the test-only worker and must be authorized
separately before any src/ change.

## Log
- 2026-07-13: planner reproduced the flake under 3x load (a third provider
  assertion failed), caught + sampled the days-old 03:06 orphan server (idle,
  no deadlock now), and cleaned all strays. Verdict: driver bug primary; server
  shell-out timeout is optional hardening. Impl scoped to ui/web/test only.

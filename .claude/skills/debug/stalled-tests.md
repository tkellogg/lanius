# Detecting and handling stalled test runs

The e2e suite (`node ui/web/test/ui.spec.mjs`) — and occasionally other
long-running commands — can hang forever mid-run instead of failing. This has
happened at least four times (see chainlink #13). A stalled run wastes hours
silently and can mask a real product bug underneath (the server it's waiting
on may have genuinely deadlocked). This runbook is how to detect a stall
early, kill it cleanly, and decide whether it was noise or signal.

## Detect: is it stalled or just slow?

Baseline: the full ui.spec.mjs suite finishes in **well under 10 minutes** on
this machine. cargo test finishes in ~1–2 minutes. Anything past 2× baseline
with no new output is presumed stalled.

Three checks, cheapest first:

1. **Output-file mtime.** Background runs write to a task output file; the
   suite prints an `ok:`/`FAIL:` line every few seconds while healthy.
   ```sh
   stat -f '%Sm' <output-file>       # macOS; last write time
   tail -3 <output-file>             # what was it doing when it stopped?
   ```
   If the mtime is minutes old on a suite that prints constantly → stalled.
   The last `ok:` line tells you *where* it stalled — record it in #13.

2. **Process liveness vs progress.** A stalled run is alive-but-silent:
   ```sh
   pgrep -fl 'ui.spec|lanius web|lanius-ui-spec'
   ```
   Driver process alive + old output mtime = the wait-for-a-ghost signature.
   (Driver *gone* + servers still listed = a crashed driver that leaked its
   servers; clean those up too.)

3. **Is the server it's waiting on actually answering?** This is the
   noise-vs-signal fork (see below) — do it BEFORE killing anything:
   ```sh
   lsof -nP -iTCP -sTCP:LISTEN | grep lanius   # find the test server ports
   curl -m 3 -s http://127.0.0.1:<port>/api/status | head -c 200
   ```

## Decide: flake or real bug?

- Server **answers** curl normally → the *driver* lost the race (missed
  event, wrong selector wait, SSE subscription raced). Test-side flake;
  note the stall point in #13 and move on.
- Server **hangs or refuses** → possible real deadlock/livelock in
  `lanius web` (mutex held across an await, SSE writer blocked, sqlite lock).
  This is signal. Before killing it, capture a native stack sample:
  ```sh
  sample <server-pid> 3 -file /tmp/lanius-web-stall.txt   # macOS
  ```
  Attach the sample to a chainlink issue. This is the one artifact that can
  root-cause the hang; it is unrecoverable after you kill the process.

## Kill: clean up ALL of it

The suite spawns servers and package-actor processes that outlive a killed
driver:

```sh
kill <driver-pid> <lanius-web-pids>
pkill -f 'lanius-ui-spec'        # /tmp test-root python actors
pgrep -fl 'ui.spec|lanius web|lanius-ui-spec'   # verify: expect nothing
```

Orphaned `lanius web` servers hold ports and skew the next run (and dev's
default port-shifting will silently walk around them, hiding the leak).

## Prevent: bound every run up front

- Foreground: run with the Bash tool's `timeout` parameter (10 min for e2e).
- Any wrapper/script: `timeout`/bounded loop around the suite, kill + retry
  ONCE, then fail loudly. Never wait on a test run without a deadline.
- Workers dispatched to run e2e must carry this rule in their prompt
  (the handoff-workflow skill's ~20-minute worker bound is the backstop).

Gotcha while grepping results: `grep -i FAIL` matches the word "failed"
inside *passing* assertion names (e.g. "ok: … running/failed states…").
Anchor it: `grep -E '^FAIL'`.

## Standing residue

Chainlink #13 tracks the fix-it-for-real work: per-assertion + whole-suite
hard timeouts inside ui.spec.mjs, teardown that always reaps spawned
servers/actors, and de-flaking the provider-flow assertions. If you hit a
stall, add the stall point and the curl/sample evidence to #13.

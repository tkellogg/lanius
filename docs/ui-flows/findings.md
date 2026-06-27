# Web UI — QA findings (run 2026-06-15)

Produced by the `web-qa` harness (Playwright over an isolated live stack;
catalog of record: [configuration.md](configuration.md)). Three suites ran —
`agent-identity`, `kits-review`, `history-config` — each on its own throwaway
root + daemon + web server with `ELANUS_WEB_LOG` capture.

## Summary

Every in-scope flow is **functionally** sound: agents create, configure writes
round-trip correctly (model / max_turns / workdir / skills.include / .exclude /
rename / raw TOML all persist across a reload), kits stage, grants approve, and
the degraded history state is explained rather than blank. The interesting
findings are in the gap between *"it worked"* and *"the user could tell it
worked"* — which is the whole reason this harness drives a real browser and
reads the backend log rather than just asserting HTTP 200s.

Two fixes made earlier this session were **corroborated**; the suites found **two
new bugs** and a set of UX gaps.

## Confirmed bugs (new)

### 1. Configure form silently discards edits made during the load race — *medium*

`loadConfigure()` (`ui/web/src/App.tsx`) leaves the form editable while its two
round trips (profile fetch, then a trailing `await loadDiskAgents()`, ~1–1.3 s)
are in flight, then fills every field from on-disk values at the end. Anything
typed in that window is overwritten with **no lockout, no "loading…", no
dirty-state warning** — silent, invisible edit loss. The raw `#cfg-toml`
escape hatch has the same window.

Reproduced on the first (un-hardened) suite run: the configure POST carried disk
defaults (`model.max_turns=24`, `sandbox.workdir=""`, `agent="kestrel"`) instead
of the typed `7` / `/tmp/kestrel-wd` / `falcon` — proven by the `[web:cli]` line
in the captured log. The suites otherwise pass only because they wait for the
form to settle before typing (`openConfigureStable` + `fillStable`).

**Status: FIXED this session** — the form fields and both save buttons are now
disabled while the pane populates (`setConfigureLoading`), which is both the
guard and the missing "still loading" affordance. A caller-set note (e.g.
na-create's "created …") is preserved across the load.

### 2. Add-on `installed` badge is reachable — *fixed 2026-06-16*

The old web flow ran `elanus kit add <name> --pending`, then approved requests
with `decided_by = "ui"`, so `loadSetup()` never saw the `kit:<name>` provenance
it used for the `installed` badge. Increment 5 removed that split: the web add
button now runs `elanus kit add <name>` as the one human action, so the kit
provenance is present immediately and the badge/`add again` state can render.
Agent-started changes moved to the separate request cards.

## Corroborated fixes (earlier this session)

- **Web server credential / deny-by-default** — server now resolves root as
  `--root > $ELANUS_ROOT > ~/.elanus/root` and presents the fenced owner
  credential; suites connect cleanly (`[web:bus] connected as owner (connack
  reason 0)`), no refusals. Backend logging (`[web:boot|bus|http|cli|pub|sse]`,
  optional `ELANUS_WEB_LOG`) is the instrument that caught bug #1.
- **Stage/approve "flash"** — the durable `#setup-status` banner persists past
  `loadSetup()`'s re-render. The kits suite re-read it ~1.8 s after staging
  (well past the old ~1.4 s flash window) and confirmed text + class survive,
  for both `.status-ok` and the approve case.

## UX gaps (lower priority)

- **No persistent proof a save stuck.** `#cfg-note` "saved — applies on the next
  run" is transient by design; the only durable proof is the reloaded field
  value, so a non-expert gets no lasting visual without manually reloading.
- **Rename has no textual confirmation.** The re-select clears `#cfg-note`; the
  only confirmation is the nav row / `#stage-title` changing — reasonable but
  implicit.
- **Raw-TOML note is thinner** than the form note ("saved" vs "saved — applies
  on the next run"). Inconsistent.
- **Create-agent empty-name feedback is transient** (`#na-note` "name it
  first"). Minor; the rejection is correct and fires no network call.
- **Catalog/seed drift (informational).** The fixture exposes five kits (core,
  dev, funnel, recent-history, window); staging `dev` yields three pending
  packages (git-protect, recent-history, window). The catalog doc names fewer.
  The UI handled all three correctly — the doc is the thing that's stale.

## Recommended regression assertions

Worth promoting into the permanent suite (`ui/web/test/ui.spec.mjs` or a new
`config.spec.mjs`):

1. `#setup-status .status-ok` persists >1.5 s after staging a kit (text + class
   byte-identical when re-read past the old flash window); same after approve.
2. Configure persistence by reload: save model/run steps/workdir/include/exclude,
   `page.reload()`, re-open configure (wait for settle), assert each via
   `toHaveValue` — never trust `#cfg-note`.
3. Empty-workdir clear persists (guards `prunedSet()`'s keep-when-empty contract
   for `sandbox.workdir`).
4. Empty-include coercion: clear `#cfg-include`, save, reload → shows `#` and the
   log shows `skills.include=["#"]`.
5. No-profile guard: a traffic-only agent → configure shows the "only exists as
   traffic" note and an empty `#cfg-toml`.
6. Rename follows the nav: after rename + reload, new name is a nav item, old
   name is gone.
7. Empty-name rejection is local: `#na-create` with blank `#na-name` sets
   `#na-note`, fires no network call, does not navigate.
8. **Kit provenance round-trips (would catch bug #2):** stage + approve a kit via
   the UI, reload setup, assert the row shows the `installed` badge and `stage
   again` — *fails today.*
9. **Load-race guard (catches bug #1):** open configure and immediately type into
   `#cfg-turns` before the form settles; the typed value must survive the load,
   or the field must have been disabled during it — *passes after this session's
   fix.*

# Web UI — SSE reconnect finding (run 2026-06-27)

User report: after laptop sleep/wake, the dashboard did not reconnect to MQTT;
the browser console showed `/api/stream` being interrupted.

Finding: the Rust SSE endpoint only wrote the initial status and live bus
messages. A quiet installation could leave the browser with no bytes on
`/api/stream` for a long stretch, and the shared browser helper relied solely on
native `EventSource` retry. That was too weak for a sleep/wake half-open socket:
the UI could remain visually disconnected even though the backend relay and MQTT
broker were healthy.

Status: fixed with SSE ping frames from `src/web.rs` and an application-level
watchdog/reopen loop in `ui/web/src/live.ts`. Regression:
`ui/web/test/sse-reconnect.mjs` stands up an isolated stack, asserts quiet-stream
ping keepalive, forces the first browser `/api/stream` response to close, then
proves the UI opens a replacement stream and receives a live MQTT event through
it.

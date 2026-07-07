---
name: web-qa
description: >-
  QA the lanius web dashboard (ui/web) by driving the real UI in a headless
  browser against an isolated live stack, asserting DURABLE state and reading
  the backend log. Use when changing ui/web (server.mjs, src/App.tsx,
  src/api.ts, src/styles.css), when a UI gesture "does nothing"/flashes/feels
  unconfirmed, before merging web-UI changes, or to add/run a
  configuration-flow regression. Catches the class of bug HTTP 200s hide:
  silent edit loss, feedback that flashes and vanishes, dead UI affordances.
---

# lanius web-qa

The web UI is a pure MQTT client relayed to the browser over SSE, with admin
gestures that shell out to the `lanius` CLI. The bugs that matter here are the
ones a unit test or an HTTP-200 check miss: a confirmation that flashes for one
frame, an edit silently overwritten by a late async load, a badge that can never
render. So this harness drives a **real headless browser** against a **real
stack** and treats the **backend log** as the forensic instrument.

**Catalog of record:** [`docs/ui-flows/configuration.md`](../../../docs/ui-flows/configuration.md)
— every configuration & verification flow, keyed on real selectors, each paired
with the *observable* a user (and an assertion) can trust.
**Latest findings:** [`docs/ui-flows/findings.md`](../../../docs/ui-flows/findings.md).
**Working reference harness:** `ui/web/test/ui.spec.mjs` (copy its boilerplate).
**Main UI source:** `ui/web/src/App.tsx`.

## The core discipline

1. **Assert DURABLE state, never a transient note.** `#cfg-note`, `#na-note`
   are liveness only. The truth of a config write is the **reloaded field value**
   and the **raw `profile.toml`**. For banners (`#setup-status`), assert
   immediately *and* re-read after ~1.8 s (past the old flash window) to prove it
   persists, not flashes.
2. **Separate "it worked" from "the user could tell it worked."** A flow can be
   functionally `pass` while `user_observes_clearly` is false. That gap is the
   point — record both.
3. **Read `LANIUS_WEB_LOG`.** It proves the actual CLI invocation and POST body
   (e.g. that `model.max_turns` / `sandbox.workdir` / `skills.*` arrived
   correctly). This is how the configure load-race clobber was caught: the log
   showed disk defaults where typed values should have been.

## Recipe — an isolated stack (mirror `ui/web/test/ui.spec.mjs`)

```js
const TMP = fs.mkdtempSync('/tmp/lanius-qa-<suite>.');
const BUS_PORT = 22000 + /* unique per suite */;   // never 1883 (user's live bus)
const WEB_PORT = 9500 + /* unique per suite */;    // never 7180 (user's live web)
const ENV = { ...process.env, LANIUS_ROOT: TMP,
  PATH: `${REPO}/target/debug:${process.env.PATH}`,
  LANIUS_WEB_LOG: `${TMP}/web.log` };              // <-- capture the backend trace

lanius('init');                                    // mints .secrets/owner etc.
fs.writeFileSync(`${TMP}/bus.toml`, `enabled = true\nbind = "127.0.0.1:${BUS_PORT}"\n`);
const daemon = spawn(BIN+'/lanius', ['daemon','--interval-ms','200'], { env: ENV, stdio: 'ignore' });
const server = spawn('node', [`${REPO}/ui/web/server.mjs`, '--root', TMP, '--port', String(WEB_PORT)],
  { env: ENV, stdio: ['ignore','pipe','inherit'] });
// waitFor `${BASE}/` to answer, then chromium.launch({ headless: true }).
```

The web server resolves root as `--root > $LANIUS_ROOT > ~/.lanius/root` and
presents the fenced owner credential, so it authenticates — no deny-by-default
refusal. The boot line `[web:boot] ... credential=present` confirms it; if you
ever see `credential=MISSING`, the root is wrong.

### Playwright module-resolution gotcha

`playwright` installs only under `ui/web/node_modules`, and **ESM ignores
`NODE_PATH`**. A script in `/tmp` that does `import { chromium } from 'playwright'`
will fail to resolve. Run the suite script from inside `ui/web/test/` (drop a
copy alongside `ui.spec.mjs`, run it, delete the copy), or keep the suite as a
file under `ui/web/test/`. Chromium itself is already installed.

### Always tear down (in a `finally`)

`browser.close()`; `server.kill('SIGKILL')`; `daemon.kill('SIGKILL')`;
`execFileSync('pkill', ['-9','-f',TMP])`; then it's safe to `rm -rf $TMP`.

**⚠ Never broaden the cleanup to `pkill -f "lanius daemon"`.** That is global — it
kills the user's live daemon and any other test running at the same time (it
sabotaged a live daemon and an in-flight e2e once). Kill the daemon/server
handles you spawned, and scope any `pkill` to this run's `$TMP`. When you ask a
subagent to run a suite, give it the scoped form too.

## Running / extending suites

- Drive by the selectors + `observable_expectation` in the catalog. Group flows
  into suites (`agent-identity`, `kits-review`, `history-config`, …); give each
  its own ports so they can run in parallel without colliding.
- **Wait for async loads to settle before typing.** `loadConfigure()` populates
  the form only at the end of two round trips; the fields are now *disabled*
  during that window (the fix for the silent-clobber bug), so wait until
  `#cfg-model` is populated and enabled before `fill()`. Don't type into the load
  race.
- **Known-benign console noise to filter:** `fonts.googleapis`/`gstatic`,
  "model list unavailable" / "no API key", `/api/history?kind=agents → 503`
  before the history package is installed, and SSE
  `ERR_INCOMPLETE_CHUNKED_ENCODING` / `ERR_CONNECTION_REFUSED` at teardown
  (server SIGKILLed mid-stream). Everything else is a real finding.

## Backend log tags (`[web:<tag>]`)

`boot` (root/owner/credential/broker/port) · `bus` (connect+connack reason,
reconnect/close/error/disconnect) · `http` (method path → status + ms) · `cli`
(every shell-out + failures) · `pub` (publishes) · `sse` (client connect/
disconnect). Set `LANIUS_WEB_LOG=<file>` to also append to a file; otherwise it's
on stderr.

## When you find something

Write it to `docs/ui-flows/findings.md` (stamped with the run date) and, if it's
worth guarding forever, add the assertion to `ui/web/test/ui.spec.mjs`. See the
"Recommended regression assertions" in the findings doc for the current backlog.

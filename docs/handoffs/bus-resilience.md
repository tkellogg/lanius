---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: soft-degrade when the radio is off, and the stale-prompt replay

Two failure modes from Tim's backlog ([../_questions.md](../_questions.md)),
one handoff, both **root-cause-first** — because grounding partially refutes
both reports as written, and fixing the wrong mechanism would leave Tim's
actual experience unfixed.

**(a)** "If the coding agent can't contact the MQTT broker it dies. This is
not a good failure mode. Something softer??" Grounding says the covered code
paths are **already soft**: every coding-agent bus publish funnels through
`publish_obs` (`src/codeagent.rs:7204`), which logs and **swallows** broker
errors (`:7226` — "obs publish … failed (continuing)"); session mint is pure
local file+sqlite (`codesession::mint`, `src/codesession.rs:1586` — no broker
round-trip); completions are ledger writes, not publishes
(`emit_completion_delivery` → `events::emit`, `codeagent.rs:1112`, caller
swallows at `:3237`); and coding sessions hold an **empty subscribe grant**
(`src/codesession.rs:1165-1166`) so there is no subscribing connection to die.
So either Tim hit a path we haven't found (candidates below), or the death
predates a landed fix, or "can't contact the broker" was really "daemon down"
manifesting elsewhere (his memory notes record exactly one such prior
incident). M1 reproduces before anything is built.

**(b)** "When I start a new codex or claude code session through elanus, it
starts running one of my prompts from some previous session… Maybe it's
related to QoS 1??" Grounding says QoS1/retained redelivery is **not the
mechanism**: deliveries are driven off the **ledger**, not an MQTT
subscription (`drive_code_deliveries` scans `events WHERE state='pending' AND
type LIKE 'in/agent/%'`, `src/dispatcher.rs:1284-1287`); every delivery is
scoped by the session id embedded in the topic (`recognize_delivery`,
`src/codeagent.rs:485-508` — requires `in/agent/<noun>/<conv>` with an
existing matching `code_sessions` record); `in/agent/*` publishes are
non-retained (`dispatcher.rs:329` `retain=false`); the broker has **no
persistent-session/offline-QoS1 queue** (fresh `SessionRec` per connect,
`src/broker.rs:499-513`; retained replay exists only on SUBSCRIBE
`broker.rs:715-731`, and coding sessions never subscribe); and duplicate
drives are blocked by per-session idempotency keys (`dispatcher.rs:1328-1346`,
`codesession.rs:395/:412`). But the ledger scan has **no time bound** — an
old `pending` row fires whenever its target resolves — and daemon boot
**re-pends every `running` event** (`dispatcher.rs:189-192`). The plausible
real mechanisms are ledger-side, against a **reused durable session id**, not
MQTT. M1 pins it down.

## Wonky bits / decisions to confirm

1. **Reproduce before fixing, both halves.** For (a) the honest candidate
   list to probe with the broker/daemon actually down: the interactive-launch
   full path (TUI up, then daemon killed mid-session — do hooks misbehave? the
   hook binary itself is defensive: no env → quiet `Ok(())`,
   `codeagent.rs:6710-6714`, and all its publishes are `publish_obs`); `elanus
   code deliver`/`spawn` gestures from *inside* a session (these use
   `events::emit` — but sqlite, not broker); the **codex hook bridge**
   (commit e6aff7f, the newest connect path — check its connect/retry
   behavior); opencode's served-events subscriber; and simple version skew
   (Tim's report may predate the `publish_obs` softening). For (b): write a
   reproduction score against each ledger-side hypothesis below. *Fable:
   confirm root-cause-first even though it may conclude "(a) is already
   fixed; ship only the warning/visibility milestone".*

2. **The stale-prompt hypotheses to test, ranked.** (i) **Boot re-pend to a
   reused durable id**: daemon restarts re-pend `running` `in/agent/*` rows
   (`dispatcher.rs:189-192`); the idempotency guard blocks a *second* drive of
   the same event id per session (`delivery_key_seen`), but a row that was
   `running` and never key-claimed — or a delivery keyed against the durable
   `code_sessions` record that a *new TUI resume of the same native session*
   maps back onto (`upsert_record`, `codesession.rs:90`; thread-folding per
   [session-thread-grouping.md](session-thread-grouping.md)) — would fire into
   what Tim experiences as "a fresh session". (ii) **Old pending mail, no
   time bound**: mail addressed days ago to a session id that comes back alive
   (a `--resume`) is driven immediately — correct by the letter of the mailbox
   contract, surprising in a TUI. (iii) **The native tool's own resume**:
   `resume_command_for` (`codeagent.rs:6418`) continues the tool's native
   conversation — if a launch path ever resumes when the user meant "new",
   the *tool* replays its last prompt with elanus uninvolved. Each hypothesis
   has a distinct observable signature in the ledger (event ids, session ids,
   `delivery/duplicate` obs) — the reproduction must capture which one fires.
   *Fable: confirm testing all three rather than presuming (i).*

3. **The fix for (b) is written as acceptance criteria on delivery scoping,
   not a mechanism.** Whatever M1 finds, the contract to enforce: **a new
   session receives only mail addressed to it** — where "addressed to it"
   means the exact session id in the topic (`recognize_delivery` already
   enforces this; the bug, if real, is in which session *id* a relaunch/boot
   maps onto), and a delivery older than a sanity horizon (or predating the
   session's creation) is **held for confirmation, not silently run**. The
   likely shape is small: stamp deliveries with the target session's
   `created_at` comparison, or make the boot re-pend of `in/agent/*` rows
   settle-with-failure-mail rather than re-drive when the target session is
   gone — but the mechanism choice belongs to the implementer *after* M1.
   *Fable: confirm criteria-not-mechanism for M3.*

4. **"Broker down" must stay distinguishable from "broker refuses this
   credential" — today it isn't.** Client-side, both surface as an untyped
   `io::Error`: TCP failure at `bus.rs:296-306` (`connect_timeout` `:297`),
   CONNACK refusal at `read_connack` `bus.rs:370-396` (`:393-394` — a refused
   CONNACK and a wire error are the same error type). The broker *does* know
   the difference (`handshake`, `src/broker.rs:428-480` — `NotAuthorized` for
   bad session token `:444-445`, bad fenced secret `:456-457`, unknown
   identity `:468-469`, no credential `:476-478`). Soft-degrade must key on
   this: **unreachable ⇒ degrade quietly + retry; NotAuthorized ⇒ loud,
   every time, never "opportunistically reconnect" past it** (an auth failure
   is a fact about the credential, and silently retrying it is how the
   credential errors Tim previously hit stayed confusing). Type the error at
   `read_connack`/`buscli` so callers can tell. *Fable: confirm
   loud-on-denied, soft-on-down as the doctrine line.*

5. **Once-per-session warning, not once-per-publish.** `publish_obs` today
   eprintlns on *every* failed publish (`codeagent.rs:7226`) — with the
   broker down that's a stderr drip into a TUI the whole session. The soft
   contract: first failure prints one loud, plain warning ("elanus can't
   reach its message bus — this session continues UNCAPTURED (no record, no
   sibling awareness, no mail) until it reconnects"), subsequent failures are
   silent, a successful publish (opportunistic — just the next attempt
   succeeding, no reconnect daemon needed since `buscli::publish` dials fresh
   each time) prints one "reconnected, capture resumed" line. Per-process
   state (an `AtomicBool`/Once), no config. The warning must also land as a
   `session/start` briefing note when the *launch-time* publish already
   failed, so the agent itself knows its trace is dark (journey-14: tell the
   agent). *Fable: confirm the agent-visible half — an uncaptured session
   that doesn't know it's uncaptured will confidently claim "I filed my
   mail".*

## Milestones

### M1 — Reproduce + root-cause both reports (no fix code)
Scripted probes on a scratch root (`/private/tmp` scratch only, per
containment doctrine; kill everything started):
- **(a)** matrix: {claude, codex, opencode} × {launch with daemon/broker down,
  daemon killed mid-session} × {TUI, headless}, plus in-session gestures
  (`elanus code deliver`, `spawn`, hook fire) with the broker down. Record per
  cell: does the session live? what's on stderr? what's captured after
  recovery? Explicitly test the codex hook bridge connect path.
- **(b)** the three hypotheses (wonky bit 2): seed pending/running
  `in/agent/*` rows, restart the daemon, relaunch/resume each harness fresh
  and resumed; capture which ledger rows fire into which session ids.

**Acceptance:** a written root-cause note in this handoff's Log for each
report: the failing path with file:line (or "not reproducible on this tree,
behavior already soft — closing (a) with M2's visibility work only"), and for
(b) which hypothesis (or which new mechanism) produces a prior prompt in a
fresh-feeling session, with the reproducing script checked into the test tree.

### M2 — Soft-degrade contract + loud/quiet split (a)
- Type the connection errors (wonky bit 4): `read_connack` (`bus.rs:370`)
  and `buscli::publish` distinguish `Unreachable` vs `Denied(reason)`;
  `NotAuthorized` stays loud on every occurrence.
- Once-per-session warning + reconnected notice in `publish_obs`
  (`codeagent.rs:7204-7226`, wonky bit 5), and the agent-visible uncaptured
  notice on the launch path when the `session/start` publish fails
  (`codeagent.rs:3136` envelope site).
- Fix whatever hard-death path M1 actually found (if any) to the same
  contract: launch proceeds, turns proceed, capture/injection degrade,
  auth failures bail loudly with the broker's reason.

**Acceptance:** with the broker down: `elanus code <tool>` (TUI and headless)
launches, runs a full turn, and exits by the tool's own status; stderr carries
exactly one warning and (after the broker returns mid-session) one reconnect
line; the session's post-reconnect events are captured. With a **bad
credential**: the failure message names authorization (not "is the daemon
running?"), and repeats. A unit test covers the error typing; the e2e matrix
from M1 re-runs green under the new contract.

### M3 — Delivery scoping acceptance (b)
Implement the smallest fix M1's root cause demands, judged against the
contract (wonky bit 3): a freshly launched session receives no mail addressed
to any other session id; a *resumed* session receives only mail addressed to
its id, and mail predating a legible horizon is surfaced-not-run (or settled
to failure-mail when its target session is gone at boot re-pend). The
delivery paths in play: `drive_code_deliveries` (`dispatcher.rs:1272`,
scan `:1284-1287`), `recognize_delivery` (`codeagent.rs:485`), the boot
re-pend (`dispatcher.rs:184-192`), `settle_code_deliveries`
(`dispatcher.rs:962`), the idempotency guard (`:1328-1346`).

**Acceptance:** a regression test reproducing M1's (b) mechanism fails on the
old code and passes on the new; the normal delivery round-trip
(spawn → worker → failure-mail/completion) is unchanged (existing tests
green); a fresh TUI launched after a daemon restart with stale pending rows
in the ledger runs **no** prompt the user didn't just type (e2e assertion via
the captured obs trace). `cargo test` green.

## Read these first
- The soft-by-construction publish path: `src/codeagent.rs:7204-7226`
  (`publish_obs` — the single choke point and the swallow), `src/buscli.rs`
  (`publish` — the only place broker errors become `Err`: "no broker response
  within 5s (daemon running?)" / "connection failed" / PUBACK `bail!`).
- The launch path that doesn't need the broker: `codeagent.rs:3082`
  (mint call), `src/codesession.rs:1586` (`mint` is local), `:1165-1166`
  (empty subscribe grant), `codeagent.rs:3136`/`:3204` (start/stop envelopes
  via `publish_obs`), `:3243-3246` (the only `process::exit`, keyed on the
  tool). The hook binary's defensiveness: `codeagent.rs:6705-6714`.
- Down-vs-denied: `src/bus.rs:296-306` (`connect`), `:341-345` (clean-start
  CONNECT), `:370-396` (`read_connack` — the untyped refusal);
  `src/broker.rs:428-480` (`handshake` — the four `NotAuthorized` arms).
- The delivery machinery for (b): `src/dispatcher.rs:1272` (`drive_code_
  deliveries`, scan `:1284-1287` — no time bound), `:962` (`settle_code_
  deliveries`), `:184-192` (boot re-pend), `:1193` (`reconcile_lost_routes`),
  `:1328-1346` (idempotency); `src/codeagent.rs:485-508` (`recognize_
  delivery`), `:667-675` (default key), `:888` (`launch_session_id`);
  `src/codesession.rs:90` (`upsert_record`), `:395`/`:412` (delivery keys),
  `:474-503` (`inbox_for_session` — a ledger query, not a subscribe).
- Why QoS1/retained is exonerated: `src/broker.rs:499-513` (no persistent
  sessions), `:715-731` (retained replay only on SUBSCRIBE), `:648-650` +
  `:1260-1319` (QoS1 is live-subscriber machinery); `dispatcher.rs:329`
  (`retain=false` on ledger announcements). DISPATCH_HINT is a static help
  string (`codeagent.rs:103`, read only at `:6904`) — not a replay vector.
- Doctrine: [../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md)
  (tell the agent — the uncaptured notice); the failure-mail contract in
  [cross-harness-death.md](cross-harness-death.md); prior incident context in
  Tim's memory ("credential errors were operational — daemon down").

## Log
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull. Grounding
  materially reshaped both halves: (a) the coding agent does NOT hard-die on
  broker-down through any covered path — `publish_obs` swallows, mint is
  local, completions are ledger writes, sessions never subscribe — so M1 must
  reproduce before M2 fixes; the one real hole found is that down-vs-denied
  is indistinguishable client-side (`read_connack` returns an untyped error).
  (b) QoS1/retained redelivery is exonerated by construction (ledger-driven
  delivery, session-scoped topics, no persistent broker sessions,
  non-retained announcements); the live hypotheses are ledger-side — the
  unbounded pending scan, the boot re-pend, and native-tool resume — all
  against a reused durable session id. Judgment calls for Fable:
  root-cause-first even if it closes (a) as already-soft (1); test all three
  (b) hypotheses (2); M3 written as scoping criteria, mechanism chosen
  post-root-cause (3); loud-on-denied vs soft-on-down (4); once-per-session
  warning + the agent-visible "you are uncaptured" notice (5).

- 2026-07-02 — **M1 root-cause (both reports).**
  - **(a) NOT reproducible — already soft; closed with M2's visibility work
    only.** Scripted probe `tests/bus_resilience_repro.sh` (scratch `ELANUS_ROOT`,
    no daemon = broker down) launches `elanus code echo --headless`: the session
    **survives** (exit 0, by the tool's own status), its work is **captured to
    disk** (`code_claims` row present — capture is local sqlite, not the bus), and
    the only defect the report was actually feeling surfaces as a **stderr drip** —
    one `[code] obs publish … failed (continuing)` line *per publish* (five for a
    one-turn echo). No hard-death path exists on this tree through any covered
    route (`publish_obs` swallows at `codeagent.rs:7204`; `mint` is local; the
    `session/start`/`stop` envelopes and all capture publishes are `publish_obs`;
    the external-adapter child publishes through the same swallow). The genuine
    hole confirmed: **down-vs-denied is indistinguishable** — a pure
    connection-refused prints the same `connection failed (daemon running?)` a
    refused CONNACK would (`buscli::publish` mapped every `ConnectionError`
    through one anyhow `.context`; `bus::read_connack` returned an untyped
    "CONNACK refused"). So (a) reduces to a *visibility* problem, exactly as
    grounding predicted → fixed in M2, no hard-death fix needed.
  - **(b) mechanism = the unbounded ledger scan driving stale mail into a reused
    durable id (hypothesis ii, with (i) boot-re-pend as the same class).** Pinned
    down deterministically in `src/dispatcher.rs`
    `drive_holds_stale_delivery_and_drives_fresh`: an `in/agent/<noun>/<conv>`
    delivery sits `pending` with **no time bound** (`drive_code_deliveries` scan,
    `dispatcher.rs:1284-1287`), so the instant its target `code-*` record resolves
    again — a `--resume`, or the boot `running→pending` re-pend
    (`dispatcher.rs:189-192`) of a reused id — `recognize_delivery` matches it and
    the daemon drives DAYS-old mail into what the human experiences as a fresh
    session. QoS1/retained stays exonerated (the test needs no broker at all —
    a ledger seed reproduces it). Hypothesis (iii), the native tool's *own*
    `--resume` replaying its last prompt, is out of elanus's ledger and not
    reproduced here; it is a tool-launch-flag concern, noted not fixed.

- 2026-07-02 — **M2 shipped (soft-degrade contract + loud/quiet split).**
  Typed the publish error (`buscli::BusError` = `Unreachable | Denied | Other`;
  `publish_typed` classifies a rumqttc `ConnectionRefused` CONNACK as `Denied`,
  everything else as `Unreachable`; `publish` unchanged for existing callers).
  `read_connack` now makes an auth refusal legible even on the swallowed mirror
  path. `publish_obs` collapses the drip via a pure, unit-tested state machine
  (`degrade_decision` over two per-process atomics): **one** uncaptured warning
  per degraded stretch, **one** reconnect line on the next successful publish,
  and `Denied` is **loud every time** and named as authorization (never
  "daemon running?"). The launch path tells the agent itself: when the
  launch-time `session/start` publish fails, a briefing note warns the session it
  is UNCAPTURED (do not claim mail was filed), and the child is seeded
  `ELANUS_BUS_DEGRADED=1` so the launcher+adapter (and per-hook) processes emit
  ONE warning, not one each. Verified: broker-down launch = exit 0, one warning,
  zero drip, work on disk; broker-up = zero warnings. Tests:
  `buscli::…refused_connack_is_denied…`,
  `codeagent::…degrade_decision…`, `bus::…read_connack_distinguishes…`, and the
  broker-down e2e `tests/external_harness.rs` re-runs green.

- 2026-07-02 — **M3 shipped (delivery scoping).** `drive_code_deliveries` now
  holds a stale delivery instead of driving it: a pending `in/agent/*` row older
  than a 24h horizon, **or** addressed before its target session record existed
  (a reused-id signature), is settled `state='held'` with an
  `obs/agent/code/delivery/held` trace — surfaced in the session's inbox for
  confirmation, never silently run. The horizon is generous enough that a normal
  spawn→worker→completion round-trip and a same-day idle resume are unaffected
  (existing delivery/idempotency tests stay green). Regression tests
  `drive_holds_stale_delivery_and_drives_fresh` (fails on old code) and
  `drive_holds_delivery_predating_session_creation`. **Deferred:** the boot
  re-pend "settle-to-failure-mail when the target session is *gone*" variant was
  not needed — a delivery to a vanished record already never drives
  (`recognize_delivery` returns `None`, leaving it for `dispatch_pending`); the
  hole was stale mail to a *surviving* record, which the hold closes. Report
  (b)'s hypothesis (iii) (the native tool's own `--resume`) is also deferred —
  it lives in launch-flag territory, not the ledger.

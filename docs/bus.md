# elanus v2 — The Bus Architecture

> Status: design, agreed 2026-06-10. Supersedes the *architecture* of
> [init.md](init.md); init.md remains the accurate record of v1, which is
> implemented and live-tested on `main`. Nothing in this doc is built yet.
> Companion: [sandbox.md](sandbox.md) — the cage/lease authority model, fs
> events, packages-as-actors. Same status, agreed same day.
> Same conventions: **[DECIDED]** is settled with rationale, **[OPEN]** needs a
> decision. Spec citations are to MQTT 5.0 (OASIS Standard, 2019-03-07),
> verified June 2026.

## Thesis

**[DECIDED]** Everything is an event: one envelope, one topic grammar, one bus.
The bus is **in-process** and wears **MQTT 5 as its boundary interface** —
external processes (skills, UIs, phones, bridges) speak standard MQTT to a
listener; internal components (ledger, recorder, hook engine, exec) consume the
same topic stream as plain Rust, no framing, no loopback hop.

v1 tagline: `inetd + cron + git hooks + sqlite`.
v2 tagline: `mosquitto + systemd + git hooks + sqlite — in one process`.

Principles, each load-bearing:

1. **Interface unification, not storage/scheduler unification.** "Everything is
   a file" won because the interface was small and the contracts varied
   (`O_NONBLOCK`); /proc was never on disk. Likewise: every happening is
   expressible as `{topic, ts, cause, correlation, payload}`; *planes* differ
   in delivery contract, never in shape.
2. **The black box doesn't depend on the radio.** Flight recorder, work ledger,
   and hook chains are in-process bus consumers. If the MQTT listener dies,
   you lose dashboards and external ingress — never recording, never work,
   never policy enforcement. (Threat model: otherwise killing the broker is
   the hook bypass.)
3. **Direction is convention.** Topic names carry direction (`human/ask`);
   the kernel never does. Unchanged from v1.
4. **The agent runs as late as possible.** See the variety ladder.

## Spec ground truth (what MQTT 5 actually gives us)

Verified against the OASIS spec, June 2026. These verdicts shaped the design;
section numbers cited so future-us can re-check.

- **MQTT 5.0 is still the latest spec.** No 5.1/6 exists or is drafted; the
  OASIS TC charter mentions refinements but has published nothing post-5.0.
- **SUBSCRIBE packets carry metadata** (§3.8.2.1): Subscription Identifier
  (0x0B) and **User Properties (0x26)** — arbitrary UTF-8 key-value pairs,
  repeatable. Spec text: *"The meaning of these properties is not defined by
  this specification."* It is a sanctioned client→broker extension point:
  standard brokers ignore them; a broker interpreting them is conformant.
  **This is the mechanism for blocking-subscription declarations.**
- **Deferred (manual) acks are legal**: PUBACK signals "accepted ownership"
  [MQTT-4.3.2-4] with no deadline — ack-after-processing is fine. Constraints:
  PUBACKs must be sent in the order PUBLISHes arrived [MQTT-4.6.0-2];
  Receive Maximum (§4.9) is the de facto prefetch window.
- **Redelivery happens ONLY on session re-establishment** [MQTT-4.4.0-1] —
  a MUST NOT at any other time. No live-connection retry exists in 5.0.
- **A negative PUBACK is a terminal drop, not a requeue** [MQTT-4.4.0-2]; on
  shared subscriptions the broker **MUST NOT** redispatch to another member
  [MQTT-4.8.2-6]. **There is no visibility timeout, no nack-requeue, no DLQ
  anywhere in the spec.** The SQS-style processing-ack never arrived.
- **Shared subscriptions** (`$share/group/filter`, §4.8.2): consumer choice is
  broker-discretion; **no ordering across the group**; unacked QoS 1 messages
  redistribute only when the dead consumer's *session terminates* (SHOULD/MAY)
  — so worker sessions must use short session-expiry or messages stick to
  corpses.
- **Wills**: fire on abnormal close (or explicit DISCONNECT 0x04 "with will");
  Will Delay Interval smooths reconnect flapping; retained wills supported;
  will-delay > session-expiry turns a will into a "session expired" event.
- **Request/Response** (Response Topic + Correlation Data, §4.10): broker is
  forward-only; the pattern is non-normative convention. Fine for us — the
  hook engine is the counterparty, not the broker.
- **No standard way to enumerate subscriptions** ($SYS is a non-normative
  mention; structure is broker-specific). This *strengthens* the in-process
  case: we own the subscription table by construction.

Consequence threaded through everything below: **completion is kernel-owned.**
The protocol cannot tell a publisher "all deliveries complete"; our bus can,
because it sees its own subscription table and its own acks.

## Topic grammar

> **[SUPERSEDED IN DESIGN — 2026-06-11]** A v3 grammar is agreed in
> [topics.md](topics.md): verb-first `{verb}/{category}/{noun}/{locators}`,
> verbs = delivery contracts (`in`/`obs`/`signal`; `out` deliberately does
> not exist — mailbox model), agent identity first-class, conversation IDs as
> topic locators, rooms as nouns. **Migration landed 2026-06-11** — topics.md
> is now the as-built grammar; the table below is the historical v2 record.

**[DECIDED]** One namespace, MQTT filter syntax (`+`, `#`) as the *single*
pattern language — handler subscriptions, throttles, recorder rules, grants,
hook points all use it. Replaces v1's dotted names + globset.

```
work/...                          durable work items (ledger-backed)
  work/agent/exec                 was agent.exec
  work/discord/message            was discord.message
obs/exec/<session>/llm/{request,response}
obs/exec/<session>/tool/<name>/{call,result}
obs/exec/<session>/reasoning/{pre,post}
obs/ui/<device>/...               keydown and friends
obs/skill/<name>/status           retained liveness (LWT writes "dead")
obs/hook/<point>/<outcome>        every hook invocation echoed
obs/ledger/{emit,expire}          ledger bookkeeping echoes
obs/dispatch/{spawn,exit}         handler dispatch lifecycle
signal/{pain,anomaly,...}         algedonic; never coalesced, never queued behind
human/{ask,answer}
delivery/<channel>/{sent,acked}   receipt events; escalation reads these
ingress/<source>/...              external arrivals, pre-triage
fs/<path>                         file-change events from caged subprocesses;
                                  topic = "fs" + canonical absolute path with
                                  the leading "/" dropped — see sandbox.md
```

Envelope unchanged from v1: `{ts, topic, cause, correlation, payload}`.
Dots→slashes migration is mechanical. **[DECIDED]** Path-derived topic
segments percent-encode exactly `+`, `#`, `%` (as `%2B`, `%23`, `%25`):
wildcards are legal in filenames but illegal in topic *names*
[MQTT-4.7.1-1]. Filters are authored against the encoded form; nothing ever
decodes for matching. **[DECIDED]** `handlers.d/` retires — registration
moves to package manifests (see Packages below); `ls packages/` replaces
`ls handlers.d/` as the debugging surface.

## The bus

**[DECIDED]** In-process bus; MQTT 5 listener as the boundary; privileged
in-process consumers for ledger/recorder/hooks/exec.

**[DECIDED — spike passed 2026-06-10, see spike/ntex/REPORT.md]** Build the
micro-broker on **ntex-mqtt 8.x**. Research verdict (June 2026): it is the
only option meeting every requirement *natively* — and the spike confirmed
each claim live:

- SUBSCRIBE user properties + subscription identifiers are surfaced directly
  to application code (`v5::codec::Subscribe { user_properties, .. }`), with
  per-filter ack/deny — blocking-subscription declarations and per-client ACL
  are ordinary application logic.
- Per-delivery completion is built in: `send_at_least_once()` returns a future
  resolving on the subscriber's PUBACK. Deferring the *publisher's* PUBACK
  until the fan-out's futures resolve makes **"all deliveries complete" a
  control-flow fact, not a synthesized approximation.**
- We own the subscription table, retained store, and shared groups by
  construction.

Costs, eyes open: ~1–2k LOC of broker logic (topic trie, retained store,
shared groups, will-delay timers, session expiry), the ntex `System`-in-a-
thread embedding dance, and single-maintainer framework risk.

Spike results (2026-06-10, spike/ntex/): **both fallback triggers clear.**
Embedding works — ntex on a dedicated `std::thread` creates its own
current_thread runtime (`Handle::try_current()` → Err path in ntex-net),
coexisting with the main tokio multi-thread runtime, 10/10 clean runs; the
contract is *launch the System only from `std::thread::spawn`, never inside a
tokio task*. `Subscribe::user_properties` + per-filter `confirm`/`fail(0x87)`
confirmed; `send_at_least_once()` futures resolve on subscriber PUBACK
(fan-out join = "all deliveries complete"). Owned broker logic ≈ **1k LOC**
vs the 2k trigger. Loopback publish ~11 µs release, round trip < 10 ms —
well inside the 500 ms hook budget. Residual: `!Send` internals
(`Rc<MqttShared>`) pin broker logic to the ntex thread — cross-thread via
channels, as designed; keep owned logic thin so rmqtt migration stays
mechanical.

**[DECIDED — fallback, not triggered]** If ntex sours later (maintainer risk):
embed **rmqtt 0.21+** (very active, documented library mode, tokio-native). It has the right
hook points (`ClientSubscribeCheckAcl`, `MessagePublish`, `MessageAcked`/
`MessageDropped`, router introspection for fan-out sets) — but **drops
SUBSCRIBE user properties before they reach hooks** (small localized patch, or
fall back to CONNECT-level user properties). Rejected: rumqttd (no ACL, no
hooks, discards user properties, slow maintenance); NanoMQ via FFI (no
in-process API, no Rust bindings); `mqtt5` crate (right shape, ~48 stars —
watch, don't bet).

Rust-side external skills use **rumqttc** (`subscribe_with_properties`,
`set_manual_acks(true)` + `client.ack()`). Avoid paho-mqtt-rust: no manual
ack, which breaks crash-only consumption.

**[DECIDED 2026-06-10] Listener boundary**: TCP, loopback by default
(`bind = "127.0.0.1:1883"` in root `bus.toml`), configurable. Binding beyond
loopback is *possible but discouraged* until grants land — there is no
authentication yet, and the daemon warns loudly. Unix sockets rejected:
non-standard MQTT, and the client ecosystem (rumqttc included) doesn't
reliably speak them. Local processes (exec, emit, handlers) mirror their
happenings to the listener with a hand-rolled runtime-free QoS 0 publisher
(`el-mirror` user property = "origin already recorded this, forward
verbatim") — a client library on the flight path would drag an async runtime
into trace::write. Identity/auth for non-local clients stays open (question
7).

**[LANDED 2026-06-11]** Two UIs exist, both pure-client proofs, zero
privileged access: `ui/tui/` (ink + mqtt.js, an ordinary anonymous loopback
MQTT 5 client) and `ui/web/` (the preferred surface — a node server is the
MQTT client, browsers ride SSE one hop behind; conversation view + telemetry
rail + signal lamp). Both subscribe `obs/# in/# signal/#`, answer asks and
compose agent work by publishing to the agent mailbox (correlation via the
`el-correlation` user property, see topics.md). Each has its own smoke test
driving a real daemon end to end (`npm test` in its directory; deliberately
not part of tests/e2e.sh — the repo gate stays node-free).

**Degradation order** (the test for every design choice): MQTT listener down →
external fan-out and ingress lost; work, hooks, recorder, exec unaffected.

## Planes are delivery contracts, not buses

| plane | contract | slow consumer means | volume |
|---|---|---|---|
| observation | QoS 0, fire-and-forget | drops data | unbounded |
| work | ledger-backed, at-least-once, completion tracked | work is late | bounded |
| hook | blocking chain, timeout + declared default | **system stalls** → timeout fires | tiny |

Precedents for the split: LSM hooks vs tracepoints, netfilter vs pcap, filter
drivers vs ETW, Claude Code PreToolUse vs its transcript. Interception and
observation are never the same mechanism in mature systems because their
failure semantics differ.

### Observation plane

Everything echoes here: tool calls/results, LLM request/response, reasoning
pre/post, dispatch/exit, keydown, state transitions. QoS 0. Persistence is
opt-in via recorder rules. This is where "massive numbers of events" live
without anything else paying for them.

### Work plane

**[DECIDED]** The sqlite ledger remains the source of truth — at-least-once,
suspend/resume, causality audit, exactly as v1. `work/#` topics are transport
and announcement; the ledger is a privileged bus consumer that materializes
state transitions. The spec findings above are why the broker can't own this:
NACK = drop, redelivery only on reconnect, no completion fan-in, no DLQ.
"All deliveries complete" events are published by the kernel (from deferred-ack
control flow on ntex, or dispatch bookkeeping) as ordinary observations.

**[GAP CLOSED — work-plane-on-bus, 2026-06-11].** Historically (steps 5/6),
kernel-*originated* work events (created by `elanus emit`, cron, or the
dispatcher → `events::emit`) were written to the ledger and dispatched to
**exec-mode** handlers, but never announced on the bus under their own topic
— a daemon actor SUBSCRIBEd to `in/package/discord/send` only ever saw
bus-origin publishes. As built now:

- **Announcement is exactly-once, decided at the row.** `events::emit`
  inserts `announced=0`; the daemon's per-tick sweep
  (`dispatcher::announce_ledger_events`) publishes every unannounced `in/#`
  and `signal/#` event under its own topic via the in-process channel and
  marks the row. The broker's inbound path inserts `announced=1` because it
  fans the materialized event out itself — a bus-origin event is never
  announced twice. The sweep also covers events emitted while the daemon was
  down. The kernel deliberately does NOT ride the `el-mirror` loopback for
  this: the broker re-ledgers `in/signal` mirrors by design (the mirror
  marker is never a license to inject un-ledgered work), so the in-process
  channel is the only correct route.
- **Completion fan-in is control flow.** For an `in/#` delivery the broker
  counts QoS 0 sends complete immediately and joins the QoS 1 PUBACK futures;
  when the last lands it publishes `obs/harness/delivery/complete`
  `{topic, event_id, subscribers}` (zero subscribers → no event). Bookkeeping
  only: the publisher's PUBACK stays "the ledger accepted it."
- **`$share/<group>/<filter>`** (§4.8.2) is supported: round-robin one-of-N
  per group, no retained replay to shared subscriptions, ACL accepts a grant
  on either the full `$share` form or the inner filter (the group is delivery
  mechanics, not extra authority).

Degradation order holds: listener down → daemon actors miss live
announcements; the ledger row, exec dispatch, and recording are untouched.

### Hook plane

**[DECIDED]** Blocking interception at fixed points: `pre`/`post` tool call,
`pre` LLM request (context/policy injection), `post` LLM response (scrubbing),
`pre` dispatch. Two registration styles, one semantics:

- **Resident hooks**: a grant-approved local client SUBSCRIBEs with User
  Properties — `mode=blocking, phase=pre, order=10, timeout_ms=500,
  on_timeout=allow|deny`. Spec-legal (§3.8.2.1.3); the bus interprets them.
  Round trip via Response Topic + Correlation Data. Standard external clients
  subscribing to the same topics get observation semantics — blocking is a
  *granted capability*, never ambient.
- **Exec hooks**: `[[hook]]` in the manifest, git-hooks style — fork/exec with
  the event on stdin; exit 0 = allow, nonzero = deny, stdout = rewritten
  event. For cheap stateless policy.

`on_timeout` is declared per registration because fail-open vs fail-closed is
a security decision: a dead policy hook must not silently approve tool calls.
Every hook invocation and outcome echoes to `obs/hook/#`. The pre-tool-call
chain is where sandbox capability policy enforces when it lands — hooks are
the enforcement point init.md promised ("injection checks before dispatch").

**[DECIDED 2026-06-11]** Resident-hook transport: **MQTT request/response
round trip** (Response Topic + Correlation Data, §4.10), the canonical 5.0
pattern — stable per-client response topic subscribed once at connect,
per-request Correlation Data to demux (UUID-in-topic costs a subscription per
request; opaque echoed bytes are free). The verdict is an ordinary publish
whose payload carries `{decision: allow|deny, event: <rewritten>}`; PUBACKs
are QoS plumbing and never carry the verdict. §4.10 is non-normative — the
broker forwards the property without understanding it and nothing obliges a
responder — so `timeout_ms` + `on_timeout` stay load-bearing, not paranoia.
Explicitly rejected: an HTTP sidecar API (second listener, second auth story,
second framing for a round trip MQTT already does) and direct-SQLite verdict
channels (that's leg 3 of the containment gap — it would consecrate the
hole). Spike numbers: ~11 µs loopback publish, < 10 ms round trip, vs the
500 ms budget.

**[LANDED 2026-06-11]** Resident hooks as built (src/broker.rs coordinator,
src/resident.rs requester, `elanus bus sub --blocking` client). Refinements
where implementation sharpened the design: (a) hook *requests* are a QoS 1
publish to `obs/harness/hookreq/<point>/<matched>` — under `obs/`, not a
reserved `in/` prefix, because in/# materializes to the ledger by the v3
routing rule and hook round trips must never be ledger-backed (topics.md
decided 7); a special-cased in/ prefix would break "delivery contract
decidable at segment 1". (b) The subscription *filter* is authoritative for
what a registration intercepts; the blocking grant vocabulary stays the
exec-hook one (grant value = literal point name), so one manifest
`blocking = ["pre_tool_call"]` line covers both registration styles, and the
grant is re-checked per invocation (revocation detaches live, not at
reconnect). (c) Chain order: exec hooks first (local, stateless, no round
trip), then resident hooks on the exec-rewritten subject; first deny
short-circuits. (d) Zero overhead when nothing is registered: the broker
maintains a kv row (`resident_hooks_active`) of points with live
registrations; exec/dispatcher do one indexed sqlite read per chain run and
never touch the bus otherwise. Staleness: an attach mid-tool-call is seen at
the next tool call; a daemon crash leaves the row stale-active until the
next daemon start clears it (the consult then fails fast toward allow with a
backoff). (e) Degradation order holds: coordinator unreachable or verdict
lost → allow, loudly echoed — broker down means resident hooks don't exist,
while the exec-hook chain and recording are untouched.
**[OPEN]** Whether the render-provider contract folds into pre-LLM-request
hooks (a provider is arguably a hook that appends context).

## Recorder: disk is a set of subscription patterns

**[DECIDED]** The recorder is an in-process consumer — never an MQTT client —
evaluating pattern rules:

```toml
[[record]]
match = "work/#"
sink  = "ledger"          # sqlite

[[record]]
match = "obs/exec/#"
sink  = "trace"           # trace.jsonl; rotation policy lives here

[[record]]
match = "obs/ui/#"
sink  = "none"            # live-only, never touches disk
```

trace.jsonl semantics unchanged from v1: append-only, write-only, nothing
reads it for control flow, thinking excluded (transcripts hold it).

**[DECIDED 2026-06-11]** Flight recorder vs reconstruction views — split the
concern. The in-process recorder is a **WAL**: each process records its own
happenings synchronously in the write path, then mirrors to the broker
best-effort (`el-mirror`). It stays in-process — it is ~250 LOC total, and
the security posture depends on it ("the audit ledger bounds a package's
blast radius" is only true if no package can sever the recorder by killing a
connection). File order across processes was never the truth — trace.jsonl is
multi-writer O_APPEND already; `ts` + `cause_id` + `correlation_id` are the
ordering mechanism, the file is a crash-proof raw feed, not a timeline.
**Reconstruction views** — per-agent traces, search indexes, conversation and
room views, anything agents read to rebuild reality — are userland
subscribers, never kernel. The multi-agent future means many *views*, not
kernel growth; agents reconstructing reality read the ledger and transcripts
(transactional, causal), not the trace.

## Packages: skills, clients, actors

**[DECIDED]** `skills/` and `handlers.d/` are replaced by `packages/`. A
package is a directory; its contents declare what it is:

```
packages/discord/
  SKILL.md          # optional: it's a skill (agentskills.io, stays pure)
  elanus.toml       # optional: it's a bus client — requests, supervision, hooks
  scripts/...
```

**[DECIDED]** `harness.toml` → `elanus.toml`. An ecosystem-facing manifest wants
the tool-named convention (`Cargo.toml`, `package.json`) for grep-ability and
uniqueness. **[DECIDED 2026-06-16, supersedes the earlier "keep role names"
carve-out]** the product is named `elanus` end to end: the ledger file is
`elanus.db` (auto-migrated from `harness.db` at first open) and the canonical env
vars are `ELANUS_*` (`ELANUS_ROOT`, `ELANUS_DB`, …). The old `HARNESS_*` names
keep working — the kernel reads them as a fallback and sets them as legacy
aliases on every child process (`src/envcompat.rs`) — so existing shells and
custom package scripts don't break. Cron, provider, and throttle declarations
carry over unchanged; v1 `[[handler]]` declarations become subscription requests
with `mode = "exec"`.

**[DECIDED]** Discovery via per-agent `elanus_path = [...]`. Missing
`elanus_path` inherits from the parent scope; `"$parent"` includes that scope at
that point, e.g. `elanus_path = ["kits/dev", "$parent"]`. An agent inherits its
parent profile, profiles without an explicit parent inherit `default`, and
`default` inherits the built-in instance/global path. Each entry is either a
package directory or a kit directory with a `packages/` child. The path is
ordered, first-hit-wins name shadowing —
systemd unit load path semantics (`/etc/systemd/system` > `/run` >
`/usr/lib`, including override-by-shadowing).

**[DECIDED]** **Discovery is not authority — packages are actors.** A
discovered package boots into a zero cage: read its own dir, write its
scratch, publish its own `obs/skill/<name>/status`; exact floor is [OPEN] in
sandbox.md. Its manifest is a standing **request**, never a self-grant —
otherwise anything that can write a directory onto the path grants itself
`subscribe = ["#"]` and exfiltrates every session. Approval appends to the
grant ledger, pinned to the manifest hash: a package that edits its manifest
re-enters pending for the delta (browser-extension re-prompt semantics).
Approved capabilities attach live; no restart. This is the phone-app model —
install is one gesture *because* install grants nothing — and it is what
makes the activation UX smoothable without sacrificing correctness. The
open-strix property survives strengthened: third-party manifests are
untrusted requests *by type*, and the approval ledger stays the
diff-reviewable artifact.

```toml
# packages/discord/elanus.toml — requests, not grants
[request]
subscribe = ["$share/discord/work/discord/#", "ingress/discord/#"]
publish   = ["obs/skill/discord/#", "ingress/discord/#", "work/agent/exec", "signal/pain"]
blocking  = []                  # hook capability is its own explicit request
fs_write  = []                  # durable fs beyond scratch; leases cover the dynamic rest

[process]
mode             = "daemon"     # or "exec" (per-event fork/exec, v1 handlers)
run              = "scripts/main"
restart          = "backoff"
session_expiry_s = 30           # short: shared-group redelivery keys on session termination
```

Enforcement is locality-dependent; the request/grant language isn't
(principle 1: interface unification). Remote MQTT clients (phones, UIs,
bridges) get protocol-side enforcement: per-filter SUBACK 0x87 Not authorized
(§3.9.3), PUBACK 0x87 on QoS 1 publish, silent-drop-plus-obs-event on QoS 0.
A denied SUBSCRIBE echoes to `obs/` and can climb the variety ladder —
handler → `human/ask` → approval appended → client retries. Authorization is
just another event flow.

**[KNOWN GAP — security review 2026-06-11, the containment boundary is not
yet closed against a *malicious local package*; honest accounting here so the
"local children get the cage plus ACL" line above is not read as more than it
is].** What is actually enforced today:

- **The OS cage bounds file *writes*** for daemon actors and the agent's
  shell tool — to scratch + approved `fs_write` (+ leases). Reads are open,
  network/loopback is open, and **exec-mode handlers are not caged at all and
  receive `ELANUS_DB`** (they read/write the ledger directly — watchdog and
  escalation are ledger-readers by design). So the cage is a write-fence on a
  subset of spawn paths, not a sandbox.
- **The bus ACL is authentication-gated, and authentication is presently
  opt-out.** A session that presents a valid actor token is scoped to its
  grants; a session that presents *no* credentials is treated as "the human"
  with full access. A local package is also a local client: nothing stops its
  script from connecting to the loopback broker without its token (the cage
  permits loopback) and getting human authority — reading every session's
  `obs/exec/...`, driving `work/agent/exec`, resolving `human/answer`. The
  per-connection ACL contains a *cooperative* package, not a hostile one.

The root cause is not "no auth" — auth exists and scopes a token-bearing
session correctly. It is that **authority is decided by locality (local code
is trusted) and a package is local code.** "Add authentication" is the fix
that looks right and isn't sufficient, because the gap has three interlocking
legs that must move roughly together; closing one alone closes nothing:

1. **Bus authorization default** — unauthenticated must become *deny*, not
   "the human." Necessary, but on its own a package just keeps using (or
   re-reads) a credential.
2. **fs_read scoping** — once unauthenticated = deny, the privileged path (the
   human CLI, the kernel mirror) needs a credential, e.g. a 0600 cookie; with
   reads open the package simply reads the cookie and presents it. So leg 1 is
   load-bearing only *with* read scoping. (sandbox.md defers this.)
3. **exec-handler containment** — this is a *separate door the bus does not
   guard at all*: exec handlers run uncaged with `ELANUS_DB`, so a hostile
   exec package never touches the bus — it opens the ledger directly, inserts
   `work/agent/exec`, reads the transcripts table. Caging their writes and
   removing the raw DB handle collides with watchdog/escalation being
   ledger-readers by design, so it needs its own pass.

(Network-egress control, also deferred in sandbox.md, is the cleaner cut for
leg 1: a package with no egress grant cannot open the loopback bus connection
at all.) So the honest boundary today: **the OS write-cage + the audit ledger
bound a package's *filesystem* blast radius; the bus ACL and the ledger are a
correctness/teamwork boundary, not yet a security one against hostile local
code.** Consistent with Tim's untrusted-package threat model only once those
passes land; until then the mitigation that makes punting safe is that
**packages are human-installed** — the write-cage and the audit ledger bound
what an installed-but-buggy or injection-compromised package can reach, but a
*deliberately* malicious package is not contained. Do not present the bus ACL
as a sandbox to a user installing third-party packages until legs 1–3 land.

**[DECIDED / BUILT]** Identity model (supersedes the old "loopback = the human"
assumption, which the review showed is unsound against local packages):
*unauthenticated is deny, not allow.* The broker handshake (`src/broker.rs:424`,
`handshake`) refuses both wrong credentials and no credentials — a connection
with no verified identity gets none — so privileged local clients (the human
CLI, the kernel mirror, exec processes) must present a verified identity
(username + a fenced secret, or a package's supervisor-minted spawn token) to
get anything past the handshake. Still does not fully contain a package until
fs_read scoping lands (leg 1 above).

Both process modes coexist: daemons for adapters/indexers (warm state,
websockets, ingress — the hole v1 hand-waved), exec for stateless scripts.
SKILL.md stays pure agentskills.io, unchanged. Authority semantics — grants
vs leases, the whole-agent grant, fs events — live in
[sandbox.md](sandbox.md).

## Skill lifecycle: crash-only

**[DECIDED]** Daemon packages boot into the zero cage (capabilities attach
as approvals land; no restart) and connect with a retained will
(`obs/skill/<name>/status` → `dead`), publish `alive` retained on connect.
QoS 1 + manual acks + ack-after-processing; crash anytime. Crash → will fires
→ supervisor restarts with backoff → session resumes → unacked messages
redeliver. The supervisor (kernel) owns what the protocol lacks:

- **Poison parking**: N crash-loops on the same message → park it + emit
  `signal/pain`. (This is the DLQ we build; the spec has none.)
- **Hung-but-connected liveness**: no visibility timeout exists; supervision
  substitutes (status heartbeats on the retained topic, restart on staleness).
- Short `session_expiry_s` for shared-group workers, per the spec finding.

## The variety ladder (doctrine)

**[DECIDED]** Escalation policy, named after Ashby's law: each rung absorbs
the variety it can; overflow escalates via `signal/*` — the algedonic channel
is the clutch between rungs.

```
hooks      reflexes; microseconds; can veto/rewrite; zero tokens
  ↓ overflow: can't decide → emit signal or let work plane handle
handlers   scripts absorb routine variety; zero tokens
  ↓ overflow: failure, watermark, novelty → signal/pain → escalation handler
agent      expensive regulator; emits work/agent/exec with context
  ↓ overflow: uncertainty, authority limit → human/ask
human      most expensive; deadline + default so even this rung is non-blocking
```

Ingress flows accordingly: `ingress/<source>/#` → triage handlers (scripts)
handle what they can → the residue becomes agent work. **The agent runs as
late as possible.** Conversely `delivery/*` receipts let the escalation
handler stop re-pinging a human who already acked on another channel.

## Migration from v1

1. **Topic grammar**: dots → paths; one filter language (MQTT §4.7) for
   handlers, throttles, recorder; percent-encoding rule; `fs/` family
   reserved. Mechanical. Interim shim: `handlers.d/` dirnames keep dots,
   converted at match time — the directory dies in step 5.
2. **Hook plane, pre-bus**: `[[hook]]` exec-style in the exec loop and
   dispatcher (pre/post tool call first). Works without the bus; resident
   hooks arrive with it. Independently valuable as the sandbox seam.
3. **Cage + camera**: sandbox spawn wrapper for agent tool execs (whole-agent
   grant only, no leases yet) + boundary diff → `fs/` events as trace lines.
   Pre-bus, independently valuable. See [sandbox.md](sandbox.md).
4. **The bus**: ntex-mqtt spike → micro-broker (or rmqtt fallback); mirror
   events + trace onto topics; recorder rules. *Landed 2026-06-10*: recorder
   (src/recorder.rs), micro-broker on ntex-mqtt 8.x in the daemon
   (src/broker.rs — subscription table, retained store, per-filter SUBACK,
   QoS 0/1 fan-out, work/signal/human ingress → ledger), kernel publish path
   + loopback mirror (src/bus.rs), `elanus bus pub|sub` debugging surface.
   *2026-06-11*: $share groups and completion fan-in landed with
   work-plane-on-bus (see the GAP CLOSED block under Work plane); resident
   hooks landed the same day (see the LANDED block under Hook plane) —
   step 4 is complete.
5. **Packages**: `packages/` + `elanus.toml`, request/approval ledger,
   leases, supervised daemons + LWT; `handlers.d/` and `skills/` retire.
   *Landed 2026-06-10 (commit 07720b3).* As-built notes, where reality
   refined the design: (a) v1's per-package `[[handler]]` list collapsed to
   one `[process]` per package — a package does one thing; its script
   dispatches on the envelope's `type`. (b) Approval is all-or-nothing per
   package for now; the `elanus approve` printout is the review surface;
   per-capability decisions are a CLI growth, the ledger already stores
   rows individually. (c) Manifest-edit semantics: unchanged (kind, value)
   pairs carry over under the new hash (`decided_by = 'carried'`), the
   delta re-enters pending, revoked values re-ask. (d) Actor identity =
   per-spawn supervisor-minted tokens via env; `elanus bus pub|sub` picks
   them up automatically, so script actors authenticate for free.
   Anonymous loopback clients (the human) keep full access — open
   question 7 stands for remotes. (e) The supervisor publishes retained
   `obs/skill/<name>/status`; client LWT is honored too (ACL-checked,
   fires on abnormal close only). (f) Leases landed per sandbox.md: tool
   call surface, kernel borrow checker, cage narrowing.
6. **Ingress bridge** (Discord first) — the first real daemon package.
7. **Delivery receipts + escalation handler** — pure userland; can land any
   time after 4.

## Open questions (consolidated)

Resolved since first draft: handlers.d (retired in favor of `packages/`);
topic-filter-vs-filename encoding (percent-encode `+ # %`; interim dots shim);
resident-hook transport (MQTT request/response — see Hook plane, 2026-06-11);
topic grammar redesign agreed in [topics.md](topics.md) (2026-06-11 — its own
[OPEN] list, each with a recommended default).

1. ntex spike: embedding ergonomics, latency of resident-hook round trip,
   LOC reality check. Fallback trigger to rmqtt is "broker logic > ~2k LOC or
   System embedding fights tokio."
2. Shared-group (`$share`) vs per-skill queues for work topics.
3. Render providers → pre-LLM-request hooks?
4. Recorder rotation/retention knobs per pattern.
5. Poison policy parameters (N, backoff curve, park location).
6. Ledger schema changes for topic-shaped types (probably just rename
   `type` values; `emitted_by_dispatch` and correlation machinery carry over).
7. Identity/auth for non-local MQTT clients (local children = peer creds;
   remotes need tokens).
8. Zero-cage floor and spawn policy for untrusted package roots (sandbox.md).
9. Exclusive publish leases on topic prefixes for source authenticity
   (sandbox.md).

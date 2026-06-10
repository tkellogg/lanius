# elanus v2 — The Bus Architecture

> Status: design, agreed 2026-06-10. Supersedes the *architecture* of
> [init.md](init.md); init.md remains the accurate record of v1, which is
> implemented and live-tested on `main`. Nothing in this doc is built yet.
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
signal/{pain,anomaly,...}         algedonic; never coalesced, never queued behind
human/{ask,answer}
delivery/<channel>/{sent,acked}   receipt events; escalation reads these
ingress/<source>/...              external arrivals, pre-triage
```

Envelope unchanged from v1: `{ts, topic, cause, correlation, payload}`.
Dots→slashes migration is mechanical. **[OPEN]** Whether `handlers.d/` survives
as a materialized *view* generated from grants (for `ls`-ability) or dies; lean
keep-as-view — debugging stays `ls` even when routing is dynamic.

## The bus

**[DECIDED]** In-process bus; MQTT 5 listener as the boundary; privileged
in-process consumers for ledger/recorder/hooks/exec.

**[DECIDED — lean, spike before committing]** Build the micro-broker on
**ntex-mqtt 8.x**. Research verdict (June 2026): it is the only option meeting
every requirement *natively*:

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

**[OPEN — fallback]** If the micro-broker spike runs heavy: embed **rmqtt
0.21+** (very active, documented library mode, tokio-native). It has the right
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

**[OPEN]** Resident-hook transport: in-process trait object vs MQTT
request/response round trip — measure the latency budget in the spike.
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

## Grants: the manifest changes jobs

**[DECIDED]** `harness.toml` stops being routing config and becomes a
**capability grant + supervision spec**. Routing goes dynamic (subscription IS
registration); the *envelope* a skill may operate in stays a diff-reviewable
artifact — preserving the open-strix security property. Without grants, any
skill could SUBSCRIBE `#` and exfiltrate every session's observations.

```toml
[grant]
subscribe = ["$share/discord/work/discord/#", "ingress/discord/#"]
publish   = ["obs/skill/discord/#", "ingress/discord/#", "work/agent/exec", "signal/pain"]
blocking  = []                  # may not register hooks; hook grants are explicit

[process]
mode             = "daemon"     # or "exec" (per-event fork/exec, v1 style)
run              = "scripts/main"
restart          = "backoff"
session_expiry_s = 30           # short: shared-group redelivery keys on session termination
```

The bus enforces grants as per-connection ACLs (skill identity = client id,
authenticated locally). Grant changes are commits; approval is reviewing the
diff — same flow as sandbox policy. Both process modes coexist: daemons for
adapters/indexers (warm state, websockets, ingress — the hole v1 hand-waved),
exec for stateless scripts. SKILL.md stays pure agentskills.io, unchanged.

## Skill lifecycle: crash-only

**[DECIDED]** Daemon skills connect with a retained will
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

1. **Topic grammar**: dots → paths; one filter language for handlers,
   throttles, recorder. Mechanical.
2. **Hook plane, pre-bus**: `[[hook]]` exec-style in the exec loop and
   dispatcher (pre/post tool call first). Works without the bus; resident
   hooks arrive with it. Independently valuable as the sandbox seam.
3. **The bus**: ntex-mqtt spike → micro-broker (or rmqtt fallback); mirror
   events + trace onto topics; recorder rules.
4. **Grants + supervised daemons + LWT**; `handlers.d/` becomes a generated
   view or retires.
5. **Ingress bridge** (Discord first) — the first real daemon skill.
6. **Delivery receipts + escalation handler** — pure userland; can land any
   time after 3.

## Open questions (consolidated)

1. ntex spike: embedding ergonomics, latency of resident-hook round trip,
   LOC reality check. Fallback trigger to rmqtt is "broker logic > ~2k LOC or
   System embedding fights tokio."
2. Shared-group (`$share`) vs per-skill queues for work topics.
3. Render providers → pre-LLM-request hooks?
4. `handlers.d/` as generated view: keep or retire?
5. Recorder rotation/retention knobs per pattern.
6. Poison policy parameters (N, backoff curve, park location).
7. Topic-filter encoding anywhere a filesystem name is needed.
8. Ledger schema changes for topic-shaped types (probably just rename
   `type` values; `emitted_by_dispatch` and correlation machinery carry over).

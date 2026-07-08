# elanus v3 — Topic Grammar

> Status: design, agreed 2026-06-11. Supersedes the **topic grammar section**
> of [bus.md](bus.md) — which remains the accurate record of the v2 grammar
> as built — and nothing else; planes, broker, recorder, packages all stand.
> Nothing in this doc is built yet. Same conventions: **[DECIDED]** is settled
> with rationale, **[OPEN]** needs a decision — every [OPEN] here carries a
> recommended default so implementation can start on quick yes/no calls.

## Shape

**[DECIDED]**

```
{verb}/{category}/{noun}/{locators & IDs...}

in/agent/alice/c123                      message into alice's mailbox, conversation c123
obs/agent/alice/s42/tool/shell/call      telemetry about alice, session s42
obs/fs/Users/tim/notes.txt               a file is a noun; the noun runs to end-of-topic
signal/pain                              algedonic lane, top-level, unchanged
```

**Verb first, because the first segment is a routing instruction, not
decoration.** Three arguments, each sufficient:

1. **The broker is not neutral.** It decides delivery contract by prefix
   (v2: `work/signal/human` → ledger, `obs/` → fan-out). In a generic pub/sub
   deployment the hierarchy is purely organizational and noun-first reads
   nicely (`clients/<id>/responses`); ours is the boundary of a kernel with
   delivery contracts. Verb-first makes the contract decidable at segment 1,
   for topics never seen before, from clients we don't trust, with zero
   knowledge of noun shapes. The kernel stays dumb.
2. **Closed alphabet at the fixed position, open alphabet in the tail.** The
   verb set is tiny and closed (`in`, `obs`, `signal`); the noun set is open
   and includes *multi-segment* nouns — file paths today, possibly nested
   groups tomorrow. With a trailing or embedded verb you cannot parse where a
   variable-length noun ends and the verb begins (`fs/Users/tim/obs/x` — verb
   or directory?). Verb-first: noun runs to end-of-topic, unambiguous.
3. **The load-bearing subscribers are plane-shaped.** Broker routing
   (`in/#` → ledger), recorder rules (`obs/#` → trace), hook registrations
   ("all tool calls anywhere"), escalation (`signal/#`). Noun-first turns each
   into `+/+/in/#` — legal, but correct only while every noun is exactly two
   segments deep, forever: security and recording rules coupled to the locator
   conventions of every future category. Entity-shaped consumers (UIs,
   dashboards) pay one character under verb-first: `+/agent/alice/#`.

Precedents, read honestly: Sparkplug B (`{group}/{MESSAGE_TYPE}/{node}` —
direction verbs NCMD/NDATA relative to the noun, at a *fixed* shallow
position) and AWS IoT shadows (noun-first, verbs last — workable only because
thing names are forced to one segment by a managed service). Both demonstrate
the same rule: the closed set lives at a fixed offset and everything around
it gets rigidified to keep it fixed. Top-level is the one offset that is
fixed for free, with no rigidity tax on nouns.

## The verbs are delivery contracts

**[DECIDED]** v2's planes (bus.md) wearing better names — this is why the
redesign is more than cosmetic:

| verb | contract | v2 equivalent |
|---|---|---|
| `in/` | addressed, at-least-once, ledger-backed; the recipient's mailbox is the single durable copy | work plane (`work/`, `human/`) |
| `obs/` | telemetry; QoS 0, droppable; persistence opt-in via recorder rules | observation plane (`obs/`) |
| `signal/` | algedonic; never coalesced, never queued behind anything; not entity-relative — survives top-level outside the verb scheme | `signal/` |

**`out/` deliberately does not exist.** Every message between two parties is
one party's out and the other's in; making both real doubles every publish
and buys a consistency obligation forever. Mailbox model (email, actors,
Erlang — none has a wire outbox): the recipient's `in/` is the durable copy;
the sender's emission is an observation *of the sender* and lands under
`obs/`. `err` likewise collapses into `obs/` (or escalates as `signal/`).
Per-recipient acks are exactly what the work plane runs on, so group delivery
("all members have processed this") falls out of the completion fan-in the
kernel already owns.

Broker routing rule becomes cleaner than v2's prefix list: `in/#` and
`signal/#` materialize to the ledger; `obs/#` fans out only.

## Identity is first-class

**[DECIDED]** Agents are named nouns in the topic space. v2 has sessions but
no agent identity — fine for one agent, wrong for a system hosting many.
Capability shapes fall out of addressing: "this package may talk to alice" =
publish grant on `in/agent/alice/#`. Grants, recorder shards, and UI filters
all key on the same segments.

## IDs: three layers, one per job

**[DECIDED]** Same word "correlation" appears at two layers; they are
different things and both stay:

| ID | layer | lifetime | tracing analog |
|---|---|---|---|
| MQTT Correlation Data | packet property | one request/response pair | RPC request ID |
| envelope `correlation_id` | application | one flow, many events | trace ID |
| envelope `cause_id` | application | one edge (event's parent) | parent span pointer |

MQTT Correlation Data is used by the resident-hook round trip (see bus.md
hook plane) and never touches the envelope. The envelope pair is v1
machinery, unchanged.

**[DECIDED 2026-06-11, as-built]** External clients attach the *envelope*
correlation to a publish via the **`el-correlation` user property** (the
`el-*` namespace, like `el-mirror`); the broker materializes it into the
ledger event's `correlation_id` on `in/#`/`signal/#` topics and echoes it on
the announced line. This is how a pure MQTT client answers an ask such that
the suspended asker actually resumes. `elanus bus pub --correlation` sets it.
Correlation Data stays reserved for hooks — keeping the taxonomy above
intact.

**[DECIDED]** **Conversations get their own identity — as a topic locator,
not by overloading `correlation_id`.** The dispatcher matches resumes by "the
unanswered ask with this correlation" (flow-scoped); a conversation is a
*container* of many flows, and overloading would make that matching ambiguous
again (we already fixed one hot-loop bug on exactly this seam). Conversation
ID rides the topic: `in/agent/alice/c123`. Payoffs: "x-ray this conversation
across all mailboxes" is one filter (`in/+/+/c123`); recorder rules can shard
per conversation; the envelope keeps correlation/cause for flow structure
within it. Minting: the initiator mints (opaque string, ULID-ish); the kernel
treats it as a locator, nothing more.

## Conversations and rooms

**[DECIDED]** 1:1 conversations are **correlation, not location** — N mailbox
deliveries threaded by the conversation locator (email solved group
communication without a room; threading is a view). Any observer or indexer
materializes the conversation as a view, in userland.

**[DECIDED]** A group chat is **just a new noun**: `in/group/<id>` — a
conversation-as-actor with its own mailbox (Erlang: a chat room is a
process). No new prefix, no new protocol. The only protocol is: mint the
noun, share the address, publish `in/`, subscribe. "Master sets up a room for
subordinates" = master mints and hands the address in spawn context; "ad-hoc"
= anyone mints and DMs invitations (`in/agent/bob`, payload: join g123). Same
protocol; only who mints differs.

- **Not named `shared/`**: `$share/` is MQTT-reserved and means the opposite
  (competing consumers, one-of-N delivery; a room is everyone-receives).
- **Rooms are ledger-backed.** Late joiners need history and MQTT cannot
  provide it (retained = exactly one message per topic). A subagent spawned
  mid-conversation reads history from the ledger; live members ride ordinary
  subscriptions on top. Topics are transport, ledger is truth — again.

## v2 → v3 mapping sketch

Leans, to be confirmed during migration; **[OPEN]** items called out below.

| v2 | v3 lean |
|---|---|
| `work/agent/exec` | `in/agent/<name>` |
| `work/<pkg>/<kind>` | `in/package/<pkg>/<kind>` |
| `human/ask` | `in/human/<name>` (deadline machinery unchanged) |
| `human/answer` | `in/agent/<name>/<conv>` (correlation matches the ask flow) |
| `obs/exec/<sess>/...` | `obs/agent/<name>/<sess>/...` |
| `obs/skill/<n>/status` | `obs/package/<n>/status` (retained + LWT semantics unchanged) |
| `obs/hook/...`, `obs/ledger/...`, `obs/dispatch/...` | `obs/harness/{hook,ledger,dispatch}/...` |
| `ingress/<source>/...` | see [OPEN] 3 |
| `delivery/<chan>/{sent,acked}` | `obs/channel/<chan>/{sent,acked}` |
| `fs/<path>` | `obs/fs/<path>` (percent-encoding rule [MQTT-4.7.1-1] carries verbatim) |
| `signal/*` | unchanged |

## Formerly-open questions — all eight leans adopted by Tim, 2026-06-11

1. **[DECIDED]** Category set: `agent, human, group, package, fs, harness,
   channel, dm` — small and closed-ish; adding a category is a design event,
   not a convention. `human` stays a distinct category — ACL and escalation
   treat humans differently by type, the grammar should too.
   **`dm` added 2026-07-08 (Handoff B) — that addition IS the design event.**
   `dm` is external-channel conversation *ingress*: a message that arrived from
   a platform, addressed by the platform's own conversation id (see #2 and
   [channels.md](channels.md)). It is distinct from `channel`, which stays the
   *receipt* noun (`obs/channel/<kind>/{sent,acked}` — an `obs` noun, not an
   `in` verb); `channels.md` weighed reusing `in/channel/...` for ingress and
   rejected it, because the `dm` address now names *a conversation*, which is
   what recall keys on. Addressing under `dm` is uniform — a conversation is an
   id, and its participant set is a resolved phonebook fact, never a structural
   property of the topic; enforcing who is in a conversation is a layered
   security measure (the reserved `in/dm/` prefix of #M2 + grants + recall's
   trust rule), **not** an authority boundary drawn by the category.
2. **[DECIDED]** Locator conventions: actor categories (agent, human, group,
   package) have single-segment nouns; for `in/` the first locator is the
   conversation ID; for `obs/` the first locator is the session. `fs` nouns
   are the entire remainder.
   **`dm` is the one two-segment-noun category (Handoff B):**
   `in/dm/<kind>/<chat-addr>` — the platform `<kind>` (e.g. `telegram`,
   `discord`) *and* the platform's conversation id `<chat-addr>` together are
   the address, because recall must see `(kind, addr)` on the broker-verified
   topic. `<chat-addr>` is `encode_segment`-ed (`src/topic.rs`) so a platform
   id containing `/` cannot add levels. The conversation IS the address: a 1:1
   DM and a group chat are the *same* shape — `in/dm/telegram/<chat.id>` either
   way — the difference (one resolved participant vs. several) is a phonebook
   fact, not a different topic.
3. **[DECIDED]** Ingress: the twin-publish dies — an arrival is published
   once, addressed to its handler (`in/package/...` per-manifest routing);
   observation of arrivals comes from the delivery echo, not a second
   publish. Resolves the v2 step-6 tension.
4. **[DECIDED]** Delivery receipts: `obs/channel/<chan>/{sent,acked}`;
   escalation's filter becomes `obs/channel/+/acked`.
5. **[DECIDED]** Room membership: agent-minted rooms ride **leases**
   (dispatch-lifetime, crash-released — the borrow checker already exists);
   durable rooms (human participants) ride grants.
6. **[DECIDED]** Default nouns: agent name from profile, default `main`;
   human noun = profile owner.
7. **[DECIDED]** Hook-verdict response topics: verdicts are sub-500ms
   ephemera, never ledger-backed; the bus mints response topics under
   `obs/harness/hookresp/<id>`; the blocking grant includes publish right to
   that prefix.
8. **[DECIDED]** Ledger `type` value renames are mechanical (carries bus.md
   open question 6; `emitted_by_dispatch` and correlation machinery carry
   over).

## Migration

Mechanical, same family as v2 step 1 (dots → slashes); that precedent says a
session, not a saga. Order: `src/topic.rs` constants/helpers → broker routing
prefix rule (`in|signal` materialize, `obs` fans out) → recorder.toml
template → in-repo package manifests and scripts (escalation, notify,
linemux, discord filters) → e2e. sandbox.md's `fs/` references update in the
same pass. **Do this before work-plane-on-bus lands** — don't build new
delivery machinery on a grammar scheduled to die.

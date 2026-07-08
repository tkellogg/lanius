---
name: Channels
description: An external messaging app (Signal, Telegram, Slack, email) is not a new subsystem — it is a package that follows the ordinary bus topic protocol. There is no "transport" abstraction, and there does not need to be.
---

# Channels: a transport is just a package

**Thesis.** Reaching a person on Signal or Telegram — or letting them reach an
agent — requires **no new "transport" concept.** A channel bridge is a *daemon
package* that follows the ordinary topic protocol: inbound, it publishes what
arrived on the wire to a channel-faithful `in/` topic; outbound, it consumes a
`send` command on its own inbox and emits an `obs/channel/<kind>/…` receipt. Who
a wire address *is* — mapping a phone number to `tim` — is the existing
phonebook/recall machinery, resolved at read time from the broker-verified
topic, never frozen into the wire.

This is the design law applied ([../memory or feedback: simple core, extended
through packages]): a new capability is a **package + a grant**, not a special
case baked into the kernel. Adding Telegram should edit **zero** kernel files.

---

## Why there is no transport abstraction

Three facts already in the architecture make a "transport layer" redundant.

### 1. The channel is already in the grammar

The topic grammar is `{verb}/{category}/{noun}/{locators…}` ([topics.md](topics.md),
[topic.rs](../src/topic.rs)). Two seams already name the platform:

| direction | topic | meaning |
|---|---|---|
| inbound | `in/dm/<kind>/<addr>` | a message arrived from platform `<kind>` at wire address `<addr>` (e.g. `in/dm/telegram/<chat-id>`, `in/dm/signal/+1…`). Ledger-backed — the mailbox is the one durable copy. |
| outbound receipt | `obs/channel/<kind>/{sent,acked}` | a bridge delivered (or the recipient acked) on platform `<kind>`. Telemetry, droppable, fan-out. Escalation filters on `obs/channel/+/acked`. |

`<kind>` **is** the platform. `<addr>` is what's true on the wire. Neither
carries *who that is* — that stays a revisable query-time join (§2), so a later
phonebook correction re-unifies all history at once instead of freezing a guess
into the immutable record ([identity.md](identity.md) "Resolution is revisable").

### 2. There is deliberately no `out/` plane

**[DECIDED]** ([topics.md](topics.md)): `out/` does not exist. Every message
between two parties is one party's out and the other's in; making both real
doubles every publish and buys a consistency obligation forever. A message to
another elanus actor is a write to its `in/` mailbox; a message to the outside
world is a **direct command a daemon executes and gets *observed* doing** — the
emission lands under `obs/`, not a wire outbox. So "send to Signal" is not a
relay through some transport layer — it is a command to a specific bridge, whose
delivery is observed on `obs/channel/signal/sent`.

### 3. A "channel" is already a first-class identity concept

The phonebook ([identity.md](identity.md)) models:

```
identity( id, kind, canonical, … )                       -- tim (human)
channel ( channel_kind, address, identity_id NULL, … )   -- (telegram, <chat-id>) → tim
alias   ( identity_id, name, context NULL )
```

A **Channel is a `(kind, address)` pair** — the elanus principal is *just one of
its channels*. Channels can be logged before they're resolved to a person
(`identity_id NULL`); resolution is a query-time join; merge re-points and never
collapses, so a wrong link is a cheap re-point back. **Recall** (a stock
context-pipeline stage) assembles one identity's conversation across *every*
channel into a single linear frame — the unified inbox — keyed **only** on the
broker-verified `in/dm/<kind>/<addr>` topic (never a body field, never a
self-emitted event), because the correspondent decides *whose* history loads and
is therefore authority-bearing.

---

## Anatomy of a bridge (the two templates already exist)

A bridge is a **daemon package**. Daemons matter for provenance: they are spawned
token-authed and caged, so the broker stamps `sender=<bridge>` on every event —
an `exec`-mode handler, by contrast, runs uncaged/tokenless and would
authenticate as the *owner*, mislabeling every send ([security.md](security.md)
entry 16). Two shipped packages are the copy-paste templates:

- **Egress:** [`packages/webhook`](../packages/webhook/lanius.toml) — inbox
  `in/package/webhook/send {url,text}` → POSTs directly (off the bus) → emits
  `obs/channel/webhook/sent`. "Swap the POST for any service's SDK and this is
  the template for a real external-channel egress bridge."
- **Ingress:** [`packages/discord`](../packages/discord/lanius.toml) — a daemon
  that watches a gateway and publishes arrivals to the bus, holding its own
  channel credential (no agent ever touches it), authenticated via
  supervisor-injected `ELANUS_PACKAGE`/`ELANUS_BUS_TOKEN`, parks-not-crashes when
  unconfigured.

### Worked example — the Telegram bridge

**Inbound** (a Telegram message arrives):
1. The `telegram` daemon long-polls the Bot API (holding the bot token — no agent
   sees it).
2. On a message it publishes **once** to `in/dm/telegram/<chat-id>` — ledger-backed,
   channel-faithful, addressed by what's true on the wire. (No twin-publish;
   observation of the arrival is the delivery echo.)
3. The dispatcher routes that event to whatever handler/agent holds an approved
   `subscribe` filter matching the ingress plane
   ([dispatcher.rs](../src/dispatcher.rs) `matching_exec_handlers` → `topic::matches`;
   approval gating in [packages.rs](../src/packages.rs)).
4. When that agent runs, the **recall** stage resolves `telegram/<chat-id>` →
   identity `tim` via the phonebook and loads Tim's whole cross-channel history
   into the prompt.

**Outbound** (an agent wants to DM Tim on Telegram):
- Egress is command-shaped: publish to the bridge's inbox
  `in/package/telegram/send {recipient, text}`; the daemon delivers and emits
  `obs/channel/telegram/sent`.
- There is **no** "send to Tim, system picks the platform" primitive on the wire.
  *Which* channel to use is a **policy decision** owned by a human-proxy / EA
  actor ([journeys/reaching-the-user.md](journeys/reaching-the-user.md)) that
  reads per-channel effectiveness (ack latency on `obs/channel/+/acked`) and
  chooses. Web UI vs Telegram vs Signal = "which bridge inbox do I publish to."

The same picture explains **agent DMs in the web UI**: the web dashboard is
itself just a *relay of the `in/human/<owner>` plane*. An agent DMing you in the
browser and an agent DMing you on Telegram are the **same publish, different
renderer.** That is why the two are one arc, not two.

---

## What must be true first (four small items — none of them a transport)

The substrate is built; these are the gaps a real bridge exposes. All are
already documented; none requires a new abstraction.

1. **Reserve the `in/dm/` ingress prefix to bridge packages — DONE (Handoff B
   M2).** Recall's trust model assumes only bridges publish `in/dm/…`; the broker
   publish ACL now enforces it ([broker.rs](../src/broker.rs) `actor_may_publish`
   / `is_dm_scoped_filter`): a grant-scoped actor may publish `in/dm/…` only if it
   holds an explicitly dm-scoped publish grant (e.g. `in/dm/discord/#`) — the
   ingress-bridge capability, which only an owner-approved manifest can hold. A
   broad grant (`#`, `in/#`) or a coding session's structural scope is refused, so
   a prompt-injected agent can no longer forge `in/dm/telegram/<victim>`
   ([security.md](security.md) entry 15 — the raw-bus-grant residual, now closed).
2. **A channel bridge must be a *daemon*** (own package token) or every send is
   mislabeled as the owner ([security.md](security.md) entry 16). A rule, not a
   subsystem — the `webhook` exemplar models it.
3. **Register `owner` in the phonebook at init.** Recall doesn't work *for Tim
   himself* until there's an `identity{id:owner,kind:human}` + `(elanus,owner)`
   channel row ([identity.md](identity.md)). A small seeding step.
4. **The EA / channel-selection policy** (which platform to reach Tim on;
   escalation A→B→C) is a policy *on* the rails, not part of any bridge.

### The grammar decision — DECIDED (Handoff B, 2026-07-08): option (a)

`identity.md`/recall key on `in/dm/<kind>/<addr>`, but the closed category set in
[topics.md](topics.md) #1 formerly lacked `dm`, and DECIDED #3 routed ingress to
`in/package/<handler>/…`. The shipped Discord scaffold published
`in/package/discord/triage`, **not** the canonical `in/dm/discord/<addr>` recall
expects. So the `in/dm/*` convention was documented but not realized.

**Resolved: option (a).** `dm` joins the closed category set (topics.md #1 — that
addition is the "design event") and ingress publishes `in/dm/<kind>/<addr>`; the
dispatcher routes it to any handler subscribing `in/dm/#`. The wire stays
channel-faithful because recall's provenance needs `(kind,addr)` on the
broker-verified topic. (Option (b), reusing `channel`, was rejected — `channel`
stays the *receipt* noun; option (c), carrying `(kind,addr)` in the payload, was
rejected because recall must key on the topic, never a body field.)

#### A DM is the degenerate case of a group chat — address the conversation, not the person

The wire address under `dm` is the **conversation**, identified by the platform's
own conversation id. Telegram already models it this way: every message carries a
`chat.id` whether the chat is private or a group. So a 1:1 DM and a group chat are
the **same shape** — `in/dm/telegram/<chat.id>` either way — and the difference
(one resolved participant vs. several) is a phonebook fact, not a different topic.
This is the DM/group conflation, and it lands *inside* `dm`: there is **no**
separate "group DM" wire category.

#### Addressing is uniform; membership is a security layer, not a category

A conversation is addressed by an **id**; its participant set is a *resolved
fact*, never a structural property of the topic. So the category is **not** the
authority boundary — the broker/MQTT does not structurally enforce who is in a
conversation anyway. Enforcing "only these participants may publish / be recalled"
is a **layered security measure**, applied per conversation:

- **the reserved `in/dm/` prefix** (item 1 below, now enforced) — only a package
  declaring the ingress-bridge capability may publish external ingress; a
  prompt-injected agent can't forge `in/dm/telegram/<victim>`.
- **publish/subscribe grants** — who may write to or read a conversation is an
  explicit grant checked in the broker ACL.
- **recall's trust rule** — the correspondent is taken only from the
  broker-verified topic, so which threads load can't be widened from a payload.

**Embrace multi-channel reachability.** A person or an agent is reached through a
DM *and* several group chats at once — the model embraces this: uniform
conversation addressing, identities and participants resolved through the
phonebook, security applied as an explicit layer where a boundary must hold. (The
existing internal `group` category — `in/group/<id>` — is not refactored here;
its membership rules are just its own security layer, and folding internal rooms
into the one conversation model is a plausible future direction, out of scope.)

---

## The principle, made concrete

Standing up Telegram touches **zero kernel files** — it is a new package plus
grants (`subscribe in/dm/#` on ingress, an inbox for egress) plus phonebook rows.
The places where that is *not yet* true are exactly the core special-cases the
audit flagged and that this work should fix in passing:

- `web.rs` `is_worker_session` / `source_for` **hard-code the channel taxonomy**
  (`web-`, github/jira/linear, cron) into the core conversation projection —
  adding a channel edits the kernel. The right seam already exists (`source_for`
  prefers an explicit `payload.source`); the hard-coded fallbacks should move
  into channel/package descriptors.
- The template done right: `withheld_builtin_tools`
  ([packages.rs](../src/packages.rs)) derives `send_message`/`ask_human` from
  **package visibility on the profile path**, not a conditional. That is the
  capability-from-package model every channel should follow.

> A channel is data + a package + a grant. The kernel should never learn a new
> channel's name.

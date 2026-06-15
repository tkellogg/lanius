# recall — the conversation with a person, across every channel

`recall` is a context stage that, when a message arrives from someone, puts
the whole conversation with that **person** in front of the agent — not just
the thread on the channel the message happened to arrive on. If Tim writes on
Bluesky today and wrote on Discord yesterday, the agent sees one linear frame
for "Tim," in time order, as if it were a single chat.

It is the payoff of the identity model's resolve-at-recall choice (see
docs/identity.md): incoming topics stay **channel-faithful**
(`in/dm/<channel_kind>/<address>`), and the unification happens here, at recall
time, as a join over the phonebook and the ledger. Nothing is frozen — when a
phonebook link is corrected, every past and future recall re-unifies for free.

## How it decides who to recall (and why only this way)

The correspondent is taken **only** from the broker-verified, channel-faithful
event topic `in/dm/<channel_kind>/<address>` (percent-decoded) — a kernel-
stamped fact — and **only** when the event was not emitted by the running agent
itself. It is **never** read from a message body field. That is deliberate: who
you are talking to is authority-bearing (it decides whose history loads), so a
prompt-injected agent must not be able to name a correspondent — in a payload,
or by forging its own dispatch — and pull another person's messages into its
prompt (docs/identity.md). With no trustworthy correspondent, recall adds
nothing — it never guesses.

## How it resolves and assembles

1. Ask the phonebook (its HTTP read plane) to `resolve` the channel to an
   identity, then list that identity's channels.
2. Gather the ledger messages across all those channels' `in/dm/...` topics,
   in time order, into one frame, injected as a `recall` system block.

Resolution is **best-effort**: if the phonebook is unavailable, returns an
unexpected shape, or the channel is not yet matched to anyone, recall falls
back to the single channel's own thread. Recall is enrichment — it degrades, it
does not fail the run for it. (A genuine fault in its own ledger read fails
*that run* closed, like any stage; only bus trouble is crash-only and restarts
the daemon.)

## For ingress bridges

To make a channel's messages recallable, publish each inbound message
channel-faithfully — **on the bus, which is what records it to the ledger**:

```sh
# 1. record who the handle is (create the identity, then link the channel —
#    or record the channel unresolved now and link it later)
elanus bus pub in/package/phonebook/identity '{"id":"tim","kind":"human","canonical":"Tim"}' --qos 1
elanus bus pub in/package/phonebook/channel  '{"channel_kind":"bluesky","address":"@tim.bsky","identity":"tim","confidence":1.0}' --qos 1
# 2. publish each inbound message on the channel-faithful topic, QoS 1
elanus bus pub "in/dm/bluesky/@tim.bsky" '{"text":"hey"}' --qos 1
```

- The message text goes in **`payload.text`** (recall reads `text`, then
  falls back to `prompt`/`answer`/`question`).
- `in/dm/<kind>/<addr>` is the **inbound** plane only; egress is a direct send
  that emits an `obs/` record (docs/actors.md) — never reuse `in/dm/` for it.
- Percent-encode `% + # /` in the address segment (Discord `tim#1234` →
  `in/dm/discord/tim%231234`, SMS `+1555…` → `in/dm/sms/%2B1555…`), matching
  `src/topic.rs`.

## Composing with recent-history

`recall` (order 25) is the *correspondent's* cross-channel history;
`recent-history` (order 30) is the agent's own recent mail. They are
complementary and both run if approved.

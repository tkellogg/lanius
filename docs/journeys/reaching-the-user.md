---
name: Reaching the user (the human-proxy / EA actor)
description: How an agent decides where and how to get hold of a person who is one identity across many channels — a policy layer on the built phonebook/recall/egress rails.
status: stub
---

# Reaching the user

> **Stub.** Captured from a planning chat (2026-06-24) so the idea isn't lost.
> Not yet a worked journey. The substrate it sits on is largely *built* (see
> "What already exists"); what's missing is the **policy layer**.

## The problem (Tim, from the open-strix experience)

A person is **one identity reachable many ways** — the elanus interface, a phone,
Bluesky, Teams, email, a front door — and each channel has *varying
effectiveness* depending on time of day, urgency, what the person is doing, how
they feel about that channel, and a dozen other angles. In open-strix this was a
persistent struggle: the agent never reliably knew *where to respond* or *how to
reach me*, and forcing the decision at message-arrival time (with the least
information) made it worse. "Bluesky, Teams, email, and direct chat are all me"
is true, but they are not interchangeable.

## The idea: an on-behalf-of / EA actor

A **human-proxy actor** whose whole job is *getting hold of the person*. Concretely
a rules engine — or a literal LLM agent acting as an executive assistant — that:

- decides **which channel(s)** to use for a given message, by criteria (urgency,
  recency of response on each channel, time, sender, topic, the person's stated
  preferences);
- **escalates**: try channel A, and if no acknowledgement within a deadline, fall
  to B, then C;
- can **set up private comms** between the true user and another actor (broker a
  side channel) when that's the effective move;
- is genuinely useful precisely as the *single place* that owns "how do I reach
  Tim right now" so no other agent has to solve it.

Per [../actors.md](../actors.md), "an actor uses zero or one language model" — so
the rules-engine version and the LLM-EA version are **the same socket, two
implementations**. Start with rules; swap in a model when the judgement gets hard.

## What already exists (the rails — mostly built)

This is the important part: most of the substrate shipped with the identity model
([../identity.md](../identity.md), increments 2/4/5). The EA is the *policy* on
top, not a new subsystem.

- **Phonebook** (`packages/phonebook`, SQL, shared) — which channels belong to
  which identity, **many per identity**, each with **confidence + provenance**.
  This is "all of these are me." Channels can be recorded before they're resolved
  to a person; merge is non-destructive (split reverts). The *where to reach
  them*. ([../identity.md](../identity.md) "The phonebook".)
- **Recall** (`packages/recall`, a context stage) — assembles the conversation
  with a person **across every channel the phonebook knows**, as one frame. The
  unified inbox, already built (provenance-gated: the correspondent comes only
  from the broker-verified topic).
- **Egress** (`packages/webhook`, a **daemon bridge**) — the send-out exemplar:
  direct delivery off the bus + an `obs/channel/<kind>/sent` record, **no `out/`
  plane**. For a real channel (Teams/email/Bluesky), copy the daemon shape and
  swap the POST for the service SDK whose credential the bridge holds — so no
  agent handles credentials. ([../actors.md](../actors.md) "Egress is
  command-shaped".)
- **Human-proxy packages** `notify` (desktop tap) and `escalation` — the stock
  "get hold of the human" handlers, matching `in/human/#`
  ([../identity.md](../identity.md):489). The EA generalizes these.
- **The ask/deadline/default machinery** — the "human rung" already carries a
  deadline + default so even the most expensive rung is non-blocking
  ([../bus.md](../bus.md):531), which is exactly the primitive escalation needs.

## The asymmetry that shapes it ([../actors.md](../actors.md):164)

- **Ingress is event-shaped** — inbound messages arrive over the bus through
  small bridges, addressed by the channel they *actually arrived on*
  (`in/dm/bluesky/<handle>`, not `in/dm/tim`); *who that is* is resolved later by
  the phonebook, never frozen into the topic.
- **Egress is command-shaped** — sending out is a specific action with a result;
  it goes *direct* (HTTP/SDK) and leaves a bus *record*, not bus *transport*.

So the EA reads inbound effectiveness signals off the bus (acks, response
latency per channel) and acts via direct egress commands — it is a natural
bus-citizen on both sides.

## Known gaps to close first

- **The owner isn't auto-registered in the phonebook** (deliberately deferred,
  [../identity.md](../identity.md):515). So recall — and therefore an EA reasoning
  over "every channel for Tim" — doesn't work *for the owner* out of the box; the
  owner is reached as the agent's human via `in/human/<owner>`, not as a phonebook
  correspondent. Seeding an `identity{id:owner, kind:human}` + `(elanus, owner)`
  channel at init/phonebook-startup is the small first step.
- **Egress provenance** — an exec-handler send authenticates as the owner and
  mislabels its sends (security.md entry 16); real channels must be **daemon
  bridges** (their own package token) so the record's sender is honest.

## Open questions (for when this becomes a real journey)

- How are per-channel **effectiveness signals** captured and decayed (ack
  latency, read receipts, explicit preference, quiet hours)?
- Is escalation policy **declarative** (a rules table) or **delegated to a model**
  — and where's the audit boundary (Tim's "safety = the change log" stance)?
- Multi-human: each human has their own phonebook identity + EA policy; how does a
  fleet share the directory but not the policy?
- Does the EA broker **private actor↔actor** channels (its "set up private comms"
  role), and what authority does that require beyond ⊆-delegation?

## Read these first
- [../actors.md](../actors.md) — "Reaching an actor: channels, and which way they
  flow" (the whole model; ingress/egress asymmetry, the egress exemplars).
- [../identity.md](../identity.md) — identity/channel/name; the phonebook; recall;
  egress; the human-proxy packages; the owner-not-in-phonebook gap.
- [../topics.md](../topics.md) — `in/human`, `in/dm/<kind>/<addr>`, `in/group`,
  the category set; channel-faithful addressing.
- [07-chatting.md](07-chatting.md) — the chat/companion seat the EA ultimately
  serves; [../handoffs/chat-rendering.md](../handoffs/chat-rendering.md) split
  this out.

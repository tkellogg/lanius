# Channels and reaching a person

A person is one identity reachable many ways — the web UI, Signal, Telegram,
email. Lanius treats each way as a **channel**, and a bridge to an outside
messaging app is **just a package that follows the ordinary topic protocol.**
There is no separate "transport" layer to learn. Full design:
[docs/channels.md](../../../../docs/channels.md).

## The one mental model

- **Inbound** (a message arrives from outside): the bridge daemon publishes it
  once to a channel-faithful ingress topic carrying the platform and the wire
  address, e.g. `in/dm/telegram/<chat-id>`. It is ledger-backed — that mailbox is
  the single durable copy.
- **Outbound** (reach a person on a platform): publish a command to the bridge's
  own inbox, `in/package/<bridge>/send {recipient, text}`. The bridge delivers
  directly and emits a receipt `obs/channel/<kind>/sent`. There is **no `out/`
  plane** and no "send to Tim, system picks the platform" wire primitive — which
  channel to use is a policy decision (the EA / human-proxy actor), not a
  transport.
- **Who is this?** The wire address (`telegram/<chat-id>`) is mapped to an
  identity (`tim`) by the **phonebook** at read time, via the **recall** stage —
  never frozen into the topic, so a later correction re-unifies all history.

## Rules that keep it honest

- A channel bridge **must be a daemon** (it carries its own package token), or the
  broker mislabels every send as the owner. Copy `packages/webhook` (egress
  template) and `packages/discord` (ingress template).
- Recall resolves the correspondent **only** from the broker-verified ingress
  topic, never a payload field or a self-emitted event — otherwise a
  prompt-injected agent could name a correspondent and pull another person's
  messages. Reserve the `in/dm/` prefix to bridges so an agent can't forge one.
- Register the `owner` in the phonebook at setup, or recall won't unify Tim's own
  channels.

## Why this is the pattern, not a special case

Adding a new channel should edit **zero kernel files** — it is a package + a
grant (`subscribe` the ingress plane; an inbox for egress) + phonebook rows. If
you ever find yourself hard-coding a channel's name into core code (as
`web.rs` `source_for` / `is_worker_session` still do), that is the anti-pattern:
capability comes from an installed package and a grant, never a baked-in string.
The web UI is itself only a *relay* of the `in/human/<owner>` plane — an agent
DMing you in the browser and on Telegram are the same publish, different renderer.

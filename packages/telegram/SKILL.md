---
name: telegram
description: Telegram bridge (daemon) — getUpdates messages become in/dm/telegram/<chat.id> conversation ingress; in/package/telegram/send delivers replies with sender=telegram receipts. A transport is just a package (zero kernel edits).
---

# telegram

A two-way external-channel **daemon** — the payoff of the "a transport is just a
package" design (docs/channels.md). It edits **zero kernel files**: it rides the
built-in `in/dm/<kind>/<addr>` grammar (Handoff B) and the already-built
`phonebook` + `recall` packages to become a live Telegram relay.

Setup: `TELEGRAM_TOKEN` in the daemon's environment (a BotFather token), copy
this package onto the package path, `lanius approve telegram`. For CI/offline
work, point `TELEGRAM_API_BASE` at a stub/recorded Bot API — no live token
needed.

Flow:

- **Ingress:** the daemon long-polls `getUpdates` holding the bot token; each
  message → **one** `in/dm/telegram/<chat.id>` publish (the canonical,
  recall-keyable conversation address; the chat id *is* the address, 1:1 and
  group alike). Any handler subscribing `in/dm/#` (or `in/dm/telegram/#`)
  receives it. The `in/dm/telegram/#` publish grant IS the ingress-bridge
  capability the broker ACL requires — only an owner-approved manifest may hold
  a dm-scoped grant, so a prompt-injected agent cannot forge ingress. On each
  inbound the bridge also records a phonebook channel **sighting**
  (`in/package/phonebook/channel {channel_kind:"telegram", address:<chat.id>}`),
  provenance = the bridge's broker-verified sender.
- **Egress:** an agent commands a send by publishing
  `in/package/telegram/send {recipient, text}`; the bridge calls the Bot API
  `sendMessage` directly (off the bus) and publishes an
  `obs/channel/telegram/sent` receipt **stamped `sender = telegram`** (the
  daemon authenticates as itself — docs/security.md entry 16).

It is a **daemon, not an exec handler** (entry 16): only a caged, token-authed
daemon gets `sender = telegram` on its records; an uncaged exec handler would
authenticate as the owner and mislabel every send. Unconfigured, it parks
instead of crash-looping.

Unification (the "load-test the phonebook" payoff): once the owner is seeded and
the Telegram chat linked to identity `owner`, `recall` resolves an inbound
Telegram message to `owner` and pulls his cross-channel history (elanus +
telegram) into the frame — the same person, seamless across platforms.

---
name: discord
description: Discord ingress bridge (UNTESTED scaffold) — gateway messages become in/dm/discord/<channel-id> conversation ingress; in/package/discord/send posts replies with delivery receipts.
---

# discord

**Status: untested scaffold.** Written without a live token; the structure
follows packages/linemux (the tested template) but the gateway handling has
not seen a real connection. Treat the first live run as a debugging session.

Setup: `DISCORD_TOKEN` in the daemon's environment, `pip install websockets`,
copy this package onto the package path, `lanius approve discord`.

Flow: gateway `MESSAGE_CREATE` → `in/dm/discord/<channel-id>` (ledger,
published once — the canonical conversation address recall keys on; the
channel id is the address, 1:1 and group alike, Handoff B). Any handler
subscribing `in/dm/#` (or `in/dm/discord/#`) receives it. The `in/dm/discord/#`
publish grant is the ingress-bridge capability the broker ACL requires — only
an owner-approved manifest may hold a dm-scoped grant. Outbound: emit
`in/package/discord/send` with `{channel_id, content}`; the bridge posts it
and publishes an `obs/channel/discord/sent` receipt. Unconfigured, the actor
parks instead of crash-looping; the supervisor still shows it alive in
`obs/package/discord/status`.

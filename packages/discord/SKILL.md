---
name: discord
description: Discord ingress bridge (UNTESTED scaffold) — gateway messages become harness work; in/package/discord/send posts replies with delivery receipts.
---

# discord

**Status: untested scaffold.** Written without a live token; the structure
follows packages/linemux (the tested template) but the gateway handling has
not seen a real connection. Treat the first live run as a debugging session.

Setup: `DISCORD_TOKEN` in the daemon's environment, `pip install websockets`,
copy this package onto the package path, `lanius approve discord`.

Flow: gateway `MESSAGE_CREATE` → `in/package/discord/triage` (ledger,
published once, addressed to a triage handler — see packages/triage-demo;
observation of arrivals comes from the delivery echo). Outbound: emit
`in/package/discord/send` with `{channel_id, content}`; the bridge posts it
and publishes an `obs/channel/discord/sent` receipt. Unconfigured, the actor
parks instead of crash-looping; the supervisor still shows it alive in
`obs/package/discord/status`.

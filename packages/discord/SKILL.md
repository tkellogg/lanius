---
name: discord
description: Discord ingress bridge (UNTESTED scaffold) — gateway messages become harness work; work/discord/send posts replies with delivery receipts.
---

# discord

**Status: untested scaffold.** Written without a live token; the structure
follows packages/linemux (the tested template) but the gateway handling has
not seen a real connection. Treat the first live run as a debugging session.

Setup: `DISCORD_TOKEN` in the daemon's environment, `pip install websockets`,
copy this package onto the package path, `elanus approve discord`.

Flow: gateway `MESSAGE_CREATE` → `ingress/discord/message` (observation) +
`work/discord/triage` (ledger, write a triage handler — see
packages/triage-demo). Outbound: emit `work/discord/send` with
`{channel_id, content}`; the bridge posts it and publishes a
`delivery/discord/sent` receipt. Unconfigured, the actor parks instead of
crash-looping; the supervisor still shows it alive in
`obs/skill/discord/status`.

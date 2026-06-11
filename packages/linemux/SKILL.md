---
name: linemux
description: Local line-file ingress bridge — drop text into its inbox and it becomes harness work; the testable template for real ingress adapters.
---

# linemux

Drop a file ending in `.line` into this package's scratch inbox
(`<root>/run/pkg-linemux/inbox/`) and its contents become an
`in/package/linemux/triage` event on the ledger — published once, addressed
to its handler; observation of the arrival comes from the delivery echo. The
`triage-demo` package shows what to do with the work side.

This package exists to prove the ingress-adapter pattern end to end with no
external dependencies: a supervised daemon actor, token-authenticated bus
publishes, crash-only restarts. Copy it when writing a real bridge
(see `packages/discord`).

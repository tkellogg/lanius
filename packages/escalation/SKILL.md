---
name: escalation
description: Re-pings unanswered human asks (capped) until answered, expired, or a delivery receipt shows a human already saw it on some channel.
---

# escalation

A cron sweep (no resident process) that re-surfaces unanswered `in/human/owner`
asks as `signal/attention` — which the notify package renders like any
other signal. Stops when: an answer arrives, the deadline default fires, an
`obs/channel/<channel>/acked` receipt names the ask (read receipts from
channels that have them; desktop only produces `sent`), or the nag cap (3)
is hit. Nag counts live in the kv table; causality threads each nag to its
ask via `cause_id`.

Tuning: the cron's `payload = { after_secs = N }` sets how stale an ask must
be before the first nag.

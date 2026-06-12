---
name: history
description: The first reconstruction view — answers read-only history queries (agents, sessions, transcripts, conversations) over the bus from the sqlite truth.
---

# history

The ledger and transcripts in `harness.db` are the truth; the flight
recorder is a WAL, not a timeline. Anything that *reconstructs* reality from
them — per-agent traces, conversation views, search — is userland
(docs/bus.md, decided 2026-06-11). This package is the first such view: a
supervised daemon that answers history queries over the bus, reading the
database strictly read-only (`mode=ro` URI — a view physically cannot write
the truth it reconstructs).

Queries ride the **obs plane** so a UI poking at history never becomes a
ledger event:

```
you  -> obs/ui/history/q          {"kind":"transcript","qid":"q1","session":"s-abc"}
it   -> obs/ui/history/r/q1       {"qid":"q1","ok":true,"session":"s-abc","messages":[...],"has_more":false}
```

The `qid` is minted by the requester and echoed back as the last response
topic segment — subscribe `obs/ui/history/r/<your qid>` before publishing
the query. Kinds:

| kind | args | answer |
|---|---|---|
| `agents` | — | agent nouns seen in `in/agent/…` events, each with its transcript sessions; plus sessions with no agent linkage |
| `sessions` | `agent?` | sessions with first/last timestamp, message and event counts |
| `transcript` | `session`, `limit?` (≤200), `before_id?` | messages page (role, content JSON, created_at), `has_more`; page backwards with `before_id` |
| `conversation` | `correlation` | events sharing that correlation, in ledger order |

Errors come back on the same response topic: `{"qid","ok":false,"error"}`.
Oversized message content is replaced by `{"truncated":true,"chars","preview"}`
so a response stays one sane MQTT publish.

ui/web's explorer uses this for its sessions/transcript views and degrades
to live-only when this package isn't installed or approved.

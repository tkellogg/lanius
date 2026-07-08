---
name: comms
description: The chat/conversation view — owns the chat protocol on the bus and answers the chat-shaped conversation-list + introspection queries over local HTTP from the sqlite truth. Requires the history package.
---

# comms

One package owns the conversation concern end to end: **(1) the chat protocol on
the bus** — the topic conventions that say what a conversation *is*, on the wire —
and **(2) a read API** over the ledger for the chat-shaped conversation list and
per-conversation introspection. It is the second reconstruction view after
`history` (docs/bus.md, decided: *reconstruction views are USERLAND subscribers,
never kernel*). Like `history`, it holds **no state of its own** — every query is a
fresh read-only connection to `lanius.db` (`mode=ro` — a view physically cannot
write the truth it reconstructs). The kernel already serializes every chat message
to disk (`in/# → ledger` is the single durable copy, docs/topics.md); this package
supplies the chat *shape* and the *API*, which is the part that was missing.

It `requires = ["history"]`: comms answers the chat-shaped questions
(conversation list, introspection); history answers transcript/search. Two
granular, independently-approvable capabilities composed by a **declared
dependency edge**, not one merged daemon. A profile that puts comms on its path
must also carry an approved history.

## (1) The chat protocol — what a conversation is, on the bus

This is the read-side companion to `docs/channels.md` ("a transport is just a
package"). The conventions the projection reads, in plain language:

- **A conversation is threaded by its locator, not a room.** The locator rides the
  topic: `in/agent/<noun>/<conv>` (docs/topics.md, *"Conversations get their own
  identity"*). The recipient's mailbox is the durable copy; a conversation is the
  set of events sharing a locator/correlation, reconstructed on read.
- **External conversations are addressed by the `dm` grammar:**
  `in/dm/<kind>/<addr>` — e.g. `in/dm/telegram/<chat-id>` (Handoff B,
  `docs/handoffs/dm-channel-grammar.md`; `in`/`dm` are reserved). A bridge package
  (e.g. `packages/telegram`) maps a channel's addresses onto this grammar.
- **`source` is a stamped fact, not spelling.** The channel a message came in on is
  `payload.source`, stamped **at the source** by the channel/package (the Telegram
  bridge and the send/ask reply path do this — Handoff C,
  `docs/handoffs/agent-dm-relay.md` M3, via `exec::reply_source`). The projection
  prefers `payload.source`; a shrinking set of legacy spelling guesses
  (`web-`/github/jira/linear/cron) remains **only** until those sources become
  packages that stamp their own — adding a new channel must **not** add a branch.
- **Provenance is the broker-verified `sender`, never a payload field.** Who is in a
  conversation is decided by the ledger's `sender` column the kernel stamped, not a
  `sender`/`source` an agent could forge (the same trust rule recall and the
  phonebook hold).
- **Receipts land on `obs/channel/<kind>/{sent,acked}`** — the delivery side of a
  channel, observable but out of this read view's scope.

## Finding the endpoint

The harness assigns the port; read it from the run dir (never guess, never trust a
bus message for this):

```sh
PORT=$(python3 -c 'import json,os;print(json.load(open((os.environ.get("LANIUS_ROOT") or os.environ["HARNESS_ROOT"])+"/run/pkg-comms/http.json"))["port"])')
curl -s "http://127.0.0.1:$PORT/healthz"   # {"ok": true, "kinds": [...]}
```

A connection refused means the package is parked (serving is a granted capability —
`lanius approve comms`) or not running. The web dashboard's `/api/conversations`
relays here; when comms is parked the dashboard degrades to "approve the package".

## (2) Queries — `POST /query`, one JSON body, dispatched on `kind`

| kind | args | answer |
|---|---|---|
| `conversations` | `agent`, `owner?` | the chat-shaped conversation list for an agent: one row per thread — `{session, agent, title, source, last_ts, message_count, preview, last_role, branched_from}`, newest first (worker/coding sessions evicted, ambient/agent-first threads seeded, correlated replies folded). Byte-compatible with the web comms list. |
| `conversation_info` | `session` (or `conv`), `owner?` | introspection for one conversation: `{session, participants, source, channels, message_count, turn_count, event_count, first_ts, last_ts, branched_from, correlations}`. `participants` are broker-verified senders only. |

```sh
curl -s "http://127.0.0.1:$PORT/query" -d '{"kind":"conversations","agent":"main"}'
curl -s "http://127.0.0.1:$PORT/query" -d '{"kind":"conversation_info","session":"web-9"}'
```

`owner` defaults to `$LANIUS_OWNER` (else `owner`); the web relay passes the real
owner so the owner's own messages label as `you`. The `conversations` projection
lived hard-coded in the core web server (`src/web.rs`) until
`docs/handoffs/comms-package.md` M1/M2 relocated it here, so adding a channel is a
package edit, not a kernel edit.

## See also

- `docs/channels.md` — "a transport is just a package"; the closing section named
  the kernel hard-codes this package removed.
- `docs/handoffs/dm-channel-grammar.md` (Handoff B) — the `in/dm/<kind>/<addr>`
  grammar this protocol documents.
- `docs/handoffs/agent-dm-relay.md` (Handoff C) — the `payload.source` stamp seam
  the projection reads.
- the `history` package — the transcript/search half of the reconstruction view,
  which this package `requires`.

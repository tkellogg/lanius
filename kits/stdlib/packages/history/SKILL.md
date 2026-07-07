---
name: history
description: The reconstruction view — answers read-only history queries (agents, sessions, transcripts, conversations, search with filter/projection/pagination) over local HTTP from the sqlite truth.
---

# history

The ledger and transcripts in `lanius.db` are the truth; the flight
recorder is a WAL, not a timeline. Anything that *reconstructs* reality from
them — per-agent traces, conversation views, search — is userland
(docs/bus.md, decided 2026-06-11). This package is the first such view: a
supervised daemon that answers history queries over HTTP, reading the
database strictly read-only (`mode=ro` URI — a view physically cannot write
the truth it reconstructs).

## Finding the endpoint

The harness assigns the port; read it from the run dir (never guess, never
trust a bus message for this):

```sh
PORT=$(python3 -c 'import json,os;print(json.load(open((os.environ.get("LANIUS_ROOT") or os.environ["HARNESS_ROOT"])+"/run/pkg-history/http.json"))["port"])')
curl -s "http://127.0.0.1:$PORT/healthz"   # {"ok": true, "kinds": [...]}
```

A connection refused means the package is parked (serving is a granted
capability — `lanius approve history`) or not running.

## Queries — `POST /query`, one JSON body, dispatched on `kind`

```sh
curl -s "http://127.0.0.1:$PORT/query" -d '{"kind":"sessions","agent":"main"}'
```

| kind | args | answer |
|---|---|---|
| `agents` | — | agent nouns seen in `in/agent/…` events, each with its transcript sessions; plus sessions with no agent linkage |
| `sessions` | `agent?` | sessions with first/last timestamp, message and event counts |
| `transcript` | `session`, `limit?` (≤200), `before_id?` | messages page (role, content JSON, created_at), `has_more`; page backwards with `before_id` |
| `conversation` | `correlation` | events sharing that correlation, in ledger order |
| `search` | `filter?`, `select?`, `page?` | the query DSL below |

## The search DSL — filter × projection × pagination

```json
{ "kind": "search",
  "filter": { "channels": ["in/agent/main"],        // event-type prefixes
              "involvement": "agent",                // or "all" (default)
              "roles": ["tool"],                     // user|assistant|tool
              "text": "reactor pressure",            // substring in content
              "since": "2026-06-12T00:00:00Z",       // ISO bounds on created_at
              "until": "2026-06-13T00:00:00Z" },
  "select": { "tool_calls": true,                    // false drops assistant tool_calls
              "reasoning": true,                     // false strips recorded reasoning
              "tool_results": { "truncate": 500 } }, // or true / false (drop rows)
  "page":   { "limit": 100, "cursor": "12345" } }    // cursor = the returned cursor
```

Reasoning note: transcripts record whatever reasoning the provider
returned; the real-vs-summary distinction needs provider metadata the
transcript doesn't carry yet, so `reasoning` is keep/strip, not a
three-way filter.

Answers `{"messages": [...], "has_more": bool, "cursor": <id|null>}` —
newest first; pass `cursor` back to page deeper. The DSL is interpreted
server-side (values bind as SQL parameters, never concatenate). Oversized
message content is replaced by `{"truncated":true,"chars","preview"}`.

Typical agent moves: "what did I tell the human about X" →
`{"kind":"search","filter":{"text":"X","roles":["user","assistant"]}}`;
"replay a session without tool noise" →
`{"kind":"search","filter":{"channels":["in/agent/main"]},"select":{"tool_results":{"truncate":200}}}`
or `transcript` for one session in order.

ui/web's explorer proxies these same queries (`/api/history`) and degrades
to live-only when this package isn't running or approved.

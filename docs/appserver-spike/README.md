# codex app-server driver — spike record (self-verifying)

This directory is the tree-local, self-verifying evidence for the codex
app-server transport (`run_codex_app_server_capture`,
[`../handoffs/codex-app-server.md`](../handoffs/codex-app-server.md)). It holds
the M1 wire-probe scripts + transcript, and the obs trails from the **live
end-to-end composition run** (the one leg the driver's unit tests could not
cover).

## What the live run proved

A single headless codex worker was driven through the app-server transport, via
a real daemon/bus/ledger stack, on a throwaway root (`ELANUS_ROOT` under a temp
dir; never `~/.elanus`). Installed `codex-cli 0.142.5`, real `~/.codex` login
(only `auth.json`/`version.json` symlinked into a scratch `CODEX_HOME`, never
copied). A scratch stdio MCP server (`m1-probes/scratch-mcp.mjs`, one tool
`scratch_ping`) was registered for codex via `CODEX_HOME/config.toml`
`[mcp_servers.scratch_ping]`. The worker was launched with
`elanus code codex --headless --app-server "<prompt to call scratch_ping>"`.

Both branches were exercised end-to-end:

- **ALLOW** (`e2e-allow-obs-trail.jsonl`, session `code-ec0491b6`): the MCP tool
  call PAUSED the turn; an elicitation ask landed on the owner mailbox
  (`in/human/owner`, `elanus inbox`); `elanus answer <id> allow` was replied;
  the driver mapped it to `{action:"accept"}` on the RPC; `scratch_ping` ran and
  returned `pong: hello`; the turn completed and the summary routed back.
- **DENY** (`e2e-deny-obs-trail.jsonl`, session `code-ca63eb85`): same up to the
  ask; `elanus answer <id> deny` → driver replied `{action:"decline"}`; codex
  reported `user rejected MCP tool call`; the turn still completed (exit 0),
  the tool never ran.

In both, `session/start` stamps the **elicited** posture
(`"approvals":"elicited","sandbox":"workspace-write"`, cage `egress:https-only`,
`enforced:true`) — NOT `danger-full-access`. Every projected record is
fidelity-stamped `app-server-live`. The ask, answer and decision are all
reconstructable on one correlation id.

## Files

- `e2e-allow-obs-trail.jsonl` / `e2e-deny-obs-trail.jsonl` — the obs leaves
  (`obs/agent/codex/<session>/...`) emitted by the live driver, extracted from
  the run's `trace.jsonl`. Contains the `session/start` posture, the
  `tool/scratch_ping/call`+`result`, the `approval/ask`+`answer`+`decision`
  triple, and `session/idle`+`stop`.
- `m1-probes/` — the M1 wire-pinning probes (Python JSON-RPC clients) + the
  scratch MCP servers they drove, and `transcript-mcp.jsonl`, the captured
  `mcpServer/elicitation/request` round-trip against a running `codex
  app-server` (the entry-24 scenario). See the handoff Log's M1 entry for the
  four unknowns these pinned.

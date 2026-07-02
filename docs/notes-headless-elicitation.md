---
status: research
last-updated: 2026-07-02
---

# Headless elicitation across harnesses

Question: can a headless coding-agent worker (Claude Code, Codex, opencode) pause
mid-run, ask a human/mailbox a real question, and resume on the answer — the
shape elanus's `ask_human`/mailbox rail wants? Or is auto-approve-everything the
only honest option per harness?

## 1. Per-harness capability table

| harness | headless elicitation possible? | mechanism | async-tolerant (minutes+)? | effort to wire to elanus's `ask_human`/mailbox |
|---|---|---|---|---|
| **Claude Code** | Partial | `--permission-prompt-tool`: routes any decision not resolved by static allow/deny rules to an MCP tool elanus supplies, which returns allow/deny. Headless-first design (CI/batch), works with CLI auth. | **No.** The call blocks Claude's execution until the MCP tool returns; long human-in-the-loop waits hit a timeout. No documented suspend/resume of the underlying `claude -p` process. | Medium. elanus would implement the MCP tool side (schema is *not* authoritatively documented — only tool name/input reverse-engineered from community sources) and would have to poll/block inside that tool call for up to the timeout window — i.e. it can carry a *fast* ask (seconds) to a mailbox, not a slow one. `PreToolUse` hooks returning `ask` do **not** help: headless there's no TTY, so `ask` degrades to `deny`/undefined, not a real channel. Hooks *can* stall (poll a ledger row) up to their timeout (default 600s, per-hook configurable, no documented hard ceiling), but on timeout they're treated as not-run at all and fall through to the normal (headless-hostile) permission system — so a hook is a delay valve, not a resolver. |
| **Codex** | No, for `codex exec` (what elanus uses today) | `codex exec` has no approval callback at all: no `-a/--ask-for-approval` flag (that's TUI-only), and when a gated action needs a decision, `exec`'s stdin is either the prompt or EOF — the approval reader hits EOF immediately and codex treats it as decline, auto-cancelling the call. Filed bug: openai/codex #24135. Only working bypass is `--dangerously-bypass-approvals-and-sandbox`. | N/A (mechanism doesn't exist in `exec`) | High for `exec` — not available; would require **switching transport**, not adding a flag. |
| **Codex (app-server)** | Yes, architecturally | `codex app-server`/`codex mcp-server`: a long-lived, bidirectional JSON-RPC 2.0 session (stdio or `--listen ws://`/`unix://`). Server sends `execCommandApproval`/`applyPatchApproval` (or newer `item/*/requestApproval`) *requests* that block on the client replying `{decision: allow\|deny}` — a synchronous request/response elanus's wrapper could intercept, relay out-of-band to a mailbox/human, then answer. | **Yes**, in principle — the RPC session stays open while a turn is in flight; nothing in the protocol times out the approval request itself (server-side turn may still have its own limits, unconfirmed). This is the one mechanism in the whole survey that's actually built for pause/resume. | High. This is a different codex invocation entirely from what elanus currently runs (`codeagent.rs` uses `exec`), so it means building a new codex driver: hold the RPC session, watch for approval-request notifications, route to mailbox, answer. Real work, but the shape matches elanus's existing ask/resume rails better than any other mechanism here. |
| **opencode** | No, as currently used by elanus | elanus's own code (`src/codeagent.rs`, `run_opencode_capture`, confirmed in-repo) already launches opencode headless with `--dangerously-skip-permissions` — full auto-approve, chosen deliberately because "a worker can't answer interactive prompts." opencode's own docs describe only local interactive prompts or `--auto` (converts `ask`→`allow`); no documented remote-approval event/SSE surface. A hardcoded `external_directory: * → ask` can't even be overridden, and the maintainers closed the headless-auto-approve feature request (#20864) as not-planned with no human-in-the-loop alternative offered. | N/A | High and currently not worth it — the one candidate mechanism (plugin hook `permission.ask`) is unreliable upstream: reported as never firing (#7006), then reported as bypassed for exactly the first-encounter case that most needs a human (#19927, `if (!needsAsk)` gate). elanus already has a live SSE channel into a running opencode session (`run_opencode_tui_server_events`, `GET /event`) but it's wired for observability capture only, not for answering an `ask` — and the headless path elanus actually runs bypasses the ask entirely, so there's nothing to answer today. |
| **MCP elicitation (protocol-level, cross-harness)** | Not applicable to this problem | Elicitation is a *server*-initiated request nested in a `tools/call`, for the server to collect missing info from the user (form-mode / url-mode). It is explicitly not a tool-approval/permission mechanism by protocol design — client-side "should this tool call run" consent is a separate, client-owned concern outside the elicitation spec. A server *can* repurpose elicitation as a yes/no gate for its own destructive operations (Codex docs mention this convention), but that only covers approvals the server chose to gate this way — it doesn't give elanus a generic hook for "should this tool call, chosen by the harness's own permission system, run." | N/A | Not the right tool for this job — do not build against it as a permission-approval channel. |

## 2. Recommendation vs. Tim's rule (auto-approve-everything vs elicitation)

Tim's stated preference is elicitation over blanket auto-approve wherever it's
honestly possible. Per harness:

- **Claude Code**: elicitation is possible but only for *fast* decisions (the
  MCP-tool call blocks Claude's execution and has no suspend/resume — a slow
  mailbox round-trip will just time the run out). Recommendation: wire
  `--permission-prompt-tool` to elanus's mailbox for a bounded wait (seconds,
  maybe low tens of seconds), and treat "no answer within the timeout" as a
  **deny**, not a silent auto-approve — that keeps faith with fail-closed even
  though it's not true async elicitation. Don't promise minutes-scale human review
  through this channel; that's not what it is.

- **Codex**: with the `exec` transport elanus currently uses, elicitation is
  architecturally impossible — there is no channel to hold the decision open.
  Given that, `--dangerously-bypass-approvals-and-sandbox` (equivalently, running
  inside elanus's own sandbox/cage rather than relying on codex's own gating) is
  the only honest option **for `exec`-based workers**, scoped as narrowly as the
  sandbox policy allows (e.g. `workspace-write`, not `danger-full-access`) rather
  than blanket bypass. This is not a permanent answer — see §3.

- **opencode**: same conclusion as Codex-exec, for the same reason (no reachable
  channel in the transport elanus actually runs) — `--dangerously-skip-permissions`
  is the only honest option today, and elanus is already doing this deliberately.
  Do not attempt to build against `permission.ask` yet; it's unreliable upstream
  for exactly the case elanus needs (first-encounter/novel tool calls).

- **General principle for "elicitation impossible" cases**: auto-approve should
  be scoped as tightly as the harness's own sandbox/permission primitives allow
  (workspace-write not full-access, deny-list on destructive ops where offered),
  and the fact that the worker is running unattended should itself be visible in
  elanus's audit trail — consistent with "safety = audit, not restriction": if we
  can't gate it, we should still be recording that it ran ungated.

## 3. Concrete next step for the codex-MCP incident

The actionable path is **not** a flag change to `codex exec` — none exists. It's
a transport swap: build a new codex driver in elanus that speaks to
`codex app-server` (or `codex mcp-server`) over JSON-RPC instead of shelling out
to `codex exec`. That driver would:

1. Start `codex app-server` (stdio, co-located with the worker — no need for
   `--listen` remote transport initially).
2. Open a session via `thread/start` + `turn/start`.
3. Watch the RPC stream for `execCommandApproval`/`applyPatchApproval` (or
   `item/*/requestApproval`) requests.
4. On receipt, relay the request to elanus's mailbox/`ask_human` rail exactly
   like the Claude Code MCP-tool path, and reply `{decision: allow|deny}` when
   the answer comes back — with no documented timeout on the RPC request itself,
   this is the one path in this whole survey that can honor minutes-scale human
   review, not just a bounded poll.

This is real driver work (new process management, new event loop, new mapping
into elanus's existing tool-call/obs grammar), but it's the only mechanism found
here that matches the pause/resume shape elanus already has for
Claude Code/`ask_human`, and it removes the need for
`--dangerously-bypass-approvals-and-sandbox` entirely for codex workers.

## 4. Unknowns needing a live spike

- **Claude Code `--permission-prompt-tool` payload schema**: not authoritatively
  documented anywhere found; only reverse-engineered fragments (tool name + tool
  input). Need to actually stand up an MCP tool and log what it receives before
  building elanus's side.
- **`PreToolUse` hook timeout ceiling**: default 600s, configurable per-hook, but
  no documented hard maximum — unclear how far this can be pushed before Claude
  Code itself imposes a cap, or whether pushing it far breaks other assumptions.
- **`codex app-server` approval-request timeout**: the RPC request appears to
  block indefinitely on the client's reply, but this wasn't verified against a
  running server — need to confirm there's no server-side or turn-level timeout
  that would cap "async" in practice.
- **`codex app-server` stability/version surface**: docs reference both legacy
  (`execCommandApproval`) and newer (`item/*/requestApproval`) wire names for the
  same concept; need to pin which the installed codex version actually emits
  before writing a driver against it.
- **opencode `permission.ask` current state**: both filed issues (#7006 "never
  fires", #19927 "fires but skipped on first encounter") may have been fixed
  since filing — worth a quick spike against the current opencode version before
  writing this off long-term, even though it's not worth building against today.

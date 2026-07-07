---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: codex done right — the app-server transport, approval-elicited

Decomposed from [../notes-headless-elicitation.md](../notes-headless-elicitation.md)
§3 + §4 and [../security.md](../security.md) entry 24. The transport swap: a new
codex driver that speaks **`codex app-server` JSON-RPC** instead of shelling out
to `codex exec` for **headless** codex workers. The payoff: a headless codex
worker runs **approval-elicited** — real pause/ask/resume onto elanus's
mailbox/`ask_human` rail — with **no sandbox bypass**, retiring entry 24's
`danger-full-access` posture wherever the driver is active. `codex exec` stays as
a flag-gated fallback. This is the hardest handoff in the sprint; the design is
honest that **much stays unknown until M1's live spike**, and M1 ships **no fix
code**.

## The one honest tension, up front (read before the milestones)

elanus's `ask_human`/resume rail is **exit-and-resume**: the daemon exec handler
calls `ask_human`, emits the question to the owner's mailbox with a fresh
correlation + deadline + `default_action`, checkpoints the parked call
(`kv_set(pending_ask_key)`), and **returns `ToolOutcome::Suspend` — the handler
process exits (75)** (`src/exec.rs:2141-2206`). The dispatcher records the
suspension and, each tick, `resume_suspended` (`src/dispatcher.rs:922-962`) finds
a suspended dispatch whose `resume_correlation` now has a matching answer event
and **re-spawns the handler** with the answer stapled under `envelope["resume"]`.

The app-server driver **cannot exit-and-resume**: it holds a **live JSON-RPC
socket** to a running `codex app-server` with an in-flight turn. Exiting drops
the session and the turn. So the driver must **reuse the ask *emit* shape**
(owner mailbox + correlation + deadline + `default_action`) but then **wait
in-process** for the answer event on that correlation, replying `{decision}` on
the RPC when it arrives — the same rail's *data model*, a different *consumption*
of it. This is a genuine divergence from how every other suspend works today, and
M1 must confirm whether a cleaner path (thread reattach across a process restart)
exists. **Do not paper over this** — see wonky bit 2. It is flagged to Fable in
the final message as a place the settled framing ("relay onto the ask_human rail")
proved not literally implementable via the exit/resume path.

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **M1 is a pure spike — no driver code until §4's unknowns are pinned.**
   [../notes-headless-elicitation.md](../notes-headless-elicitation.md) §4 lists
   the load-bearing unknowns, all against the **installed** codex (0.142.5, per
   [mcp-on-launch.md](mcp-on-launch.md) Log): (a) which approval wire name this
   version emits — legacy `execCommandApproval`/`applyPatchApproval` vs newer
   `item/*/requestApproval`; (b) whether the approval request truly blocks
   unboundedly (no server/turn-level timeout); (c) the thread/turn lifecycle,
   **including whether a thread can be reattached after a client reconnect
   mid-turn/mid-approval** (this decides wonky bit 2). No driver is written until
   these are captured against a running server.

2. **In-process blocking wait vs thread-reattach (the tension above).** If M1
   finds app-server supports **detach/reattach of a thread mid-approval**, the
   driver *could* use the true exit/resume rail (exit on an approval, resume
   re-attaches and answers) — cheap, no held thread. If it does **not**, the
   driver must **hold the socket and block in-process** waiting for the mailbox
   answer, which ties up one OS thread (and one live codex process) per suspended
   worker — a real scaling cost at many concurrent elicited workers. **My call:
   design for the in-process blocking wait (the safe assumption), and switch to
   reattach only if M1 proves it works.** *Fable: confirm the blocking-wait
   fallback is acceptable and size the held-thread cost.*

3. **Default-on-timeout: configurable, default deny (fail-closed).** The RPC
   approval request appears to block with no server timeout, so elanus imposes
   its **own** deadline via the ask's `deadline` + `default_action` (exactly as
   `ask_human` does, `src/exec.rs:2164-2182`). On no answer by the deadline, the
   driver replies with the configured default. **My call: configurable, default
   `deny`** (matches the safety doctrine — an unattended non-answer must not
   auto-approve). *Confirm.*

4. **Transport selection: flag-gated, exec stays the fallback.** A launch flag /
   config selects the app-server transport for headless codex; absent it, today's
   `run_codex_capture` (exec) path runs unchanged. Mirror single-cage's
   per-profile opt-in rollout gate ([single-cage-macos.md](single-cage-macos.md)
   wonky bit 1) rather than a flag day. **My call: opt-in per launch/profile;
   exec is the default until the driver soaks.** *Confirm the gate.*

5. **Obs parity: reuse the codex event vocabulary, honest fidelity stamp.** The
   exec path maps codex's `--json` stream (`thread.started`, `turn.started/
   completed/failed`, `item.started/completed/updated` with item types
   `command_execution`/`file_change`/`mcp_tool_call`/`web_search`) into the obs
   grammar via `codex_map_stream_event` / `codex_map_item`
   (`src/codeagent.rs:6451-6571`). The app-server driver must map its JSON-RPC
   **notifications** into the **same** obs leaves so obs stay uniform — but the
   wire names differ (M1 pins them), so mark every projected record with a new
   honest fidelity stamp (`fidelity: "app-server-live"`), mirroring the existing
   `rollout-import` / `server-events-live` stamps
   (`src/codeagent.rs:4730`, `:5878`).

## Milestones

### M1 — the live spike (NO fix code)
Against the installed codex (0.142.5): start `codex app-server` (stdio,
co-located — no `--listen` initially, per §3), open a session
(`thread/start` + `turn/start`), drive an action that requires approval, and
**capture**: the exact approval-request method name + request/response JSON
shapes this version emits; whether the request blocks unboundedly; the full
thread/turn lifecycle and **whether a thread reattaches after a client reconnect
mid-turn/mid-approval** (wonky bit 2); the notification event schema for the
turn/item stream (to map in M2). Record findings by appending to
[../notes-headless-elicitation.md](../notes-headless-elicitation.md) §4 (or this
handoff's Log) with a captured transcript.

**Acceptance:** a written spike record pinning (i) the approval wire name +
shapes, (ii) blocking behavior, (iii) thread/turn lifecycle + reattach
answer, (iv) the notification schema — each backed by a captured transcript from
a running `codex app-server`. **No driver code.**

### M2 — the driver skeleton + obs mapping (approvals auto-answered, recorded)
Add a new headless codex transport (`run_codex_app_server_capture` alongside
`run_codex_capture`, `src/codeagent.rs:4168`) that starts app-server, opens a
session, runs a turn, and maps its notification stream into the **existing** obs
grammar (reuse the `codex_map_*` vocabulary; stamp `fidelity: "app-server-live"`).
In this milestone approval requests are **auto-answered `{allow}` but recorded**,
so obs parity can be verified against the exec path before the relay lands.

**Acceptance:** a headless codex turn via the app-server driver produces obs
records under the **same** `obs/agent/<noun>/<session>/...` leaves as the exec
path (`thread.started`/`turn.*`/`item.*`), fidelity-stamped `app-server-live`; a
diff of the projected leaves against an equivalent exec run shows equivalent
structure; the turn completes and its legible result routes back like the exec
path's `CaptureSummary`.

### M3 — approval relay onto the ask/mailbox rail
On an approval-request notification, emit the ask to the owner's mailbox
(`crate::topic::human_mailbox(&prof.owner)`) with a fresh correlation + `deadline`
+ `default_action` (reuse the `ask_human` emit shape, `src/exec.rs:2172-2187`),
then wait for the answer event on that correlation and reply `{decision}` on the
RPC — **in-process** (wonky bit 2) since the socket must stay open — with the
configurable default-on-timeout (wonky bit 3). The whole exchange is in the obs
trail.

**Acceptance:** a gated action pauses the turn; an owner reply `{allow}` lets it
proceed and `{deny}` cancels it (codex reports the cancellation, mapped to obs);
no answer by the deadline applies the configured default (default `deny`); the
ask, the answer, and the decision are all reconstructable from the obs/ledger
trail on one correlation.

### M4 — retire the bypass where the driver is active
The flag/config (wonky bit 4) selects the app-server transport for headless
codex. When active: **no `danger-full-access` override is passed** — codex's own
approval posture is in force and elicited — and the `session/start` stamp reflects
the **elicited** posture (e.g. `approvals: "elicited"` + the actual codex sandbox
mode) instead of `approvals: "auto", sandbox: "danger-full-access"`
(`src/codeagent.rs:3508-3512`, `codex_headless_approval_posture` `:4159`). `codex
exec` stays as the flag-gated fallback; entry 24's ungated posture applies **only**
where exec runs. Update entry 24 to note the driver retires it where active.

**Acceptance:** with the driver flag on, a headless codex worker attempts an MCP
tool call that would have needed approval and it is **elicited** (routed to the
owner), not auto-approved; the `session/start` obs shows the elicited posture, not
`danger-full-access`; with the flag off, the exec path + its entry-24 posture are
byte-identical to today. `cargo test` green.

## Explicitly out of scope / honest residuals
- **Remote transport** (`--listen ws://`/`unix://`): co-located stdio only (§3).
- **`codex exec` removal**: it stays as the fallback; this handoff does not delete
  it.
- **Residual risk if M1 disappoints:** if the installed version's approval wire
  name differs from both documented forms, or there is a server/turn timeout that
  caps "async," or threads cannot reattach — any of these reshapes M2–M4. The
  handoff is deliberately staged so M1 can send the design back before code is
  written. The held-thread cost of the blocking-wait fallback (wonky bit 2) is a
  known scaling residual, not a bug.

## Read these first
- The research: [../notes-headless-elicitation.md](../notes-headless-elicitation.md)
  — **all of it**, especially the codex(app-server) row in §1, §3 (the concrete
  next step), and §4 (the exact unknowns M1 must pin).
- The ruling being retired: [../security.md](../security.md) entry 24 (headless
  codex auto-approve at `danger-full-access`, the LATENT residual naming this
  driver as the fix).
- The MCP-on-launch context: [mcp-on-launch.md](mcp-on-launch.md) Log (the codex
  residual, installed versions — codex 0.142.5).
- The exec transport to swap + its obs mapping: `src/codeagent.rs:4168-4296`
  (`run_codex_capture`), `:4090-4127` (the approval ruling + constants),
  `:4159-4165` (`codex_headless_approval_posture`), `:6271` (`capture_codex_stream`),
  `:6451-6571` (`codex_map_stream_event` / `codex_map_item` — the obs vocabulary
  to reuse), `:3490-3519` (the `session/start` posture stamp), the fidelity-stamp
  precedents `:4730` / `:5878`.
- The rail to relay onto: `src/exec.rs:2141-2206` (`ask_human` emit + suspend),
  `src/dispatcher.rs:922-962` (`resume_suspended` — why exit/resume does NOT fit a
  held socket), `src/codesession.rs:372-459` (delivery idempotency, if the driver
  ever routes through code deliveries).
- The rollout gate to mirror: [single-cage-macos.md](single-cage-macos.md) wonky
  bit 1 (per-profile opt-in, no flag day).

## Log
- 2026-07-07 — Confirmed shipped+merged on main (M1–M4 all landed, the full live
  e2e composed run also executed — see `docs/appserver-spike/README.md` +
  `e2e-allow-obs-trail.jsonl`/`e2e-deny-obs-trail.jsonl`, merged in `039d640`
  "s4-codex"; `docs/security.md` entry 24 updated to match); status flipped to
  `done` (was stale at `planned`). Tracked residuals (minor, non-blocking, not
  previously recorded outside the merge commit message): the DENY branch's obs
  decision record logs `granted:true` (cosmetic label bug — the wire behavior is
  correct, the tool call still declines); session-scoped coding packages lack the
  `obs/harness/ledger/emit` publish grant, producing benign denied-obs noise in
  the trail.
- 2026-07-02 — **TWO VERIFIER HIGHs CLOSED (fixer).**
  - **(HIGH 1) The cage no longer blocks codex's model API.** The loopback cage
    (`codex_headless_cage`) previously cut ALL egress including codex's own model
    API (chatgpt.com), so no headless codex turn (exec OR app-server) could
    complete under the cage on macOS — leaving M2/M3/M4's live legs unprovable
    against the delivered driver. FIX: added `CagePolicy.allow_https_egress`
    (`src/sandbox.rs`), a NARROW model-API egress hole that re-opens ONLY outbound
    TLS (`(allow network-outbound (remote tcp "*:443"))`) + the resolver unix
    socket DNS needs, on top of the loopback fence — every other port/protocol
    stays denied. `codex_headless_cage` sets it (the network floor codex requires,
    the analogue of security.md entry 24's sandbox floor); the `session/start`
    stamp is honest (`"egress": "https-only"`). **Live-verified** against
    `sandbox-exec` on macOS 26.5: `seatbelt_https_egress_allows_tls_blocks_other`
    (`src/sandbox.rs`) confirms a caged `curl https://chatgpt.com/` connects (HTTP
    403) while a caged `:80` connect is refused; plus SBPL-emission unit tests
    (`sbpl_https_egress_reopens_only_tls_over_loopback`,
    `sbpl_https_egress_noop_under_open`). So the network blocker the verifier named
    is REMOVED and proven. **Still outstanding (honest):** one FULL live-driver
    turn — `run_codex_app_server_capture` end-to-end against live codex through the
    daemon/bus stack — was NOT executed here; it is no longer cage-blocked, just
    not yet run. **Flagged to Fable:** the :443 egress hole cannot pin the model
    host by name (SBPL limitation), so a caged worker can reach any host on :443 —
    the accepted floor, recorded in security.md entry 24; confirm this posture (or
    the "state-machine-proven, live-driver-turn deferred" acceptance) before
    marking the handoff done.
  - **(HIGH 2) M4's MCP-elicitation gate now has end-to-end coverage.** The
    `mcpServer/elicitation/request` gate (the exact entry-24 scenario) was only
    unit-tested (classifier + reply), never driven through the loop. FIX: added
    `appserver_mcp_server_script` + `appserver_driver_end_to_end_mcp_elicitation_
    {accept,decline_and_timeout}` (`src/codeagent.rs`) — they drive an
    `mcpServer/elicitation/request` server-request through `drive_codex_app_server`
    and assert the relayed ask (stamped `kind: "mcp tool call"`, method
    `mcpServer/elicitation/request`, on the owner's mailbox with a deadline +
    fail-closed default), the v2 `{action:"accept"}` on owner-allow /
    `{action:"decline"}` on owner-deny AND on timeout (never an auto-approve),
    mirroring the commandExecution e2e tests. `cargo test --lib` green (482).
- 2026-07-02 — **M2–M4 DRIVER LANDED (implementer).** `run_codex_app_server_capture`
  (`src/codeagent.rs`) speaks `codex app-server` JSON-RPC, maps its notification
  stream into the existing obs grammar (`fidelity: "app-server-live"`), relays
  `item/*/requestApproval` + `mcpServer/elicitation/request` gates to the owner's
  mailbox with a fresh correlation + deadline + fail-closed `default_action`
  (default DENY on timeout), and stamps the elicited posture on `session/start`
  (`"approvals":"elicited","sandbox":"workspace-write"`, no `danger-full-access`).
  Flag/config-gated (`[codex] app_server` / `--app-server`, default OFF; `codex
  exec` stays the fallback). `cargo test` green (unit coverage: `appserver_*`
  tests — notification→obs mapping, mapper agreement with exec, lifecycle
  mapping, answer→v2-decision fail-closed, approval classifier + detail, summary
  harvest, flag override/strip).
  **VERIFICATION STATUS — do not overstate.** The approval/elicitation
  round-trip (allow → action runs, decline/timeout → declined, incl. the MCP
  gate) was verified against the wire by the **M1 stdio probe run WITHOUT the
  cage** (below). The delivered driver ALWAYS composes with `codex_headless_cage`
  (`NetworkPolicy::Loopback`), which on macOS blocks codex's own model API
  (chatgpt.com) — confirmed for both exec and app-server — so a live headless
  turn under the cage cannot reach the model and never completes or reaches an
  approval.
- 2026-07-02 — **DRIVER STATE MACHINE NOW PROVEN END-TO-END (fixer, no network).**
  The verifier flagged that M2/M3/M4's live-behavior acceptances (turn completes/
  routes back; a gated action pauses; allow→accept, deny→decline, timeout→default;
  the answer reaches the driver) had ZERO execution coverage — only the pure
  mapper/classifier/reply fns were tested, and the driver cannot run live under the
  cage on macOS. TWO fixes:
  - **Coverage.** The JSON-RPC loop was extracted into `drive_codex_app_server`
    (`src/codeagent.rs`) so it runs against ANY frame source/sink. New
    `appserver_driver_end_to_end_{allow_relays_accept,deny_relays_decline,
    timeout_defaults_deny,no_approval_completes}` tests drive it against an
    **in-process mock app-server** speaking the pinned JSON-RPC over a real
    temp-root ledger, asserting: the handshake→thread→turn/start drive; that a
    gated command approval PAUSES and relays to the owner's mailbox (with deadline
    + fail-closed default); allow→v2 `accept`, deny→`decline`, no-answer→default
    `decline`; the turn then completes and the `CaptureSummary` routes back.
  - **Routing seam (verifier's 2nd HIGH).** The old await polled ONLY the codex
    noun mailbox (`in/agent/codex`), so the dispatcher's expire-default (emitted to
    the canonical `in/agent/main`) and a web-UI reply that resolved to a different
    selected agent could NEVER reach the driver → the allow-path could silently
    default-deny. `codex_appserver_await_answer` now matches the answer by
    correlation across ANY topic (the fresh correlation is unique to the ask, and
    the ask event — `question`, no `answer` — is skipped). Covered by
    `appserver_await_answer_matches_reply_on_any_topic` (codex mailbox / canonical
    `in/agent/main` / owner `in/human/*` all resolve) +
    `appserver_extract_answer_unwraps_dispatcher_default`.
  `cargo test --lib` green (477). The ONLY residual is a LIVE codex turn under the
  cage (blocked by the loopback cage's model-API egress) — a codex-cage concern,
  NOT a state-machine gap. security.md entry 24's addendum updated to match.
  **Residual for the next step:** add a codex-API egress allowance to
  `codex_headless_cage` and run `run_codex_app_server_capture` against a LIVE codex
  once to also close the network-egress leg.
- 2026-07-02 — **M1 SPIKE COMPLETE (implementer). No fix code; all four §4
  unknowns pinned live against installed `codex-cli 0.142.5`** via a stdio
  JSON-RPC driver (Python probe, scratchpad; transcripts captured). The TS
  protocol was also dumped with `codex app-server generate-ts` to cross-check the
  wire types. Findings:

  **(i) Approval wire name + shapes.** This version emits the **v2** names, NOT
  the legacy `execCommandApproval`/`applyPatchApproval` — and it emits them
  **regardless of `capabilities.experimentalApi` (tested true AND false)**: legacy
  is dead in 0.142.5.
    - Shell command approval → server REQUEST `item/commandExecution/requestApproval`
      (params: `threadId,turnId,itemId,startedAtMs,environmentId,reason,command,
      cwd,commandActions,proposedExecpolicyAmendment,availableDecisions`). Reply
      `{decision: <CommandExecutionApprovalDecision>}` where decision ∈
      `"accept" | "acceptForSession" | {"acceptWithExecpolicyAmendment":…} |
      {"applyNetworkPolicyAmendment":…} | "decline" | "cancel"`.
    - File patch approval → `item/fileChange/requestApproval`; reply
      `{decision}` ∈ `"accept"|"acceptForSession"|"decline"|"cancel"`.
    - MCP tool-call approval (the entry-24 scenario, M4) → **NOT** the exec
      channel: it arrives as server REQUEST `mcpServer/elicitation/request`,
      `mode:"form"`, with `_meta.codex_approval_kind:"mcp_tool_call"` +
      `tool_params`/`message`. Reply `{action: "accept"|"decline"|"cancel",
      content, _meta}` (`McpServerElicitationAction`). Live-confirmed: a scratch
      stdio MCP server's tool ran only after `{action:"accept"}` (item
      `mcpToolCall` → `completed`, tool actually invoked); with no/ wrong answer
      the tool `failed`.
    - **FOOTGUN (record it):** replying with the *legacy* keyword `"approved"` to
      the v2 `item/commandExecution/requestApproval` is silently treated as
      **decline** (command item → `declined`, turn still `completed`). The driver
      MUST use the v2 keywords; a wrong keyword fails closed, not open.

  **(ii) Blocking behavior.** The approval request blocks **unboundedly** — held
  a pending `item/commandExecution/requestApproval` for **70 s** with zero
  server-side timeout/turn-failure, and a late `{decision:"accept"}` after 70 s
  still completed the turn. Confirms the design: codex imposes NO deadline;
  elanus must impose its own (wonky bit 3).

  **(iii) Thread/turn lifecycle + reattach.** Lifecycle:
  `initialize` → (recv init result) → `initialized` notif → `thread/start`
  {approvalPolicy,sandbox,cwd} → notif `thread/started`{thread.id} →
  `turn/start`{threadId,input:[{type:"text",text,text_elements:[]}]} → notif
  `turn/started`{turn.id} → per-item `item/started`+`item/completed`
  (item types: `userMessage`,`reasoning`,`commandExecution`,`fileChange`,
  `mcpToolCall`,`agentMessage`,`webSearch`) → `turn/completed`{turn.status}.
  **REATTACH DOES NOT WORK in co-located stdio** — decisive for wonky bit 2:
  `codex app-server` is spawned as a **child of the client**, so dropping the
  client kills the server AND the in-flight turn. Live test: killed the client
  mid-approval (no reply), reconnected a fresh app-server client, `thread/resume`
  by threadId SUCCEEDED but returned the thread as **`idle`** (loaded from the
  on-disk rollout) — the pending approval was **not** re-emitted; the turn was
  gone. There is no daemon to reattach to on the stdio path. **⇒ the driver MUST
  hold the socket and block in-process (wonky bit 2's safe assumption is
  correct); exit-and-resume is impossible here.** (A shared `codex app-server
  daemon` + `proxy` control socket exists and *might* support reattach, but that
  is the remote/daemon model, explicitly out of scope per §3.)

  **(iv) Notification schema (to map in M2).** All notifications carry
  `threadId`; item notifs also carry `turnId` + `startedAtMs`/`completedAtMs` and
  a `ThreadItem` under `item` (`item.type` camelCase as above; command output
  streams via `item/commandExecution/outputDelta`; `agentMessage` text settles on
  `item/completed`). Turn cost/usage rides `turn/completed`'s `turn`.

  **Design verdict: M1 CONFIRMS the handoff; M2–M4 are green-lit unchanged**
  except the concrete decision keywords are v2 (`accept`/`decline`/`cancel` for
  exec+patch; `{action:…}` for the MCP-elicitation gate), not the legacy
  `approved`/`denied`. Transcripts + probe scripts live in the implementer's
  scratchpad (spike.py / spike_block.py / spike_mcp.py, transcript-*.jsonl). All
  codex/app-server processes killed; scratchpad-only.
- 2026-07-02 — Decomposed from notes-headless-elicitation.md §3/§4 + security.md
  entry 24 by Opus (planner) under Fable. Grounded against the sprint-4 worktree:
  the exec transport (`run_codex_capture`, `src/codeagent.rs:4168`) and its obs
  mapping (`codex_map_stream_event`, `:6451-6571`) are what the driver mirrors;
  the `ask_human` rail is **exit-and-resume** (`src/exec.rs:2141-2206` →
  `resume_suspended`, `src/dispatcher.rs:922`), which a **held RPC socket cannot
  use** — so the driver reuses the ask *emit* shape but must **wait in-process**
  (the central tension, wonky bit 2, flagged to Fable as a place the settled
  framing proved not literally implementable). Installed codex is 0.142.5
  (mcp-on-launch Log). M1 is a pure spike (no fix code) resolving §4's four
  unknowns. Judgment calls flagged: spike-first (1), blocking-wait fallback vs
  reattach (2), configurable default-deny timeout (3), flag-gated opt-in with exec
  fallback (4), obs-vocabulary reuse + honest fidelity stamp (5).

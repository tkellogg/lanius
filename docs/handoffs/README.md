# handoffs

Forward-looking implementation handoffs: work plans with milestones and
acceptance criteria, written to be picked up by another implementer (often
Codex). Each handoff carries its own "read these first" list and a Log section
for resolving open questions as the work proceeds. These are work orders, not
design records — the design lives in the docs/ files each handoff cites.

Distinct from the repo-root `HANDOFF.md`, which is gitignored local working
context for the pass currently in flight.

## Instructions
Please include a frontmatter at the top of each file and keep it up to date.
Something like:

```
---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-05-21
---

statuses: planned | in-progress | verifying | done
```

## Contents

- [worker-legibility-review.md](worker-legibility-review.md) - **changes
  requested** review of chainlink #8: real launch purpose can be polluted by
  harness flags, History's three states are not wired through the UI, and raw
  run IDs still lead the list hierarchy. Read before finishing or verifying
  [worker-legibility.md](worker-legibility.md).

- [coding-agents.md](coding-agents.md) - launch and supervise Codex and Claude
  Code under elanus as one envelope, two adapters: cage, hook→bus record,
  mailbox delivery, memory/context via the prompt hook, and the planner/worker
  orchestration loop. **M0 launcher + M1 hook→bus bridge landed for the Claude
  Code adapter** (2026-06-19, `elanus code`, [../../src/codeagent.rs](../../src/codeagent.rs)),
  with a fix pass closing the session-identity authority gap via a **grant-scoped
  per-session token** ([../../src/codesession.rs](../../src/codesession.rs)) — the
  broker resolves `code-*` as a scoped actor, not full authority.
  M2–M5 and the Codex adapter remain — see the handoff Log for as-built decisions.
  Backed by the one coding-agents journey
  [../journeys/02-claude-code.md](../journeys/02-claude-code.md) (the why); the
  Codex and Claude Code adapter references are Appendices A and B of the handoff.
- [configuration-ux.md](configuration-ux.md) - the configuration-UX altitude and
  scope pass on the web UI (instance vs agent config, essentials vs advanced,
  the off switch). Backed by [../journeys/06-configuration.md](../journeys/06-configuration.md).
- [web-ui-fidelity.md](web-ui-fidelity.md) - **not started**: the cross-cutting
  product-fidelity pass that sits on top of the configuration-UX work — contrast
  (two AA-failing color tokens, highest leverage), responsive/narrow, control
  fidelity (closed-set model + path pickers), accessibility (focus, tab ARIA,
  live-region conversation feed, hit targets, reduced-motion), product-language
  kernel-word eviction ("transmit"/"sessions"/"telemetry") + Lily's companion
  identity chip, and visual-consistency polish. From a live multi-lens UX review;
  the journey-specific structure is built, this is the layer on top. Backed by
  [../journeys/ui-preferences.md](../journeys/ui-preferences.md),
  [../journeys/characters.md](../journeys/characters.md), and
  [../journeys/07-chatting.md](../journeys/07-chatting.md).
- [coding-agent-dispatch.md](coding-agent-dispatch.md) - the agent-facing seam of
  worker dispatch: a front door (CLI help + honest briefing), the two dispatch
  modes (blocking-foreground for a live orchestrator vs async `spawn` for a
  headless planner), a footgun-free launch (no silently-dropped prompt), capture
  completeness (D4b), and in-band result visibility. Follow-on to
  [coding-agents.md](coding-agents.md). Backed by
  [../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md).
- [coding-session-reliability.md](coding-session-reliability.md) - **planned**:
  make `lanius code whose` evidence-based, make manual claim/unclaim normalization
  consistent across all claim readers, shift conflicting dev ports by default with
  `--fixed-ports`, and record the `.codex/skills` symlink invariant. Defers the
  dev-supervisor lifecycle spike and async spawn model/effort controls.
- [session-credential-lifecycle.md](session-credential-lifecycle.md) - **planned**
  (chainlink #10): the credential half of the claude-code crash class — a driven
  resume mints a fresh secret over a live incarnation's secret and then deletes
  the shared token file, so the live session's every hook publish is refused
  (`bad/unknown session token`); two supervised sessions lost in three days. M1
  makes credentials incarnation-safe (**ruling: reuse the live credential + mint
  ephemeral only for an idle session + secret-scoped compare-and-delete retire**;
  rejected multi-generation as unwarranted by the evidence). M2 gates resume as
  containment (**ruling: a delivery to a live session lands in the inbox, not a
  parallel resume**; serialize idle resumes). M3 records the terminal reason
  kernel-side when the bus is unauthorized. M4 flags a real residual: the
  adapter-staleness refresh (`5c16270`) runs only at seed/init, never at the
  launch use boundary, so an upgraded root keeps running the stale adapter. Sibling
  defects already fixed: phantom claims (`24ecdfb`), summary-race source
  (`5c16270`). Mirrors
  [../bugs/claude-code-adapter-summary-credential-crash.md](../bugs/claude-code-adapter-summary-credential-crash.md).
- [coding-agent-observability.md](coding-agent-observability.md) - the
  human-facing companion: materialize the obs/MQTT stream into sqlite, expose it
  via an API (server.mjs), and render the live session + nested subagent tree in
  the web UI (tool, model, effort, duration, resumed, resume command). Explainer
  agent deferred but kept possible via the API. Backed by the *Tim's perspective*
  section of [../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md).
- [harness-modes.md](harness-modes.md) - **the canonical mode model**: make every
  coding harness (Claude Code, Codex, future) launchable in *both* modes — `tui`
  and `headless` — with uniform CLI and semantics, via a `Harness` adapter seam and
  a per-(harness, mode) capture matrix. HM1–HM3 + OC3 landed: all three harnesses
  (claude, codex, opencode) now have both cells, bare → TUI, uniform `--headless`
  (`--worker` deprecated alias). Separates the **launch-mode** axis (tui/headless) from the **drive-pattern**
  axis (blocking/async) the other coding-agent handoffs use; they defer to this for
  the mode model. Backed by [../journeys/02-claude-code.md](../journeys/02-claude-code.md).
- [chat-conversations.md](chat-conversations.md) - the human's chat seat: turn raw
  kernel session ids into first-class, replyable **conversations** (labeled,
  one-context threads), persist one current web conversation with "+ new" and a
  recent list, and evict coding-tool agents from the chat nav into the Workers
  surface. The nav-split counterpart to
  [coding-agent-observability.md](coding-agent-observability.md). Backed by
  [../journeys/07-chatting.md](../journeys/07-chatting.md).
- [sibling-awareness.md](sibling-awareness.md) - the **agent-facing** coordination
  work plan: make a coding session know who else is in its working tree *by default*
  instead of tripping over them at commit time. Turns the three rungs of
  [../journeys/09-colliding-with-a-sibling-agent.md](../journeys/09-colliding-with-a-sibling-agent.md)
  into milestones — workdir-as-room (ambient claims, no `--room` flag), live siblings
  in the per-turn injection, and touch-is-claim (auto-claims off the fs cameras). The
  primitives already ship (dispatch handoff M5); this makes them ambient and default.
  Answers the "agents are bumping into each other" item in
  [../_questions.md](../_questions.md).
- [session-thread-grouping.md](session-thread-grouping.md) - **done** (TG1–TG3): collapse
  the N elanus sessions a manual `elanus code <tool> --resume` mints (fresh id per
  launch) back into one logical **thread** keyed by `native_session`, so the
  `elanus code sessions` listing + web tree + history reassemble instead of
  shattering. Read-model fold in `code_projection.rs` (`list_sessions` /
  `session_detail`) — **no identity/token/mailbox change**; the daemon resume path
  already reuses the id, so only manual relaunches need regrouping. Falls out of the
  `--resume` verification (hooks *do* fire on resume; the only real impact was
  audit/history fragmentation). Extends
  [coding-agent-observability.md](coding-agent-observability.md).
- [onboard-opencode.md](onboard-opencode.md) - **done** (OC1–OC5): make `opencode` a third
  first-class coding harness (`elanus code opencode`). Onboards like Codex —
  `opencode run --format json` is a raw-JSON-event stream (`Capture::StreamJson`, no
  hooks, no home pollution), `--session`/`--continue` give first-class durable
  resume, `--pure` is the no-plugins analog of Claude's `--setting-sources ''`. Key
  finding: opencode is **client/server** (`serve` + SSE, `attach`), so its TUI
  captures **live** — a better cell than codex's post-hoc rollout-import, warranting a
  new `ServerEvents` capture variant. The crux decision: do it *now* against the
  `Tool` enum (recommended — ships fast, becomes the real third case that de-risks
  the refactor) vs. fold into [harness-modes.md](harness-modes.md) HM1's `Harness`
  trait first (opencode is literally HM5's named validation harness). Answers the
  "onboard opencode" item in [../_questions.md](../_questions.md).
- [read-provenance.md](read-provenance.md) - **in-progress** (M1+M3 done; M2 deferred): make "what did this agent
  read" a subscription, the injection-provenance companion to the write camera.
  Answers the "detecting files read" item in [../_questions.md](../_questions.md) —
  but reframes it: the `_questions.md` deny→catch→allow→retry sketch is a worse
  seccomp-unotify (the cage is static, elanus isn't in the syscall path), and
  `sandbox.md` already settled on allow-and-notify. The catch: an *authoritative*
  ("can't be bypassed by `Bash`+`cat`") read camera is intrinsically a
  syscall/FS-boundary problem — **authoritative + macOS + no-root/no-entitlement,
  pick two**. **M1** projects the `Read`/`Grep`/`Glob` tool calls *already on the
  bus* (Claude Code's `PreToolUse:*` hook) into a path-keyed `obs/fs` view — but it's
  **advisory/bypassable**, not the answer; **M2** is the authoritative cage camera
  that sits below the shell (seccomp-unotify on Linux = the only authoritative *and*
  unprivileged box; macOS needs root `fs_usage`/DTrace or a signed ES extension —
  accepted-gap for now), gated on coding agents actually being caged
  ([coding-agents.md](coding-agents.md)); **M3** status/config legibility +
  fast-fail subscribe. Backed by
  [../journeys/10-what-did-the-agent-read.md](../journeys/10-what-did-the-agent-read.md)
  and the "read camera" section of [../sandbox.md](../sandbox.md).
- [authority-delegation.md](authority-delegation.md) - the **delegation** half of
  the identity model: a spawned actor's authority must be a strict subset (≤) of
  its spawner's, reconstructed at spawn and enforced at mint (`child.grants ⊆
  parent.grants`, monotone down the chain), with two flavors — capability subsets
  (`lease ⊆ grant` generalized) and partitioned budgets (`Σ children ≤ parent`, the
  RLM "halve it to pass context down" case). Closes the doctrine on the
  "more-authority-than-warranted" class (security.md entries 13/16/20/21). Backed by
  [../identity.md](../identity.md) ("Delegation") and
  [../security.md](../security.md) entry 22.
- [memory-blocks.md](memory-blocks.md) - **done** (M1–M4), the **keystone** of the
  profiles journey: make memory blocks a first-class, built-in part of the context
  pipeline — named, durable, agent-editable kv chunks with a default that
  *evolves*, rendered by a built-in block→text step so a "computed block" is just a
  vanilla stage that adds an entry and downstream stages see them uniformly. Bridges
  the unwired `context_blocks` substrate (`src/context_blocks.rs`, `db.rs:288`) into
  the live `context::Doc` path, adds an `elanus block` write surface, and projects
  blocks into the coding-agent injection seam with a placement→injection-vector
  ladder (next-turn / mid-cycle / algedonic) — the mid-cycle vectors **de-risked by
  a live cross-harness spike** (Claude Code `Pre/PostToolUse` ✓, opencode
  `prompt_async` ✓, Codex degrades). Backed by
  [../journeys/11-profiles.md](../journeys/11-profiles.md) and [../context.md](../context.md).
- [agent-comms-package.md](agent-comms-package.md) - **done** (C1–C4): inter-agent
  comms as a **package that rides on blocks**, not a subsystem — per Tim, "just have
  blocks." The transport (mailbox, rooms/`in/group`, failure-mail, inbox) already
  ships; this adds a comms-etiquette **skill** (no block dependency, ships now), the
  "unread from agent Y" surface as a **computed block** (generalizing
  `turn_injection`'s hardcoded inbox text), **priority→injection-vector** (high-pri
  mail lands mid-turn), and a **shared channel as a block** (a room's recent traffic,
  the journey's per-repo channel). Depends on [memory-blocks.md](memory-blocks.md).
  Backed by [../journeys/11-profiles.md](../journeys/11-profiles.md).
- [work-estimation.md](work-estimation.md) - **done** (E1–E3): an agent estimates
  its work right after planning, actuals are counted against it, and a retro adjusts a
  memory block so the next estimate improves. A **package** with **no kernel
  data-model representation** (Tim's constraint) — state lives in blocks + obs
  events, actuals read from the obs stream (`src/code_projection.rs`). Estimates are
  multi-dimensional (turns/tokens/wall-clock) but **dollars-normalized**; the live
  risk is that **dollars have no source** (`src/models.rs` has no pricing — see
  [../journeys/03-cost-visibility.md](../journeys/03-cost-visibility.md)), so it
  ships with a package-local pricing map. Depends on [memory-blocks.md](memory-blocks.md).
  Backed by [../journeys/11-profiles.md](../journeys/11-profiles.md).
- [chat-rendering.md](chat-rendering.md) - **planned**: how a UI decides what to
  show for an agent. Reframes the `_questions.md` "`send_message` /
  configure-how-chat-is-displayed" item: display is **not** per-agent UI config —
  it's a **read off two bus planes**, generic to any client. Rule: comms-plane
  traffic with me (`in/human`/`in/dm`/`in/group`) exists → render the
  conversation; else interpret the obs trace. `ask`/`send_message` unify into one
  *send to a channel* verb differing only by a **suspend flag** (invisible to
  UIs — `ask` was mis-factored, not wrong); suppression is at emission, with a
  pre-human-message bus-interception seam deferred; subagent control via an
  `inherit_to_subagents` package-manifest flag (default true). Extends
  [chat-conversations.md](chat-conversations.md) and
  [coding-agent-observability.md](coding-agent-observability.md); the
  reach-the-user policy is split to
  [../journeys/reaching-the-user.md](../journeys/reaching-the-user.md). Backed by
  [../actors.md](../actors.md), [../identity.md](../identity.md), and
  [../journeys/07-chatting.md](../journeys/07-chatting.md).
- [agent-comms-ui.md](agent-comms-ui.md) - **done** (M1–M6): the **human's seat** for
  the three just-shipped agent-facing capabilities — they are CLI + per-turn injection
  only, so a human can't *see* the cross-agent traffic. Comms-first: M1 a `code mail
  --json` ledger projection + `GET /api/comms/mail` (deliveries threaded by
  correlation, with priority/state/failure-mail — the data is already `in/agent/*`
  events on `/api/stream`, so it's a projection, not new capture), M2 a `CommsView`
  traffic view (FROM→TO, priority chips, live-folded like `CodeSessions`), M3 rooms
  & shared channels (roster/`room_recent`/`peer_claims`); then M4 a memory-block
  inspector (read-only first), M5 estimate-vs-actual in the runs detail, M6 the
  mid-cycle/priority signal lamp. Also records six correctness/UX concerns in the
  shipped code to respect while building. From a clean xhigh review; extends
  [coding-agent-observability.md](coding-agent-observability.md) and
  [chat-conversations.md](chat-conversations.md). Backed by
  [../journeys/11-profiles.md](../journeys/11-profiles.md).
- [model-providers.md](model-providers.md) - **planned**: make a **provider** a
  named, encrypted credential elanus can point any LLM consumer at — the genai
  dispatcher *or* a coding harness. Collapses two `_questions.md` items (the
  "provider-setup link" and "set a model provider on a subagent") into one missing
  primitive. The credential is a **sum type with a per-consumer validity matrix**
  (`ApiKey{wire,url,key,headers}` feeds both; `NativeLogin` — the Claude.AI/ChatGPT
  login — feeds only a harness, never the dispatcher), so `materialize(cred,
  consumer)` is **partial** and fails closed. Stored **encrypted in SQL** (keyring
  too inconsistent for a headless daemon; master key in a `0600` file, threat model
  = accidental disclosure), as a plain **resource** — not config-repo, not an
  identity/authority (audited, not gated). Selected via an elanus option *before*
  the tool token (`elanus code --provider deepseek claude --resume`) so tool args
  forward verbatim; the existing harness **scrub** becomes the *enabler* of nesting
  ("codex-on-ChatGPT-inside-codex-on-GLM"). M1 vault + sum type → M2 harness/launch
  (delivers #5) → M3 dispatcher (`build_client`, src/exec.rs) → M4 the #4 UI.
- [telegram-bridge.md](telegram-bridge.md) - **planned** (chainlink #15): close the
  loop so Tim can chat with his running agents from his phone on the *same*
  conversation the web UI shows. The bridge **daemon is already built and committed**
  (`packages/telegram/`, `eeaf712`) — long-poll ingress, `sender=telegram` egress
  receipts, phonebook sightings, park-not-crash; this is Handoff C's
  ([agent-dm-relay.md](agent-dm-relay.md)) unfinished second half. Four verified
  gaps: **M1** reply routing — nothing forwards `in/human/<owner>` →
  `in/package/telegram/send`, so an agent's reply renders in the web but never
  reaches the phone (stateless correlation-follow, the routing twin of
  `reply_source`); **M2** owner-sender authentication + fail-closed promotion —
  resolve a chat id to `owner` via the phonebook and promote onto `in/human/<owner>`
  (`source:telegram`), an unknown sender records a sighting but never reaches an
  agent; **M3** bot token as an encrypted vault credential materialized into the
  daemon env at the spawn seam (`dispatcher.rs:495-521` injects `BUS_TOKEN` but
  **never** config keys today — closes an absent wiring *and* the plaintext
  exposure); **M4** stub-transport e2e round trip + BotFather on-ramp. **Rulings:**
  outbound = long-poll (a laptop behind NAT has no public endpoint); reply routing =
  deterministic correlation-follow, distinct from the still-deferred EA "pick a
  platform unprompted" policy (`channels.md` gap 4). Depends on A
  ([principal-kind.md](principal-kind.md)) + B ([dm-channel-grammar.md](dm-channel-grammar.md)),
  both done. Backed by [../journeys/chat-from-anywhere.md](../journeys/chat-from-anywhere.md).
- [web-packaging.md](web-packaging.md) - **in-progress** (M1–M3 shipped, M4
  deferred): make `cargo install elanus` serve the web UI with **no Node.js, no
  npm, and no source tree at runtime** — fold `server.mjs` into a Rust `src/web.rs`
  (`ntex::web` + a `rumqttc` SSE relay + direct `elanus.db` reads; no new heavy
  deps) and embed the built SPA in the crate so `cargo publish` ships it.
  `server.mjs`/`config.mjs` kept as fallback during soak; admin shell-outs retire
  to in-process calls in M4. Answers the "Rust + web packaging" item in
  [../_questions.md](../_questions.md).

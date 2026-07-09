---
status: done
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: the helper's first encounter (H4)

Tim's first real contact with the helper went three ways wrong
(docs/journeys/16-the-helper.md): (1) he typed a message and got dead air;
(2) merely opening the panel spawned a live "Helper" agent in the left-hand
agent list — "What did I activate? Can I make it go away?"; (3) clicking
into that Helper agent showed no messages, not even the one he'd just sent.

Mechanics: the right-side `#ai-panel` (pre-split App.tsx:1684-1708) mounts
`AgentAssistant` (profile "helper"), whose mount effect AUTO-SENDS an opening
prompt (ui/web/src/components/AgentAssistant.tsx:91-95) — opening the panel
starts a live agent turn. It uses private `assistant-<rand>` session ids
(AgentAssistant.tsx:25), so its messages never appear in any Converse pane
(the /api/conversations projection keys web-* sessions), while the live turn
makes the helper light up in the agent list via `touchAgent`
(App.tsx:1334-1338).

Scope guard: the bigger concierge vision — an "Ask" button on every confusing
UI element that opens the helper already knowing what you're looking at — is
EXPLICITLY a follow-up handoff. This handoff fixes the first encounter and
specs the seam that vision will plug into (wonky bit 6). Do not build Ask
buttons.

## Dependency edges

- Requires app-tsx-split (H0) and chat-liveness (H2) — consumes H2's
  `useSystemHealth` projection for its dead-air handling.
- Parallel-safe with package-truth and chrome-polish (disjoint files:
  AgentAssistant.tsx, the #ai-panel block in App.tsx, views/Nav.tsx).

## Read these first

1. docs/journeys/16-the-helper.md — the judging document. All four
   "expectations that fall out" are acceptance criteria here.
2. ui/web/src/components/AgentAssistant.tsx — the whole file (180 lines):
   auto-send (91-95), `sendPrompt` (64-80), the SSE reply/tool loop
   (97-140), the session id scheme (25).
3. App.tsx #ai-panel block (pre-split 1684-1708) including the world-c guard
   (1693-1697), and `profiles_with_helper` in src/web.rs (grep it — the
   server already synthesizes the helper profile row).
4. docs/handoffs/chat-liveness.md — the health hook and the three-state
   pending machine this handoff mirrors.
5. docs/handoffs/helper-m4-harness-backed-turns.md — how helper turns
   actually run (background).

## Wonky bits / decisions (already made)

1. **No auto-send. Ever.** Delete the mount-effect send
   (AgentAssistant.tsx:91-95). The `intro` prop renders as a static first
   bubble from the helper (visual welcome, zero cost, zero side effects).
   The first LIVE turn starts when the person sends their first message.
   Opening the panel must create nothing durable — looking is free.
2. **The panel is the helper's ONE surface; the helper is not an agent-list
   row.** One helper, one thread, one place. The helper profile is hidden
   from the Nav agent list — but NOT by matching the literal name "helper"
   (that is the `is_worker_session` anti-pattern, the reference violation of
   simple-core). Instead:
   - a **generic, documented profile property** in the profile TOML, e.g.
     `[ui] surface = "panel"` (pick the exact key; generic and boolean-ish,
     meaning "this profile is presented in a dedicated surface, not the
     agent list");
   - the server includes the property in the profile rows it already returns
     (`profiles_with_helper` / `profile list` relay), and stamps it on the
     helper profile it synthesizes;
   - Nav filters on the PROPERTY. Any future panel-surfaced profile gets the
     same behavior for free; nothing anywhere matches the string "helper".
   - Document the property in docs/config.md's profile-field list.
3. **Dead air dies here too, by reuse.** `AgentAssistant` gets the same
   three-state treatment as ConverseView, via H2's health projection and the
   same vocabulary: local echo (already exists), thinking indicator once
   correlated obs activity appears, "No response yet…" + check-status +
   retry after the same 20s constant, and the send-time pre-check (the
   world-c guard at App.tsx:1693 generalizes: broker down ⇒ say so before
   sending). Reuse `lib/health.ts`; do not fork a second heuristic. The
   message the user sent stays visible in the panel feed no matter what.
4. **Stop and dismiss.** Two distinct affordances, both in the panel head:
   - **stop** — visible while a turn is running (`busy`); ends the wait
     locally (and if a cancel primitive exists on the bus, invoke it —
     check `lanius`'s ask/cancel surface; if none exists, local-stop is
     honest: "stopped waiting — the agent may still finish in the
     background"). Never leaves `busy` stuck true.
   - **done/dismiss** — closes the panel (exists today as onDone/×). Closing
     while idle leaves nothing behind; with no auto-send there is nothing
     half-started to leak.
5. **The thread survives the panel.** Today a close+reopen mints a fresh
   `assistant-*` session (sessionRef init, AgentAssistant.tsx:48) and the
   old exchange evaporates — that's the "message vanished" feeling. Keep the
   current session id in localStorage (like `lanius.aiPanel`, App.tsx:491-494)
   and reuse it on remount, with an explicit small "new conversation" action
   in the panel head to rotate it. In-memory feed rehydration from history
   is NOT required this sprint (the durable transcript exists if the history
   package is on); the requirement is: reopening the panel in the same
   browser continues the same helper conversation.
6. **The concierge seam (spec only, no implementation).** Leave one typed
   entry point the follow-up handoff will use: `AgentAssistant` accepts an
   optional `context` prop ({ view: string, detail?: object }) that, when
   present, is attached to the NEXT user-initiated send's payload (alongside
   `client_tools`). Wire nothing to it yet — the prop exists, is documented
   in the component header comment, and is exercised by one unit-level
   assertion. The Ask-button handoff plugs view context into this without
   touching the send path again.
7. **Language rules:** the raw session id shown in the panel head
   (AgentAssistant.tsx:154, `{sessionRef.current}`) goes away — nobody
   should read `assistant-m3k9…` to know what they're looking at. Say
   nothing, or say "helper".

## Milestones

### M1 — browse is free: no auto-send, hidden from the list, honest head

Wonky bits 1, 2, 7.

**Acceptance (real app, re-embedded binary):** opening the helper panel
publishes NOTHING (watch /api/stream or the activity rail — zero in/agent
events), shows the intro bubble, and no Helper row appears in the agent
list; the profile property (not the name) drives the filtering — a test
profile with the same property set is also panel-only; the property is
documented; full ui.spec.mjs green.

### M2 — the first message works or says why not

Wonky bits 3, 4.

**Acceptance:** daemon up: first send → sent mark → thinking indicator →
reply in the panel. Daemon down: the same honest "No response yet…" line +
recourse as the main chat, and the sent message remains visible; stop ends
a running wait without wedging `busy`; broker-down pre-check refuses
optimistically sending, with the setup pointer.

### M3 — one thread that survives, plus the seam

Wonky bits 5, 6.

**Acceptance:** send a message, close the panel, reopen — the same
conversation is there (same session id, feed continuity within the browser
session); "new conversation" rotates it deliberately; the `context` prop
exists with its documented shape and a passing assertion, and nothing sends
it yet.

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Decisions: panel is the sole
  surface; hide-by-property (generic TOML profile property, never the
  literal name); helper's dead-air fixed here (not H2) to keep one owner
  for AgentAssistant.tsx; Ask-button concierge deferred to a named
  follow-up with only the `context` seam built now.
- 2026-07-09 — implemented (Opus worker, child worktree h4): auto-send
  removed (intro = static bubble; opening the panel publishes NOTHING);
  helper hidden from Nav via the generic `[ui] surface = "panel"` profile
  property (src/profile.rs UiCfg, emitted by profile list + agent catalog,
  stamped on the synthesized helper row, documented in docs/config.md);
  panel dead-air reuses lib/health.ts (STALL_MS moved there — one
  constant); stop/dismiss; session persisted in localStorage
  (lanius.helperSession) + "new conversation" rotation; concierge
  `context` prop seam (documented, unit-asserted, wired to nothing); raw
  session id out of the panel head. 26 new e2e (suite 362).
- 2026-07-09 — VERIFIED (adversarial Opus, fresh context; GPT-5.5 channel
  unavailable): pass=true, all 8 focus items PASS — property-driven
  filtering with zero literal-name logic ("main" provably never hidden in
  a fresh root); live-stack zero-publish-on-open (incl. persisted
  remount); ConfigureView modal regresses nothing; single STALL_MS;
  crash-safe persistence; seam attaches only-when-present; server
  plumbing additive/back-compat (no synthetic-helper double rows); scope
  exactly the 12 expected files. tsc clean, full cargo test green,
  ui.spec.mjs 362/362 (an earlier rc=1 was a port/pid collision between
  back-to-back suite runs, not a failure).
- 2026-07-09 — merge-back (planner): two stale comments fixed as part of
  the merge (App.tsx ai-panel + ConfigureView modal both still claimed an
  auto-send "opening publish"). Residual LOW notes (triaged by Fable, not
  fixed): window.__assistant* test hooks ship in the production bundle;
  closing the panel drops stalled/pending indicator state (the thread
  itself survives) — corr-keyed survival across close/reopen not required
  by this handoff.

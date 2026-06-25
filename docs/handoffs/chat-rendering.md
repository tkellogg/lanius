---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-24
---

# Handoff: rendering an agent conversation — comms plane vs. trace (+ the send/ask family)

Answers the `_questions.md` chat-panel item: *"agents with a `send_message` tool
message the user only when they need to … but for turn-based coding agents that
doesn't make sense. Should chat display be configured per-agent?"*

The first draft of this handoff over-built it (a per-agent "posture" config knob).
The real model, settled in the planning chat, is simpler and far more **generic**:
chat display is not a UI setting at all — it is a **read off two bus planes**, so
any UI (this one, or a better one someone else builds) makes the same decision
from the ledger, not from app-private config.

## The model (the load-bearing reframe)

Everything an actor does to talk is one primitive: **send a message to a
channel** ([../actors.md](../actors.md): "sending to another elanus actor is just
writing to that actor's inbox", :194). The web UI is itself an actor — a
*channel* of the owner identity (the "elanus channel",
[../identity.md](../identity.md):362). So the chat panel is just one client
reading the bus.

Two planes carry what a UI needs:

- **The comms plane** — messages addressed to/from *me*: `in/human/<owner>`,
  `in/dm/<kind>/<addr>`, and the shared-channel noun `in/group/<id>`
  ([../topics.md](../topics.md):129, "a group chat is just a new noun").
- **The observation plane** — the agent's trace: `obs/agent/<noun>/<session>/…`
  (`src/code_projection.rs` already folds this into the runs view).

**The rendering rule, for any UI:**

> Does comms-plane traffic between me and this agent exist? **Yes** → render the
> conversation from the comms plane. **No** → synthesize a view by interpreting
> the obs trace.

That's the whole thing. A *companion* agent curates discrete user-facing messages
on the comms plane (via `send_message`) and the UI shows those; a *coding/worker*
agent has no curated comms-plane messages and the UI interprets its obs trace
(which the Workers surface already does,
[coding-agent-observability.md](coding-agent-observability.md)). No per-agent UI
config, nothing DOM-private, no posture enum. The distinction lives on the bus.

Note a native agent's end-of-turn reply is *already* mail to the human on
`in/human/<owner>` (`src/exec.rs`:554) — so "assistant that just answers when
asked" is the comms plane too (one reply per turn). The genuinely different case
is the *worker*, whose value is its trace, not a reply. The rule above captures
all three without naming them.

## The send / ask family (resolving "should `ask` exist?")

There are **not** two comms primitives. There is one — *send a message to a
channel* — and `ask_human` (`src/exec.rs`:1259 def, :1370 handler) just bundles
three things that need not travel together:

1. send a message to the human's inbox,
2. **suspend the run** until a reply lands (`pending_ask`, :1416),
3. thread the reply by correlation.

Only (2) creates the "request/response" feel, and (2) is a **run-scheduling**
decision, not a comms distinction. The honest axis has three positions:

- fire-and-forget (no reply expected),
- expect a reply but **keep working** (handle it as inbound mail whenever it
  lands),
- expect a reply and **suspend now** (today's `ask`).

The first two are `send_message`; the third is `ask`. So `ask` is **mis-factored,
not wrong**: the clean shape is one `send`/`message` verb to a channel with an
optional `await`/`suspend` flag, and `ask` becomes `send(expects_reply,
suspend=true)`. Keep it (the CLI/TUI "human is right here, I can't proceed" case
earns it) — just refactor it into the family.

**The UI never sees the difference.** It renders a message; if the message
carries options / expects a reply it shows the answer affordance (the existing
`AskMessage`, `ui/web/src/App.tsx`:1998); otherwise it just shows it. Whether the
agent is parked waiting is the agent's business, invisible to every client — the
genericity payoff again.

## Suppression is at emission, not interception

For a `send_message`-style agent that should *not* turn its whole trace into
chat: it simply **doesn't emit the end-of-turn reply mail** and speaks only via
`send_message`. The unwanted messages are never published, so there is nothing to
mark or filter. No bus mechanism needed.

The richer alternative — keep the agent "thinking out loud" on the bus for
observability while the UI hides it — would need a **pre-human-message blocking
interception** where a package stamps `display=false` and the UI filters on it.
That is a real, coherent extension: it generalizes the existing `pre`/`post`
**tool-call** blocking seam ([../bus.md](../bus.md):265, `src/broker.rs`:24) to a
`pre`-human-message seam. **Deferred** — we have no non-UI subscriber that needs
the unmodified stream yet; building the seam now is speculative. Recorded so it
stays possible.

## Subagent inheritance — the `inherit_to_subagents` flag

A worker subagent should not get `send_message`. Prior art:

- Package **visibility** = the profile's `elanus_path` (`src/profile.rs`:44),
  which inherits from the parent scope via the literal `"$parent"`
  ([../bus.md](../bus.md):388).
- Subagents are ordinary agents with their **own** profile, allowlisted by the
  parent's `[subagents].allow_profiles` ([../context.md](../context.md):155), so
  packages come from the child's path — *unless* it uses `$parent`, which pulls
  everything.
- Authority (grants) is a separate, strictly-`⊆` concern
  ([authority-delegation.md](authority-delegation.md)); this handoff does not
  touch it.

There is **no** per-package inherit knob today. **Decision (confirmed with
Tim):** add a package-manifest flag `inherit_to_subagents = true|false` (default
`true`), consulted when a child resolves `$parent`. Set the comms/`send_message`
package to `false` and it never flows down to workers even under `$parent`. Small
and local: package manifest + the `elanus_path` resolver. This is the only new
mechanism in the package/visibility model that this work introduces.

## Decisions (resolved in planning — recorded, not open)

1. **No per-agent UI posture config.** Replaced by the comms-plane-vs-trace read
   rule. The chat panel ships with zero per-agent display config.
2. **`ask` refactored into the `send` family**, not deleted; the suspend flag is
   the only difference, invisible to UIs.
3. **Suppression at emission**, not bus interception. The pre-human-message
   interception seam is a documented future extension, gated on a real non-UI
   consumer.
4. **`inherit_to_subagents` package flag**, default `true`; comms package sets it
   `false`.
5. **Reaching-the-user / EA policy is out of scope here** — split to its own
   journey, [../journeys/reaching-the-user.md](../journeys/reaching-the-user.md),
   because it is a policy layer on the already-built phonebook/recall/egress
   rails, not a chat-rendering concern.

## Milestones

### M1 — The `send` family: `send_message` (non-suspending) + refactor `ask`
Introduce the unified agent-side verb/tool that sends a message to a channel
(`in/human/<owner>` for the owner; the same shape reaches `in/group/<id>` or an
`in/dm/<kind>/<addr>` via a bridge). `send_message` = no suspend, no required
reply. Refactor `ask_human` (`src/exec.rs`:1259/1370/1416) to be the
`suspend=true, expects_reply=true` mode of the same verb — one emit path, one
correlation discipline, one transcript record. Document both in the
comms-etiquette skill (`kits/core/packages/comms-etiquette/`): when to speak
unprompted vs. stay quiet (the "feel alive, don't spam" discipline,
[../journeys/07-chatting.md](../journeys/07-chatting.md)).

**Acceptance:** an agent can `send_message` to its owner without suspending the
run; the message lands on `in/human/<owner>` and is replyable (continues the
thread, not a dead-end). `ask` still suspends and resumes on the correlated
answer (existing behavior preserved through the refactor). Both thread by
correlation with no duplicate against the live tail. A unit test covers
send-without-suspend vs. ask-with-suspend sharing one emit path; the skill names
the real verb(s).

### M2 — Chat panel renders comms-plane-vs-trace (the generic rule)
In the converse view (`ui/web/src/App.tsx`:1958, message render :1985,
`AskMessage` :1998), decide what to show by **whether comms-plane traffic exists
between the owner and this agent** (M1's messages on `in/human`/`in/dm`/`in/group`
involving me), not by any per-agent setting:
- comms-plane traffic exists → render it as the conversation (each message a
  first-class turn; messages that expect a reply show the answer affordance);
- none → fall back to interpreting the obs trace (the Workers/runs surface
  already owns the trace view; the chat panel either links there or renders a
  minimal trace-derived summary).
This is presentation only — it mints no authority and changes no ledger data
([elanus-conventions]). The rule must be expressible from the ledger alone so a
third-party UI reproduces it.

**Known limitation (documented, not a defect for the companion case).** The
conversation projection (`src/web.rs` `conversation_rows`) seeds sessions from
`in/agent/<agent>` inbound events and folds `in/human/<owner>` replies in by
correlation (`corr_to_session`, populated only from those `in/agent` events). A
*fully unprompted* agent-initiated `send_message` — one whose correlation was
never established by a prior `in/agent` prompt (e.g. a cron-triggered message with
no preceding owner turn) — therefore lands on `in/human/<owner>` but produces no
conversation row. This is acceptable here because companion agents always have a
prior owner prompt that establishes the correlation (the common
owner-prompts-first flow). If a genuinely unprompted-only `send_message`
conversation must render, seed `corr_to_session` (or a session) from
`in/human/<owner>` rows whose payload carries a session, not just from `in/agent`.

**Acceptance:** an agent that has sent `send_message`/`ask` traffic to the owner
renders a conversation built from the comms plane; an agent with only an obs
trace (a coding worker) does not appear as a chat conversation but is reachable as
a trace/run; the decision is derivable purely from bus/ledger reads (no
app-private flag). `ui.spec.mjs` asserts: a seeded comms-plane agent shows a
`#view-converse` conversation with at least one message; a seeded trace-only agent
shows no chat conversation (and is present in the runs surface), following the
existing `data-sel`/`waitForSelector` discipline.

### M3 — `inherit_to_subagents` package-manifest flag
Add the manifest flag (default `true`) and honor it in the `elanus_path` resolver
when a child profile resolves `$parent`: a package marked `false` is excluded
from the child's visible set. Set the comms/`send_message` package to `false`.

**Acceptance:** a subagent whose profile uses `elanus_path = ["$parent"]` does
**not** see a package marked `inherit_to_subagents = false` (so it has no
`send_message` tool), while still seeing default-inheriting packages; a package
with the flag unset behaves as before (inherited). A unit test in
`src/profile.rs`/`src/packages.rs` covers the resolver exclusion; the comms
package manifest carries the flag.

### M4 — (Deferred, keep possible) pre-human-message interception + agent-driven choice
Two deferrals recorded so they stay reachable, not built now:
- the **pre-human-message blocking interception** seam (the "think out loud on
  the bus, hide in the UI" core extension above), gated on a real non-UI
  consumer;
- letting an **agent set its own** comms behavior at runtime
  ([../journeys/ui-preferences.md](../journeys/ui-preferences.md): "the UI fully
  operable by the agent"), audit-logged via the existing `--by` attribution
  ([agent-comms-ui.md](agent-comms-ui.md) Log).

## Read these first
- The why: [../journeys/07-chatting.md](../journeys/07-chatting.md) ("What
  chatting should feel like"), [../journeys/characters.md](../journeys/characters.md).
- The comms model: [../actors.md](../actors.md) ("Reaching an actor: channels,
  and which way they flow" — the ingress/egress asymmetry), [../topics.md](../topics.md)
  (`in/human`, `in/group`, the category set), [../identity.md](../identity.md)
  (channels of an identity; the web UI as the elanus channel).
- The adjacent surfaces: [chat-conversations.md](chat-conversations.md) (the chat
  seat + nav split that evicts workers — prerequisite) and
  [coding-agent-observability.md](coding-agent-observability.md) (the trace/runs
  surface this falls back to).
- The split-out follow-on: [../journeys/reaching-the-user.md](../journeys/reaching-the-user.md).
- The code: `src/exec.rs` (`ask_human` :1259/:1370/:1416, reply-as-mail :554),
  `ui/web/src/App.tsx` (`#view-converse` :1958, `AskMessage` :1998),
  `src/profile.rs`/`src/packages.rs` (`elanus_path`/`$parent`),
  `kits/core/packages/comms-etiquette/`.

## Log
- **2026-06-24 — M1–M3 shipped** (handoff-workflow: impl Opus/medium, one focused
  agent per milestone, sequential → adversarial verify Opus/high, **pass round 1**,
  no fix rounds needed). As built:
  - **M1:** `send_message` and `ask_human` now route through one shared
    `emit_message()` path (`src/exec.rs`) onto `in/human/<owner>`.
    `send_message` returns `ToolOutcome::Output` (no suspend) and threads onto the
    turn correlation so it's replyable; `ask_human` keeps its exact behavior
    (mints a correlation, parks `pending_ask`, returns `ToolOutcome::Suspend`,
    resumes on the correlated answer). Etiquette skill documents both.
  - **M2:** the converse view (`ui/web/src/App.tsx`) decides comms-vs-trace from
    `/api/conversations` (a pure ledger/bus projection) — `data-mode=comms` with
    a feed when comms-plane traffic exists, `data-mode=trace` + a runs link
    otherwise. No per-agent UI display flag; the worker-eviction signal reuses the
    existing structural coding-noun set + bus-derived `code-*` session ids.
  - **M3:** manifest gains `inherit_to_subagents` (default `true`) and
    `provides_builtin_tools` (`src/manifest.rs`); `comms-etiquette/elanus.toml`
    sets `inherit_to_subagents=false` and owns `[send_message, ask_human]`. The
    `elanus_path` resolver (`src/profile.rs`/`src/packages.rs`) drops a
    `false`-flagged package only when reachable *solely* via `$parent`; built-in
    tools are withheld via `withheld_builtin_tools` only when a visible-universe
    package claims them AND it's filtered out — **fail-open** when nothing claims a
    tool, so the default/main agent never loses `ask_human`.
  - **Tests:** `cargo test` 292 pass (3 new); `ui.spec.mjs` 163 pass against the
    Rust `elanus web` server (`ELANUS_UI_SPEC_RUST=1`, real chromium), incl. the
    new flow-6b M2 assertions.
  - **M4 deferred** as planned (pre-human-message interception seam; agent-driven
    comms choice).
  - **Two LOW residuals** (accepted, not blocking): (1) the M1 unit test exercises
    `emit_message()` directly rather than driving the `run_tool` handler — the
    suspend/no-suspend distinction is structurally correct + all tests pass, but a
    handler-level test would lock it; (2) M2's trace-fallback gate OR's a global
    coding-noun *name* set with the bus-derived `code-*` session signal — the
    primary decision (`hasComms`) is pure ledger, the name set is a global
    structural fact reused from the existing nav-eviction, so it meets the
    "ledger-derivable" bar, but a strict reading would key the fallback solely on
    the bus-derived worker-session signal.
- **2026-06-24 — replanned (this rewrite).** First draft modeled chat display as
  a per-agent "posture" config knob; Tim's reframe collapsed it: chat display is
  a **comms-plane-vs-trace read off the bus**, generic to any UI, with no
  per-agent UI config. `ask`/`send_message` unified into one *send to a channel*
  verb differing only by a suspend flag (invisible to UIs); suppression at
  emission, with bus interception deferred as a documented core extension;
  subagent control via an `inherit_to_subagents` package flag (default true).
  The "reach the user across channels by policy" idea split to its own journey
  (it rides the already-built phonebook/recall/egress/human-proxy rails). Five
  decisions resolved, not left open. Grounded against `src/exec.rs` (the real
  `ask_human` tool), `src/profile.rs` (`elanus_path`/`$parent`), and the
  actors/topics/identity comms model.

---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-01
---

# Handoff: make the dashboard tell the truth

Three ways the web dashboard currently says things that are not quite true. Each
is small on its own; together they are the difference between a screen a cautious
person (Ganesh, journey 04) trusts and one they don't. One handoff, three
milestones, independent of each other.

## Wonky bits / decisions up front

1. **Liveness comes from the bus, not a JSON file.** The recon brief expected a
   `run/actors.json` snapshot written by the dispatcher. **It does not exist in
   this tree.** Liveness is published as a *retained* bus message per actor:
   `status_event` (`src/dispatcher.rs:546`) publishes `obs/package/<name>/status`
   with `{ state, ... }` — `alive` carries the pid at spawn, `dead` carries the
   exit code, plus failure states. So the liveness data is already on the bus and
   in the ledger; the gap is only that `src/web.rs` and `App.tsx` never surface
   it. Wire that retained status through, do not invent a file. *Fable: this
   corrects the brief's premise.*

2. **The cost panel is split across two files.** The per-session dollar/turns/
   tokens estimate report already renders — but in `ui/web/src/CodeSessions.tsx`
   (via `GET /api/estimate/{session}`, `src/web.rs:217`/`:793`), not in the setup
   "cost visibility" panel. `App.tsx` only shows *static* cost hints
   (`costSummary`, `ui/web/src/App.tsx:238`; the fold at `:1531-1540`). So the
   cost-honesty work is in `App.tsx`'s cost fold: fix the hard-vs-soft mislabel,
   and honestly reference the estimate that lives in the runs view. *Fable: this
   corrects the brief's App.tsx-only assumption.*

3. **Vocabulary is judged against [../layering.md](../layering.md).** Any word
   that only makes sense once you know how elanus works inside must not appear in
   the interface: "kit", "package", "topic", "correlation", "approved" (as a
   status word next to capabilities), "session". The fix is translation at the
   boundary, not deletion of the feature.

## Milestones

### M1 — Liveness: installed vs approved vs running vs failed
Journey 04's acceptance: "a user can tell the difference between installed,
approved, running, and failed." Today a stopped daemon looks identical to a
running one. Add a read endpoint in `src/web.rs` that reports each actor's latest
retained `obs/package/<name>/status` state (from the ledger — the retained status
events are sqlite-backed), and show it in `App.tsx` where capabilities are listed
(the installed-capabilities fold, `ui/web/src/App.tsx:1552`, and the risk badges).
Product words: "running" / "stopped" / "failed" / "not started" — never "actor"
or "daemon".

**Acceptance:** `ui.spec.mjs` seeds a `dead`/failure status event for one
capability and an `alive` status for another, and asserts the UI shows them as
distinguishable states (a "failed"/"stopped" indicator vs "running"). A capability
with no status shows "not started", not "running". Rebuild + re-embed the SPA
before running the spec (web-embed staleness note in memory).

### M2 — Translate the internal words out of the interface
Sweep `ui/web/src/App.tsx` and fix each leak (all confirmed in this tree):
- "kits expand into packages for this agent" — `App.tsx:2145` (the kit modal).
- "topic" tooltip `title="live activity across every agent and topic"`
  (`App.tsx:1383`) and the raw `topic` string rendered in the signals feed
  (`App.tsx:2266`).
- raw correlation id in a hover tooltip `title={\`correlation ${m.corr}\`}`
  (`App.tsx:2235`).
- "approved" printed as a capability status badge (`App.tsx:225` `unshift(
  'approved')`, `:207`, `:1554` "Installed is not the same as approved/running",
  `:1660`). Translate to plain trust wording ("allowed" / "on" / "waiting for
  you"), consistent with [../layering.md](../layering.md) and the config model's
  "there is exactly one situation where a review step still belongs".
- any remaining bare "session" in chrome (should move to a tooltip/detail line
  per chat-conversations.md).

**Acceptance:** a text scan of the built, user-visible SPA strings finds none of
"kit", "package", "topic", "correlation", or "approved" used as interface chrome
(builder-facing help text and tooltips included). Capture this as `ui.spec.mjs`
assertions on the specific surfaces above (the kit modal, the signals nav
tooltip, the message tooltip, the capability badges) so it does not regress.
Where a concept genuinely needs a name, use the product word.

### M3 — Cost honesty: soft limit ≠ hard cap, and the estimate is real
Journey 03 wants a distinct label set: **hard cap**, **soft limit**, **estimate**,
**unknown**. Today `costSummary` (`ui/web/src/App.tsx:238`) lumps everything into
"hard caps": run-step count (`:243`), and the throttle's tokens-per-hour /
max-concurrent (`:246-247`). A token-per-hour throttle *slows* an agent; it is a
**soft limit**, not a hard activation cap. Split them:
- **hard cap** = the run-step limit (it truly bounds one activation's loop);
- **soft limit** = throttle (tokens/hour, max concurrent).

And connect the **estimate**: the per-session dollar estimate already exists in
the runs view (`CodeSessions.tsx`, `/api/estimate/{session}`, `src/estimate.rs`).
The setup cost fold (`App.tsx:1531-1540`) currently says only "Dollar estimates
are not shown until provider pricing is known" — true when pricing is absent, but
it never points at the estimates that *do* exist when pricing is present. Make the
fold honest: show/link the estimate when available, say "unknown" (not a fake
number) when not, and keep hard cap and soft limit visually separate (journey 03
acceptance: "the UI visually separates hard limits from estimates").

**Acceptance:** with a throttle and a run-step limit both configured, the cost
panel renders the run-step limit under "hard cap" and the throttle under "soft
limit" as distinct groups; with a priced model and a recorded session estimate,
the panel shows/links the dollar estimate labeled "estimate"; with no pricing it
shows "unknown", never `$0`. A `ui.spec.mjs` assertion covers the hard/soft split;
reuse the existing `costSummary` unit shape if a unit test is cheaper for the
label logic.

## Read these first
- The why: [../journeys/04-risk-and-trust.md](../journeys/04-risk-and-trust.md)
  (installed/approved/running/failed; the status card), [../journeys/03-cost-
  visibility.md](../journeys/03-cost-visibility.md) (the honest label set),
  [../layering.md](../layering.md) (the vocabulary rule).
- Liveness source: `src/dispatcher.rs:546` (`status_event` →
  `obs/package/<name>/status`, retained; call sites at :361/:516/:389/:533).
- The cost surfaces: `ui/web/src/App.tsx:238` (`costSummary`), `:1531-1540` (the
  fold); `ui/web/src/CodeSessions.tsx` (the live estimate render);
  `src/estimate.rs`, `src/web.rs:793` (`estimate_report`).
- The vocabulary leaks: `ui/web/src/App.tsx` lines listed in M2.

## Log
- 2026-07-01 — Created from the 2026-07-01 vision-drift recon. Two brief premises
  corrected against the worktree: (1) there is no `run/actors.json` — liveness is
  the retained `obs/package/<name>/status` event (`dispatcher.rs:546`), so M1
  wires the bus/ledger status, not a file; (2) the per-session dollar estimate
  already renders in `CodeSessions.tsx`, so M3 is about the *setup cost fold* in
  `App.tsx` (the hard/soft mislabel + honestly surfacing the existing estimate),
  not about wiring estimation from scratch. Vocabulary leaks all confirmed at the
  cited `App.tsx` lines.
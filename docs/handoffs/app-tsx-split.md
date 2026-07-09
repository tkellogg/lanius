---
status: in-progress
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: split App.tsx into per-surface files (H0, extraction only)

`ui/web/src/App.tsx` is 2786 lines holding eight view surfaces plus all the
shared helpers. Four follow-up handoffs (chat-liveness, package-truth,
helper-first-encounter, chrome-polish) each need to edit a different surface;
splitting first puts them in disjoint files. The repo already has the target
pattern: one file per surface (`CodeSessions.tsx`, `CommsView.tsx`,
`ProvidersView.tsx`).

**This is a mechanical extraction. Zero behavior change. Zero copy change.
Zero markup change.** If you find a bug while moving code, note it in the log
and leave it exactly as it was — later handoffs own the fixes.

## Dependency edges

- Runs after web-embed-freshness (H1). Gates chat-liveness, package-truth,
  helper-first-encounter, chrome-polish — none of them start until this is
  committed with the full e2e suite green.

## Read these first

1. ui/web/src/App.tsx — the whole file, before moving anything.
2. ui/web/src/CodeSessions.tsx and CommsView.tsx — the existing one-file-
   per-surface pattern (default-exported component, props in, no own copy of
   shared helpers).
3. ui/web/test/ui.spec.mjs — the gate. It selects by DOM ids/classes
   (`#view-converse`, `#compose-input`, `.cfg-package-card`, …), which is why
   a faithful extraction passes untouched.

## Wonky bits / decisions

1. **App.tsx stays the state owner.** Every `useState`/`useRef`/`useEffect`,
   all loaders (`loadSetup`, `loadConfigure`, `loadConversations`,
   `submitCompose` at App.tsx:1444, `onLiveMessage` at App.tsx:1330, …), the
   routing block (App.tsx:21-85 moves to a module but `navigate`/`applyRoute`
   stay), and the top-level render tree stay in App.tsx. Only the pure,
   prop-driven view functions and module-level helpers move. They are already
   pure — no view function below touches App state except through props.
2. **Named moves only — no renames, no signature changes, no prop
   reshuffles.** Every extracted component keeps its exact props (yes, even
   the `: any` bags) and its exact JSX. Cleanup is out of scope.
3. **`AgentChip` is shared** (used by ConverseView and Nav) — it goes to
   `components/AgentChip.tsx`, not into either view file.
4. **`DetailsBlock` stays with SessionsView** for now (its only consumer);
   chrome-polish will lift it later. Do not pre-lift.
5. **The routing types/functions (`Sel`, `AgentTab`, `selToPath`,
   `pathToSel`, `TAB_TO_SEG`, `SEG_TO_TAB`) move to `routing.ts`** and stay
   exported — `App.tsx` re-exports `Sel`/`AgentTab` if anything imports them
   from App today (grep first).
6. **No barrel files, no index.ts.** Direct imports, matching the existing
   style.
7. **e2e staleness trap:** if web-embed-freshness (build.rs) is already on
   this branch, a plain `cargo build` re-embeds. Verify that is true before
   trusting a green run (the build output shows the vite build running);
   if it is not, do the manual ritual: `cd ui/web && npm run build && touch
   src/web.rs && cargo build` BEFORE running ui.spec.mjs.

## The split (current-line anchors, pre-split)

New view files under `ui/web/src/views/`:

| File | Moves (App.tsx lines today) |
|---|---|
| `views/Nav.tsx` | `Nav` (1720-1795) |
| `views/WelcomeView.tsx` | `WelcomeView` (1796-1828) |
| `views/SetupView.tsx` | `SetupView`, `CodingAgentCatalogCard`, `SetupKit`, `SetupPackageConfig`, `ProposalCard` (1829-2165) |
| `views/ConfigureView.tsx` | `ConfigureView`, `ContextStageTile`, `ConfigInputRow`, `PackageTree`, `PackageCard`, `KitModal`, `KitAddRow` (2166-2600) |
| `views/ConverseView.tsx` | `ConverseView`, `AskMessage` (2602-2729) |
| `views/RailView.tsx` | `RailView` (2731-2749) |
| `views/SessionsView.tsx` | `SessionsView`, `Transcript`, `DetailsBlock`, `TranscriptMsg` (2751-2786) |

New shared modules under `ui/web/src/`:

| File | Moves |
|---|---|
| `routing.ts` | `Sel`, `AgentTab`, `TAB_TO_SEG`, `SEG_TO_TAB`, `selToPath`, `pathToSel` (21-85) |
| `components/AgentChip.tsx` | `agentHue`, `AgentChip` (94-107) |
| `lib/format.ts` | `arr`, `csv`, `shortTs`, `timeOf`, `relativeTime`, `summarize`, `conversationLabel`, `uid`, `firstSentence`, `shortList` |
| `lib/conversation.ts` | `agentOf`, `newWebConversationId`, `conversationStorageKey`, `convMessageKey`, `mergeConvMessages`, `sessionFromPayload`, `isWorkerAgentName`, `isWorkerSessionId`, `codingAgentNames`, `topicFilterMatches` |
| `lib/packages.ts` | `packageSource`, `kitNameFor`, `packageDescription`, `actorDetail`, `packageBadges`, `grantState`, `livenessState`, `riskBadges`, `capabilityOutcome`, `declaredConfigParams`, `packageHasAgentScopedSettings`, `tomlDisplayValue`, `parseConfigRows`, `configRowMap`, `displayConfigValue`, `valueSourceLabel`, `effectiveConfigValue`, `prunedSet` |
| `lib/cost.ts` | `costSummary`, `autonomyConsequence`, `modelCostHint` |

Move each function's attached comments WITH it — several are load-bearing
design records (e.g. the `convMessageKey` dedup rationale at App.tsx:140-152,
the cost-honesty note at 305-309).

## Milestones

### M1 — shared modules out

Extract `routing.ts`, `lib/*`, `components/AgentChip.tsx`; App.tsx imports
them. No view moves yet.

**Acceptance:** `npx tsc --noEmit` (or the vite build) clean; full
ui.spec.mjs green against a re-embedded binary; `git diff --stat` shows
App.tsx shrinking by roughly the moved-line count and no other view file
changed.

### M2 — views out

Extract the seven view files; App.tsx render tree imports them.

**Acceptance:** full ui.spec.mjs green against a re-embedded binary
(all ~306 e2e assertions); App.tsx contains no `function <ViewName>`
definitions below the `App` component; every new file default-exports one
surface; zero changes to any string literal, id, or className anywhere in
the diff (spot-check with `git diff -w` — the diff should be pure moves +
import lines).

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Boundary decision: App keeps
  all state + loaders; the shared health hook later handoffs use is created
  by chat-liveness (its first consumer), NOT here.

---
status: planned
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: the package list tells the truth (H3)

The 2026-07-08 walkthrough hit the configure tab's package list as a wall of
compounding confusions (docs/journeys/06-configuration.md, "The package list
has to tell the truth, fast"): every installed package shows "enabled"
regardless of whether this agent's harness would ever load it; the history
row said "enabled" while the history pane said unreachable with no button to
fix it; enable/disable is buried inside the expanded card; rows carry no
plain-language explanation; the word "instance" leaked onscreen; and `echo`
reads as demo cruft in a shipped catalog.

Root cause of the big lie: "enabled for this agent" is derived ONLY from
`skills.include`/`skills.exclude`, and include defaults to match-everything
(App.tsx:1019-1024, `include: '#'` at 472). Installed-on-path ⇒ shown
enabled, always.

## Dependency edges

- Requires app-tsx-split (H0) — edits `views/ConfigureView.tsx` post-split —
  and chat-liveness (H2) — consumes its `useSystemHealth` projection.
- Parallel-safe with helper-first-encounter and chrome-polish AFTER the
  planner's route-stub prep commit (see wonky bit 6).

## Read these first

1. docs/journeys/06-configuration.md — the judging document, especially "The
   package list has to tell the truth, fast" and the Lily/Daniel/Ganesh
   journeys (altitude: essentials first, plain words, an off switch).
2. Post-split `views/ConfigureView.tsx`: `PackageTree` + `PackageCard`
   (pre-split App.tsx:2475-2577), `kitNameFor`/`packageSource` (pre-split
   App.tsx:175-190), `livenessState` (268-276), `grantState` (255-261).
3. src/web.rs: the packages relay (1296-1305 area, CLI `packages --json
   --profile`), `liveness` (446-462), the admin `approve` verb (1193-1204 —
   `cli(root, &["approve", pkg, "--by", "ui"])`), history reachability
   (1064-1074, reads run/pkg-history/http.json).
4. src/dispatcher.rs:344-430 `tick_actors` — what "running" actually means
   (see spike verdict below).
5. docs/journeys/16-the-helper.md — the helper's package set is a symptom of
   this surface's dishonesty (context only; the helper itself is H4).

## Spike verdict (2026-07-08, planner): what a "start" button can honestly do

The dispatcher's boot loop spawns **every discovered daemon-mode package
unconditionally** each tick (dispatcher.rs:408-430; the comment at 344-348:
"Discovery boots them… capabilities attach live via the ledger"). Approval
does not gate the spawn — it gates what the running actor is *allowed to do*
(grants ledger). Therefore:

- **"Parked" (unapproved) is repairable from the web today**: POST
  /api/admin/approve {package} exists (web.rs:1193, decided_by=ui). The row's
  repair button for a needs-review package is REAL — wire it to that route.
- **"Not running because the background service itself is down" is NOT
  web-repairable**: no primitive starts the dispatcher process from the web
  layer, and we will not invent a kernel special case for it. The honest
  UI is the truth + the command: "the background service isn't running —
  start it with `lanius daemon`".
- **Named residual (follow-up handoff, not this one):** a proper
  start/restart-the-service affordance as a package + permission.

Refinement (planner, same day): the spawn is unconditional, but the BROKER
enforces grants per package actor (src/broker.rs:296, 333 — a
supervisor-minted actor is grant-scoped; unapproved subscribe/publish are
denied). So "parked" = process alive, capabilities denied — approval
un-parks it live, consistent with web.rs's "approve the package if it is
parked" copy. Also: history is a PROTECTED stdlib package — `lanius revoke`
refuses it without `--force` (src/main.rs:312, 1538; src/kit.rs:344
`protected_packages`).

**Pre-impl verification RESULT (planner ran it 2026-07-08 in a /tmp
scratch root; this is the ground truth M2 is written against):**

1. `lanius revoke history` refuses (protected); `--force` revokes. The
   RUNNING actor keeps serving — parked only manifests after the actor
   restarts (observed after a daemon restart: /api/history → "history view
   unreachable — approve the history package if it is parked").
2. **Approve does NOT repair a revoked package.** `packages::decide` flips
   only `requested`→`approved` rows (src/packages.rs:505-506); revoke
   leaves rows at `revoked`, which is TERMINAL — `sync` never re-requests
   under the same manifest hash (packages.rs:331 "Leave its state alone").
   Observed: POST /api/admin/approve after a force-revoke → "history:
   nothing requested", still parked. NO web or CLI path re-enables a
   revoked package short of a manifest-hash change. **Second named
   residual: a sanctioned re-request/re-enable primitive (today Ganesh can
   turn a thing off, and nothing turns it back on).** The UI must tell
   this truth for `revoked` rows — no fake button.
3. **Approve DOES repair the needs-review case, live.** A package with
   `requested` grants (e.g. `recent-history` in a fresh init) flips all
   rows to approved via POST /api/admin/approve with the daemon running —
   observed: 3 grants approved, decided_by=ui, no restarts.
4. Side observation for the "running" column: in the scratch stack,
   /api/liveness reported `actors: {}` even with package daemons up
   (possibly a retained-status replay gap around web-relay/daemon restart
   ordering). Implementer: check the running column against a real stack
   EARLY; if liveness is empty there too, show "status unknown" honestly
   rather than "not started" for a running actor, and report the finding.

## Wonky bits / decisions (already made)

1. **"Enabled" decomposes into three visible facts per collapsed row, plus a
   fourth presentational one:**
   - **installed** — on this agent's package path (what the list already is);
   - **allowed here** — the skills.include/exclude verdict (the current
     "enabled"), shown honestly: when it's true only because include
     defaults to match-everything, the row says "on by default", not a
     green light that implies a choice was made;
   - **running** — for daemon/http packages only, from `useSystemHealth`'s
     actor status (H2's projection over /api/liveness) — running / failed /
     not started, the same words `livenessState` already produces;
   - **applies to this harness** — presentation ONLY, no enforcement: when a
     package is harness-specific (e.g. `harness-codex`) and this agent's
     harness is a different one, the row says why it's here and that this
     agent won't load it, instead of implying it's active. Compute this from
     the package's own declared metadata (manifest); do NOT hard-code
     package names in the UI, and do NOT touch kernel/dispatch behavior.
2. **Toggle on the collapsed row.** The enable/disable button moves from the
   expanded body (pre-split App.tsx:2572) to the `<summary>` row, same
   `skills.exclude` write as today (`setSkillExcluded`). The kit-group
   enable-all (2487) stays.
3. **Pane agreement + the repair affordance.** The history package row and
   the sessions tab must derive from the SAME health projection. On the row
   (and mirrored in the sessions tab's error state, pre-split App.tsx:1482):
   - needs review (grants `requested`) → button "allow and start" → POST
     /api/admin/approve → refresh; capabilities attach live (verified in
     the pre-impl experiment, no restarts needed).
   - revoked (grants `revoked`) → NO button (approve is a no-op here —
     pre-impl finding 2). The truth in plain words: "this was switched
     off; switching it back on isn't supported yet" (+ the residual is
     already named for a follow-up). `grantState` (lib/packages.ts)
     currently buckets revoked into its fallthrough — surface it
     distinctly.
   - approved but not running / unreachable → the truth + the command
     (spike verdict): "the background service isn't running — start it
     with `lanius daemon`". No fake button.
   - running → nothing to repair; the row says running.
4. **A plain-language one-liner per row.** `packageDescription` (206-216)
   already tries; tighten the fallbacks so every row answers "what does
   enabling this do *for this agent*" in the user's words. No jargon:
   "resident actor on the bus" becomes something a person would say (e.g.
   "runs in the background and answers on its own").
5. **Retire "instance" as onscreen copy — CAREFULLY.** `kitNameFor`
   (pre-split App.tsx:184-190) returns the literal string `'instance'` as a
   **grouping key** consumed by `PackageTree` (2479) and as the source label
   tooltip (2571). **Warning, verbatim from planning: this is a display-label
   rename only — the grouping key stays untouched (or is renamed consistently
   with a grep for every consumer). A blind find-replace of "instance" will
   break package grouping.** Onscreen the word becomes "this installation"
   (or is dropped where a tooltip suffices), per 06-configuration.md.
6. **Server routes are pre-carved.** The planner's prep commit on the sprint
   branch adds the route stub this handoff fills (a status/repair-shaped
   addition if any new route is needed at all — the approve route already
   exists, so expect ZERO new server routes; the stub exists to keep web.rs
   merge-clean against chrome-polish's /api/code/deliver). Do not add other
   routes.
7. **Echo out of the default seed.** Drop `"echo"` from the approved-seed
   loop in src/initcmd.rs:705 (`["chat", "echo", "notify", "watchdog"]`).
   The files still ship (initcmd.rs:30-36 embed stays); a fresh init just
   doesn't approve/surface it. No `--examples` kit this sprint.
8. **Simple-core law:** every distinction above is computed from what the
   package declares plus the grants ledger plus liveness — never from a
   hard-coded package-name list or a magic string in the kernel.

## Milestones

### M1 — the three-facts row + collapsed toggle

Wonky bits 1-2. Collapsed `PackageCard` summary shows name, one-liner,
installed/allowed/running facts, and the toggle; expanded body keeps
settings/badges as today.

**Acceptance (real app, re-embedded binary):** on a claude-code agent,
`harness-codex`'s row visibly says it won't be loaded by this harness (and
why it's installed), with a one-click disable on the collapsed row; a
match-everything-default row reads "on by default", not "enabled"; toggling
from the collapsed row round-trips skills.exclude (verify via the raw TOML
panel or `lanius profile show`); full ui.spec.mjs green.

### M2 — pane agreement + honest repair

Wonky bit 3, both directions (row ↔ sessions tab).

**Acceptance (restated to the pre-impl experiment's observed truth):**
- Needs-review repair, end-to-end: in a fresh scratch root (where
  `recent-history` has `requested` grants), its row shows "needs review"
  with the allow button; clicking it approves via POST /api/admin/approve
  (observe decided_by=ui rows flip) with no CLI touch and the row updates.
- Parked history, honest both ways: with history force-revoked AND the
  actor restarted (the reproduction: `lanius revoke history --force`, then
  restart the daemon), the sessions tab AND the history package row show
  the SAME degraded state; the row does NOT offer approve (it is a no-op
  for revoked rows) — it says plainly that this was switched off and
  re-enabling isn't supported yet.
- Dispatcher down: both surfaces show the `lanius daemon` message and NO
  button pretends otherwise.

### M3 — copy: one-liners, "instance", echo seed

Wonky bits 4, 5, 7.

**Acceptance:** `grep -rn "instance" ui/web/src` shows no user-visible
render of the word (code identifiers/grouping keys may remain);
`grep -n "echo" src/initcmd.rs` shows it gone from the decide loop at ~705
and cargo test green (fix any init test expecting 4 seeds); a fresh
`lanius init` root's package list shows no echo row.

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Spike run same day: approve
  is web-real, dispatcher-start is not — honest fallback + named residual.
- 2026-07-08 — pre-impl experiment run by the planner (scratch root,
  daemon+web live): revoked-is-terminal discovered (approve only flips
  requested rows); M2 restated to the observed truth; second residual
  named (re-enable primitive); liveness actors:{} side-observation logged
  for the implementer to check early.

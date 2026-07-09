---
status: in-progress
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: chrome polish — words, contrast, activity, worker deliver (H5)

Four smaller walkthrough items, one handoff, all judged against
docs/journeys/ui-preferences.md (vibe: professional, functional contrast,
no weekend-hack-job tells) and 07-chatting.md ("Talking to a coding
session"):

1. **Settings is a word, not a bare gear.** The configure tab is a lone ⚙
   IconButton in the agent tab strip (pre-split App.tsx:1575), with more
   bare ⚙ at 2657 and 2674, and the nav "runs" entry uses a ⚙ sigil too
   (1753) — the same symbol meaning two different things.
2. **Dark-mode contrast hierarchy.** Text must outrank buttons/chrome.
   Dark is the :root default (ui/web/src/styles.css:5) with the light
   override at :71; button styling spread around styles.css ~180, 192, 311,
   483, 493, 512, 526, 796, 1033.
3. **Activity rows expand.** RailView (pre-split App.tsx:2731-2749) renders
   one summarized line per event with NO way to see the full payload; the
   disclosure pattern already exists as `DetailsBlock`
   (App.tsx:2776-2778, used only in transcripts).
4. **Say something to a live worker.** The walkthrough instinct: Tim saw
   his Claude Code worker appear and immediately wanted to message it. The
   CLI has this (`lanius code deliver <worker-session> "<msg>"`,
   src/main.rs:1652-1661 → codeagent::deliver); the UI does not.
   CodeSessions.tsx is read-only.

## Dependency edges

- Requires app-tsx-split (H0) and chat-liveness (H2, ordering only — this
  handoff runs in the parallel phase after H2 is committed).
- Parallel-safe with package-truth and helper-first-encounter: touches
  styles.css, views/RailView.tsx, CodeSessions.tsx, Nav/tab-strip chrome,
  and ONLY the pre-carved /api/code/deliver stub in web.rs.

## Read these first

1. docs/journeys/ui-preferences.md and the "Talking to a coding session"
   section of docs/journeys/07-chatting.md — the judging documents.
2. ui/web/src/CodeSessions.tsx — the worker surface this extends.
3. src/codeagent.rs:481-530 (`recognize_delivery`) and src/main.rs:1652-1661
   — what deliver actually does (mailbox → resume).
4. The SECURITY NOTE below before touching anything near the chat
   projection.

## SECURITY NOTE (hard constraint)

The server keeps worker sessions OUT of the chat/conversation projection
(src/web.rs:671 and the transcript-side filter ~2850): `code-*` session
classification is security-adjacent — the `code-` prefix is load-bearing in
the broker's SECURITY CORE (src/broker.rs:440). **Do not weaken, bypass, or
special-case these filters.** The deliver affordance lives on the
code-session surface and goes through the deliver path; it must NOT write
into, or surface worker sessions inside, the chat projection. Workers are
work-you-observe with a sanctioned "say something" — not peer conversations.

## Wonky bits / decisions (already made)

1. **One meaning per symbol.** The configure tab renders as the word
   ("settings" — lowercase, matching the tab strip's converse/History/
   Activity casing convention; pick consistently) instead of a bare ⚙. The
   in-view shortcut gears (2657, 2674) get visible text or go — an icon may
   stay only WITH the word. The nav "runs" entry stops using ⚙ (it is not
   settings); give it a distinct sigil or none. Grep for every `⚙` in
   ui/web/src when done — each survivor must sit next to the word.
2. **Contrast: tokens, not spot fixes.** Adjust the dark palette variables
   (styles.css :root at 5) so body text is the highest-contrast element and
   buttons/chrome sit below it — do NOT chase individual button rules with
   overrides. Keep the existing brand discipline (the red thorn stays the
   loudest thing; agent chips stay a whisper — see the comment at pre-split
   App.tsx:94-98). Verify light theme (styles.css:71) still holds the same
   hierarchy. Sanity gate: normal body text ≥ WCAG AA (4.5:1) against its
   background in both themes; button labels readable but visually
   subordinate.
3. **Activity rows: same summary line, click to expand.** Each RailView row
   becomes a disclosure: collapsed = exactly today's one-liner (time, topic,
   `summarize()` payload); expanded = pretty-printed full JSON payload
   (lift/adapt `DetailsBlock`). Requirements: expanding must not fight the
   live feed (new rows keep appending; an expanded row keeps its content —
   note rows currently key by buffer index, pre-split App.tsx:2745, which
   breaks identity as the buffer slides; key by `env.id`/event identity
   instead), and collapsed rendering stays cheap (600 rows).
4. **Deliver = one new route, shelling the CLI.** `POST /api/code/deliver`
   {session, message} → validates the session id shape (`code-*`,
   `isWorkerSessionId` pattern) → shells `lanius code deliver <session>
   "<message>"` (the same relay-through-CLI pattern the admin routes use,
   src/web.rs:1081+ `cli(root, …)`). The route stub is pre-carved on the
   sprint branch by the planner — fill it, don't move it. UI: a small
   compose on the CodeSessions DETAIL surface (visible when a session is
   focused), labeled honestly — "send a note to this worker" — with the
   observe-vs-converse distinction stated right there in one line ("this is
   a running job, not a chat; your note is delivered to its inbox").
   Feedback on send: accepted/failed from the CLI exit — no fake delivery
   promises (same honesty rule as chat-liveness).
5. **Language rules:** no "session" in user-facing copy on the new compose
   ("worker" / "this run"); never "instance".

## Milestones

### M1 — settings word + gear cleanup

**Acceptance:** the agent tab strip shows the word for configure; ui.spec.mjs
selectors updated where they keyed on the gear; `grep -n "⚙" ui/web/src`
survivors each accompanied by visible text; runs nav no longer uses ⚙.

### M2 — dark contrast pass

**Acceptance:** a contrast check (manual measurement of the main
text/button/background token pairs is fine — record the ratios in the log)
shows text ≥ 4.5:1 and buttons visually subordinate in BOTH themes; no
per-button one-off overrides added; full ui.spec.mjs green.

### M3 — activity disclosure rows

**Acceptance (real app):** clicking an activity row expands the full JSON
payload; collapsed rows look as today; with the feed live, an expanded row
neither collapses nor changes content as new events append (event-identity
keys); pause still works.

### M4 — worker deliver

**Acceptance (real app, re-embedded binary):** spawn a real worker
(`lanius code claude-code --headless "…"` or the e2e fixture), focus it in
the runs view, send a note → the note reaches the worker (verify via the
worker's inbox/mailbox events on the bus or the worker acting on it);
a non-`code-*` session id is rejected 400 server-side; the chat projection
shows NO trace of the note as a conversation (web.rs:671 filter untouched —
assert in the verify, and diff-check no edits landed in broker.rs or the
projection filters); full ui.spec.mjs green.

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Deliver goes through a
  pre-carved /api/code/deliver stub shelling the CLI; chat-projection and
  broker filters are explicitly out of bounds.

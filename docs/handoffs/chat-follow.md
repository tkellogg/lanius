---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: the chat follows the latest message (and one verify-only check)

Two small items from Tim's backlog ([../_questions.md](../_questions.md)), one
handoff. (a) "In UI have the screen automatically follow the latest message by
default, i.e. scroll to the bottom unless the user intentionally scrolls away.
Like a normal chat app." Today the converse feed has **zero scroll handling** —
a grep of `ui/web/src/App.tsx` for `scrollTop`/`scrollIntoView`/`scrollTo`
returns nothing — so a long conversation leaves you parked wherever the browser
happens to be while new replies land below the fold. (b) A verify-only pass on
the model-config error link: "provider list unavailable … I should just have a
link to click to get me to where model providers are setup." Grounding says
model-providers M4 **already built and asserted this** — the milestone is to
verify it holds and close the `_questions.md` item honestly, not rebuild it.

## Wonky bits / decisions to confirm

1. **Pin to `.conv-feed`, not the holder.** The scrollable element is
   `.conv-feed` (`overflow-y: auto`, `ui/web/src/styles.css:373`), rendered at
   `App.tsx:2407`. Its parent `#conv-holder` (`App.tsx:2404`,
   `.conv-feed-holder` `styles.css:297`) also contains the sprint-2 branch
   **origin chip** (`#conv-origin`, `App.tsx:2400-2406`) as a *sibling above*
   the feed — so as long as all scroll math targets `.conv-feed`, the chip's
   height never enters the calculation and reply-branching is untouched.
   *Fable: confirm `.conv-feed` as the one scroll surface.*

2. **"Pinned" is derived state, re-evaluated on user scroll — with a
   tolerance.** The classic chat contract: pinned by default; a deliberate
   scroll *up* unpins; scrolling back to (near) the bottom re-pins. "At bottom"
   must be a tolerance check (`scrollHeight - scrollTop - clientHeight < ~40px`),
   not equality — sub-pixel zoom and image/markdown reflow make exact equality
   flap. The trap to avoid: the pin effect itself sets `scrollTop`, which fires
   a `scroll` event — the handler must not read that programmatic scroll as
   "the user scrolled up" (either a "we're mid-programmatic-scroll" flag, or
   re-deriving pinned from position works out naturally since a programmatic
   scroll lands at the bottom). *Fable: confirm the position-derived approach
   (no flag needed if the pin always lands within tolerance) over an explicit
   suppress-flag.*

3. **SSE re-renders and reconnect replay must not steal the scroll.** New
   messages arrive via the live stream (`openLiveStream`, `App.tsx:574` →
   `onLiveMessage` `:1120` → `addConv` `:1149`), which replaces the whole
   `conv` Map (`useState(new Map())`, `App.tsx:464`) and re-renders
   `ConverseView` (`:2327`) on every append. The follow effect must key on the
   messages actually changing (last message id / length), not on every render.
   The live stream carries a monotonic `seq` (`ui/web/src/live.ts:12-15`,
   `EventSource` at `:69`) and reconnects can replay — replayed messages merge
   by id (`mergeConvMessages`, `App.tsx:102`), so keying on the *last message
   id* means a replay that appends nothing new doesn't yank the scroll.
   *Fable: confirm keying the effect on last-message identity.*

4. **Switching conversations resets to pinned.** Opening a different
   conversation or a branch (`openConversation`, `newConversation` `:666`,
   `startBranch` `:698`) should land you at the bottom, pinned — the "where
   were we" scroll-position-per-conversation memory is more machinery than
   this earns. State it plainly as a non-goal. *Fable: confirm.*

5. **The Q4 item is already built — verify, don't rebuild.** The prompt's
   pointer to `ui/web/src/primitives.tsx:~1897` is stale; the real component is
   `ui/web/src/components/primitives.tsx` — `ModelField` `:66`, and the
   empty-list/error branch (`:85-96`) **already renders** the "set up a
   provider →" link (`data-providers-link`, `onClick={onSetupProvider}`)
   *precisely in the error state*, wired from both the new-agent setup
   (`App.tsx:1643`) and configure (`App.tsx:2005`) to `selectProviders`
   (`:617`). And `ui.spec.mjs:2306-2313` already asserts the link renders in
   the empty state **and navigates** to `#view-providers`. So M2 is: run the
   verification, confirm the assertion covers the error state (it does — the
   link only exists inside the `!list.length` branch), and mark the
   `_questions.md` item answered. Only if verification finds a hole (e.g. the
   configure-view instance untested) does code change — one added spec
   assertion, nothing more. *Fable: confirm verify-only scope.*

**Product language.** "New messages ↓" (or similar plain words) on the jump
chip; no "SSE", "seq", "pinned" in the interface ([../layering.md](../layering.md)).

## Milestones

### M1 — Follow-the-conversation scrolling + the jump chip
In `ConverseView` (`ui/web/src/App.tsx:2327`):
- a `useRef` on `.conv-feed` (`:2407`) + a `scroll` listener deriving
  `pinned` from position with the tolerance (wonky bit 2);
- an effect keyed on the last message id (wonky bit 3): when pinned and a
  message appends, scroll to bottom; when unpinned, leave the scroll alone and
  show a **"new messages ↓" chip** (a small floating button over the feed,
  `data-sel="conv-jump"`) that scrolls to bottom and re-pins on click;
- reset to pinned-at-bottom on conversation switch (wonky bit 4);
- the chip hides whenever pinned (including after the user manually returns to
  bottom).

**Acceptance:** `ui.spec.mjs` — seed a conversation tall enough to scroll;
(a) with the view at the bottom, a new live message leaves the feed scrolled to
the bottom (assert `scrollTop + clientHeight ≈ scrollHeight` on `.conv-feed`);
(b) scroll up deliberately, deliver another message: the scroll position does
NOT move and `[data-sel="conv-jump"]` appears; (c) click the chip: the feed is
at the bottom, the chip is gone, and a further message keeps it pinned;
(d) manually scrolling back to the bottom (no chip click) also re-pins;
(e) a branched conversation opens pinned at the bottom with the origin chip
(`data-sel="conv-origin"`) still rendered above the feed. Rebuild + re-embed
the SPA before running (web-embed staleness note in memory).

### M2 — Verify the provider-setup link in the error state (no planned diff)
Run the existing verification: build, launch on a scratch root with no
provider configured, confirm the model field's error state shows "provider
list unavailable — type a model id or **set up a provider →**" and the link
lands on the Providers page; run `ui.spec.mjs` and confirm the `:2306-2313`
assertions pass, and that the **configure view's** `ModelField`
(`App.tsx:2005`) gets the same link (the existing spec exercises the new-agent
setup view at `#view-setup`). If the configure-view instance is unasserted,
add that one assertion; otherwise this milestone changes nothing and closes
the `_questions.md` item in this handoff's Log.

**Acceptance:** the ui.spec run is green including the provider-link
assertions; either a note in the Log ("verified, both views covered, no change
needed") or a single added assertion covering the configure view. Nothing else.

## Read these first
- The feed being scrolled: `ui/web/src/App.tsx` — `ConverseView` `:2327`,
  `#conv-holder` `:2404`, origin chip `:2400-2406`, `.conv-feed` `:2407`,
  message map `:2409-2410`; `ui/web/src/styles.css:373` (`.conv-feed` is the
  `overflow-y: auto` element), `:297` (the holder).
- The message flow that triggers re-renders: `App.tsx:574` (`openLiveStream`),
  `:584` (`kind === 'message'`), `onLiveMessage` `:1120` (`addConv` `:1149`),
  `conv` state `:464`, `mergeConvMessages` `:102`, `messages` prop `:1359`;
  `ui/web/src/live.ts:35-73` (the `/api/stream` EventSource + `seq`).
- The sprint-2 work not to break: [reply-branching.md](reply-branching.md)
  (the origin chip + `startBranch` `App.tsx:698`), [html-messages.md](html-messages.md)
  (Markdown rendering can reflow late — another reason for the tolerance).
- The already-built Q4 target: `ui/web/src/components/primitives.tsx` —
  `ModelField` `:66`, error branch `:85-96` (`data-providers-link`);
  `App.tsx:617` (`selectProviders`), `:1643`, `:2005`;
  `ui/web/test/ui.spec.mjs:2306-2313` (the existing assertions);
  [model-providers.md](model-providers.md) M4.
- The wording rule: [../layering.md](../layering.md).

## Log
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull. Grounded
  against the worktree: the converse feed has no scroll handling anywhere in
  `App.tsx` (greenfield), the scrollable element is `.conv-feed` with the
  branch-origin chip as a sibling *outside* it, and SSE appends replace the
  whole `conv` Map per message. The Q4 provider-link item turned out **already
  implemented and spec-asserted** (model-providers M4 landed it in the error
  branch of `ModelField` with a navigation test), so M2 is verify-only.
  Judgment calls for Fable: pin to `.conv-feed` (1); position-derived pinned
  state with a tolerance, no suppress-flag (2); follow-effect keyed on
  last-message id so SSE replay doesn't yank (3); conversation switch resets
  to pinned (4); Q4 verify-only (5).

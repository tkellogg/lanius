---
name: UX Overhaul
description: Output from a UX review
status: done
---

# Handoff: web UI fidelity pass — contrast, responsive, controls, language, warmth

Status: M1–M6 implemented and verified. `npm --prefix ui/web run test:ui`
runs clean — 137 sub-checks across 10 flows (contrast baseline at boot, the
existing configure/rename/add-ons/converse/history regression suite, plus new
narrow-viewport, a11y, and product-language + identity flows). See the Log at
the bottom for per-milestone decisions and measured ratios.

This handoff is the cross-cutting product-fidelity pass on the web UI (`ui/web`,
Vite + React 19). It is the follow-on to
[configuration-ux.md](configuration-ux.md): that pass fixed the journey-specific
*structure* (altitude, scope, autonomy consequences, the off switch, cost-at-top)
and those landed (M1–M4 verified). What remains is the thin layer that sits on
top of all of it and is currently dragging the whole surface down regardless of
how good the structure is — **contrast, responsiveness, control fidelity,
product language, and companion warmth.**

The through-line: **the hard, persona-specific work is already built; the misses
are a thin cross-cutting layer.** A first-time multi-lens review (live screenshots
of every surface at desktop + narrow, graded against the four characters by four
parallel reviewers — layout/altitude, accessibility, interaction/controls,
visual/persona) found that none of the remaining problems require rethinking the
architecture. The single highest-leverage fix is two CSS variables. After that
it's pickers, responsive rules, ARIA, and copy.

Frame for the owner (Tim): the cockpit aesthetic is good and deliberate — don't
dilute it. The persona-alienation items (language, warmth) ship best as a
**default warm copy + identity layer** with the raw cockpit vocabulary available
behind a power/theme toggle, *not* a re-theme. The contrast and responsive items
are unambiguous bugs that hurt every persona including the owner.

## Read these first

Design intent (read before touching anything):

- `docs/layering.md` — the hard rule: kernel vocabulary (session, grant, stage,
  topic, mailbox, ledger, raw ids) must never appear on the product surface.
  M5 below is largely this rule applied to copy that still leaks.
- `docs/journeys/characters.md` — Tim, Lily, Daniel, Ganesh. Every finding names
  who it hurts; this is the map.
- `docs/journeys/ui-preferences.md` — the two rules that drive M3 and the color
  work: "a text box is almost always the worst choice" (use closed-set pickers /
  provider model-list APIs / file pickers), and the color/aesthetic expectations
  (Tim: any color if contrast suffices; Lily: aesthetics are crucial; Daniel:
  professional, not janky).
- `docs/journeys/07-chatting.md` — the unit of the UI is a **conversation**;
  "session"/raw ids are kernel words; coding runs are observed work in their own
  surface. M5's language work serves this.
- `docs/journeys/03-cost-visibility.md`, `04-risk-and-trust.md` — already
  well-served; don't regress the honest cost labels or the trust footprint.
- [configuration-ux.md](configuration-ux.md) — the predecessor handoff. Its
  Log section records what's done (scope honesty mechanism, essentials-first
  fold, autonomy consequence, agent-cost-at-top, off-switch for linked kits,
  deferred drift). **Do not redo that work**; this handoff only refines the one
  residual (M6: the shared-vs-per-agent save buttons are correct in label but
  visually adjacent and same-weight inside a panel titled "for this agent").

The surfaces you'll change:

- `ui/web/src/styles.css` — the palette and component kit. The color tokens
  (`--ink`, `--dim`, `--faint`, `--agent`, `--work`, `--human`, `--orange`) live
  at the top; the contrast work (M1) is mostly here. The form-vs-cockpit radius
  seam and the multiple primary-button styles (M6) are here too.
- `ui/web/src/App.tsx` — the whole dashboard. Key pieces for this pass:
  `Masthead`/nav (labels "INSTRUMENTS", "AGENT EXPLORER // LIVE"), the agent
  tab row (`converse / sessions / telemetry / configure`), `submitCompose` and
  the compose button ("transmit"), `ConfigureView` essentials + `PackageCard`
  (the residual scope-adjacency), the new-agent wizard (`createAgent`), `RailView`
  (signals empty state), the converse empty state.
- `ui/web/src/CodeSessions.tsx` — the Workers surface ("Coding sessions" / raw
  ids).
- `ui/web/src/components/primitives.tsx` — `IconButton`, tooltip wiring.
- `ui/web/server.mjs`, `ui/web/src/api.ts` — only if M3's model picker needs the
  models endpoint hardened (it already relays `elanus models --json`).

Proof and conventions (keep these honest):

- `docs/ui-flows/configuration.md` — the canonical flow catalog. Any flow you
  change updates here.
- `ui/web/test/ui.spec.mjs` — the Playwright regression suite. Extend it; don't
  break it. New a11y/responsive assertions belong here.
- `.claude/skills/web-qa/SKILL.md` — QA the real UI against an isolated live
  stack; assert durable state, not flashing notes.

Reproduce the review (optional but recommended): a throwaway-stack screenshot
harness (boot daemon + `server.mjs`, seed an agent, screenshot each view at 1440
and 400 px) is how these findings were grounded. Note one gotcha: the app holds a
persistent SSE connection, so Playwright's `networkidle` wait never settles — use
`domcontentloaded`.

## The findings (the why, condensed)

What's already right and must not regress: configure altitude + agent-cost-at-top
+ autonomy consequence; the honest cost labels ("unknown beats fake precision");
the conversation model (explicit "+ new", human labels, optimistic send with the
failure-mail surfaced inline, coding runs in their own surface); the orange-only
alarm discipline and the agent/human/work voice palette; Ganesh's copyable trust
footprint and the linked-kit off switch.

The remaining gaps:

1. **Two color tokens fail WCAG AA and touch nearly every screen (highest
   leverage).** `--dim` (the default secondary-text color) computes to ~3.6–4.1:1
   — below the 4.5:1 floor, and worse on active rows; `--faint` (labels,
   placeholders, meta, footer) is ~2:1, a severe fail. Between them they color the
   section headings, nav labels, all field labels, placeholders, and badges — so
   the *entire* surface reads low-contrast. This is also the root of Daniel's
   "janky" read and Lily's aesthetic miss, not just an a11y line item.

2. **Responsive is broken below ~900 px.** The agent tab strip overflows and
   clips at the right edge on every agent view; configure cost-card text is cut
   off; the sidebar is forced into a ~180 px scroll box that eats the top third
   before any content; the masthead clips "CONNECTED". The app is effectively
   desktop-only today.

3. **Free-text where a closed set exists** (`ui-preferences.md`: "a text box is
   almost always the worst choice"). **Model** is a raw input with only a soft
   datalist in all three places (welcome wizard, setup wizard, configure) — a
   dash/dot typo silently fails on the next run, and when `elanus models --json`
   fails (e.g. no API key) it degrades to a few hardcoded strings with no
   "provider unavailable" signal at the field. **Workdir/paths** are free text
   with no picker or existence check — and "aim it at my repo" is the entire
   reason Daniel showed up; a typo'd workdir silently runs tools in the elanus
   root.

4. **Accessibility beyond contrast.** No visible `:focus-visible` on most
   controls (inputs strip the outline); `role="tablist"` is declared but children
   lack `role="tab"`/`aria-selected`; the live conversation feed is not a
   `role="log"`/`aria-live` region, so a blind user hears nothing when the agent
   replies (and "it's a conversation" is the whole product); hit targets are
   ~18 px (below WCAG 2.2's 24 px); the alarm pulse ignores
   `prefers-reduced-motion` — a flashing channel you can't turn off.

5. **Kernel words and cockpit nouns leak into the product surface.** A **SESSIONS**
   tab with a raw-id column; the Workers surface says "Coding sessions"; the send
   button says **TRANSMIT**; the nav is **INSTRUMENTS**; "what the agent did" is
   **TELEMETRY**. `07-chatting.md`/`layering.md` are explicit that session/ids are
   kernel words and the product speaks the user's language.

6. **Lily has no companion.** An agent is a dim lowercase row — no avatar, no
   per-agent color, no identity in nav / converse header / welcome. She "treats
   agents like pets"; this gives her a process in a console. Single
   highest-leverage warmth change.

7. **"Two people built this" seams (Daniel's bar).** Sharp 2–3 px cockpit radii
   (tabs/nav) vs 6 px rounded web-form inputs; at least three different
   primary-button styles; the autonomy consequence sentence renders twice in one
   configure screenful; the run-step cap number prints twice; welcome has three
   action buttons where two route to the same place and an empty `<p>` gap; the
   Signals view is filter chips over a black void with no empty state; setup is
   one long unfolded scroll where the new-agent wizard has the same weight as
   Ganesh's audit grid.

8. **Scope-honesty residual (refinement of the predecessor's M1).** The "every
   agent" (shared, config-repo) save sits *inside* the panel titled "add-ons for
   **this agent**", same size and adjacent to the "this agent" save. The labels
   are correct; the visual adjacency and shared framing still mislead.

## Plan

Milestones are ordered by leverage and dependency. Each is shippable on its own.
The "shape" bullets are intent + constraints; exact components, copy, and layout
are yours within the guardrails.

### M1 — Contrast and color tokens (highest leverage)

Shape:
- Raise `--dim` to clear AA 4.5:1 on both the page background and the active-row
  background (≈`#9a988c` is a starting point — verify the computed ratio in both
  states, don't eyeball it).
- Stop using `--faint` for any glyph a human must read (labels, placeholders,
  meta, footer): route those to a token that clears AA (≈`#8a897e`+). Keep the
  dark `--faint` value only for non-text hairlines/dividers.
- Sweep every usage so headings, nav labels, field labels, placeholders, and
  badges all clear AA. Don't introduce a third low-contrast token.
- Do not touch the orange alarm discipline or the agent/human/work voices (they
  already pass); only the two failing greys.

Acceptance criteria:
- Body, secondary, label, placeholder, and badge text all clear AA 4.5:1
  (large-text 3:1 where applicable) against their actual backgrounds, in both
  rest and active/hover states. Record the measured ratios for the changed tokens
  in this doc's Log.
- No remaining text uses the old `--faint` value.
- `ui.spec.mjs` (or a small added check) asserts the computed token values; the
  flow catalog notes the contrast baseline.

### M2 — Responsive / narrow (sub-900 px)

Shape:
- Make the agent tab strip wrap or horizontally scroll; nothing clips at 400 px.
- `min-width: 0` on cards so cost-card and configure text can't be cut off.
- Collapse the sidebar into a drawer/disclosure under ~900 px instead of a
  permanently-open 180 px panel that buries content.
- Stack the masthead (or drop the "// LIVE" subtitle and word labels to dots) so
  "CONNECTED" isn't clipped.
- Soften the vignette below 900 px so the phone view doesn't read as
  disabled/broken.

Acceptance criteria:
- At 390–414 px, every surface (welcome, setup, converse, configure, signals,
  workers) is usable with no horizontal clipping of tabs, cards, or status; the
  compose box and primary actions are reachable without horizontal scroll.
- Provable in web-qa at a narrow viewport; `ui.spec.mjs` gains a narrow-viewport
  assertion on the configure tab strip and a converse compose reachability check.

### M3 — Control fidelity (closed-set pickers)

Shape:
- Model field → a real picker (select/combobox) populated from the provider
  model list, with the existing cost/perf hint, and a single "custom…" escape row
  for the Tim case. When the list is empty/unavailable, say "provider unavailable"
  *at the field* (honest, per `03-cost-visibility.md`) instead of silently
  degrading to free text. Apply in all three places: welcome wizard, setup
  wizard, configure essentials.
- Workdir/path fields → a directory picker, or at minimum a server-side
  exists/writable check on blur with an inline error; keep a text escape hatch.
- New-agent wizard: wrap in a `<form>` so Enter submits; disable Create until the
  one required field (name) is non-empty.

Acceptance criteria:
- Model can be chosen without typing a model id; a malformed id is not silently
  savable; an unavailable provider shows an honest field-level state.
- A non-existent/again-unwritable workdir is flagged before it silently runs
  tools in the elanus root.
- Enter submits the wizard; Create is disabled on empty name.
- Flow catalog + `ui.spec.mjs` updated for the picker and the validation.

### M4 — Accessibility completeness

Shape:
- Global `:focus-visible` (e.g. 2 px `--work` outline, 1 px offset); never strip
  an outline without a ≥3:1 replacement. Cover tabs, filters, sessions rows,
  compose, ask buttons, all form inputs.
- Complete or drop the tab ARIA: if keeping `role="tablist"`, add `role="tab"` +
  `aria-selected` + `aria-controls` and `role="tabpanel"` + roving tab-index /
  Left-Right keys; otherwise remove the role and treat as a button group.
- `role="log"` + `aria-live="polite"` on the conversation feed so agent replies
  and asks are announced; decide deliberately on the high-volume telemetry feed
  (likely `aria-live="off"`).
- Min hit target 24 px (mobile 32–44) on nav/tab/filter/badge controls.
- Gate entrance animations and the alarm pulse on
  `@media (prefers-reduced-motion: no-preference)`; provide a non-flashing alarm
  state under reduced motion.

Acceptance criteria:
- Every interactive element shows a visible focus indicator and is reachable and
  operable by keyboard alone (Tab order sane, Esc closes dialogs, Enter sends).
- A screen reader announces a new agent reply in the open conversation.
- Tab semantics are either complete or absent (no half-pattern).
- Reduced-motion users get no infinite flash.
- `ui.spec.mjs` asserts focus visibility on a representative control and the
  conversation feed's live-region attributes.

### M5 — Product language (kernel-word eviction) and companion warmth

Shape:
- Rename the leaking kernel words: SESSIONS tab → "history"/"transcripts" (the
  code already uses "transcripts" in fallback copy); Workers "Coding sessions" →
  "runs"/"activity"; the compose button "transmit" → "Send"/"Reply"; raw ids only
  in a `title=` tooltip, never a column.
- Soften the two highest-traffic cockpit nouns as a *copy layer* (not a re-theme):
  "INSTRUMENTS" → the agent list / "explore"; "TELEMETRY" → "activity". Keep the
  cockpit identity available — gate the raw vocabulary behind a power/theme toggle
  so Tim keeps his and Lily/Daniel stop paying the vocabulary tax by default.
- Companion identity (Lily): a deterministic per-agent identity chip — a colored
  monogram/glyph derived from the name (still on-brand as a bordered monospace
  glyph) — shown in nav, converse header, and welcome; let create accept an
  emoji/color. Warmer first-contact empty state that uses the agent's name and
  purpose instead of the cold "nothing yet" log buffer.

Acceptance criteria:
- No "session"/raw id / kernel noun appears on a default product surface (audit
  the rendered strings); the power/theme toggle, if shipped, is the only place
  raw cockpit vocabulary surfaces.
- Each agent shows a stable identity chip in nav + converse header + welcome; two
  agents are visually distinguishable at a glance.
- Flow catalog + `ui.spec.mjs` updated for the renamed tab/labels and the chip.

### M6 — Visual consistency and polish

Shape:
- Unify the component kit: one radius idiom (pick cockpit-sharp or web-rounded,
  not both) and one primary + one ghost button style across shell and forms.
- Kill the duplicated content: render the autonomy consequence once; show the
  run-step cap once (card *or* field, not both as if two limits); collapse the
  welcome's redundant action buttons (two route to the same place); render
  nothing for the empty welcome hint `<p>`.
- Give the Signals/rail view an explicit empty state (it currently reads as
  broken).
- Apply the configure fold discipline to setup: make the new-agent wizard the
  dominant block; fold cost-visibility / trust / installed-capabilities under a
  disclosure so a first-timer's intention and the audit knobs aren't equals.
- Scope-honesty residual (M8 finding): give shared ("every agent") writes a
  distinct color/icon + an "affects all agents" tag, or move shared-config editing
  out of the per-agent tab into setup where the copy already says "every agent" —
  so blast radius is unmistakable, not just correctly labeled.
- Distinguish overloaded glyphs (the app kite vs the agent glyph are the same);
  give nav glyphs tooltips; confirm a destructive raw-TOML save has a confirm/diff
  like the off-switch does.

Acceptance criteria:
- One primary and one ghost button style; one radius idiom across shell and forms.
- No duplicated consequence/cap text in a single configure screenful.
- Signals view has a real empty state; setup leads with the wizard behind a fold
  for the audit blocks.
- A person can tell at a glance whether a package save is shared or per-agent
  without reading the button label.
- Flows + `ui.spec.mjs` updated for the changed layouts and the empty state.

## Guardrails

- **Layering rule is absolute.** No session/grant/stage/topic/mailbox/ledger or
  raw ids/URLs on default product surfaces. Translate at the boundary.
- **Don't dilute the cockpit.** The owner likes the aesthetic; persona-warmth and
  language changes are a default layer with the raw vocabulary behind a toggle,
  not a re-theme. M1 (contrast) and M2 (responsive) are bug fixes that apply to
  the cockpit itself.
- **Durable over flashing.** Confirmations must survive a reload — follow the
  durability facts in `docs/ui-flows/configuration.md`.
- **The flow catalog is the contract.** Every behavior change updates
  `docs/ui-flows/configuration.md` and `ui/web/test/ui.spec.mjs`.
- **QA the real UI.** Use the `web-qa` skill against an isolated live stack; for
  contrast/responsive/a11y, verify computed values and narrow viewports, not just
  a green you didn't watch.
- **Measure, don't eyeball.** Contrast ratios and hit-target sizes get computed
  and recorded, not estimated.

## Non-goals

- The Codex / Claude Code integration itself and the observability/telemetry
  pipeline — out of scope here (see `coding-agents.md`,
  `coding-agent-observability.md`).
- Real billing/pricing or usage history — cost stays "honest labels, no fake
  precision."
- Kernel/bus/identity redesign.
- "Changed since approval" drift backend — already deferred in
  [configuration-ux.md](configuration-ux.md); don't fake it here.
- A full re-theme or new design system — this is a fidelity pass on the existing
  one, not a redesign.

## Log (fill in as you go)

Status: M1–M6 implemented in one pass. Specs extended (`ui/web/test/ui.spec.mjs`
flows 8 narrow-viewport, 9 a11y, 10 product-language + identity, plus
contrast-baseline assertions in flow 1 and picker/validation assertions in
flow 2).

- **2026-06-25 — verified (handoff-workflow verify phase).** Authoritative
  verification run uncaged by the orchestrator (the cross-model workers run caged
  and cannot launch chromium/the web server):
  - **Automated:** the full `ui.spec.mjs` suite passes against the Rust `elanus
    web` server with real chromium (`ELANUS_UI_SPEC_RUST=1`) — ALL PASS, incl.
    flow 1 contrast-baseline, flow 2 picker/validation, flow 8 narrow-viewport,
    flow 9 a11y, flow 10 product-language + identity.
  - **Visual QA:** a 14-shot pass (welcome/setup/converse/configure/signals/
    workers/comms at desktop 1280 **and** narrow 390) confirmed: M1 text readable
    across all views; M2 narrow views stack/wrap with no clipping (masthead
    "CONNECTED" intact, tab strip wraps, cards `min-width:0`, compose reachable);
    M3 closed-set model value renders; M5 product language + per-agent identity
    chips (distinct colors) present; M6 amber reserved for voices, orange for
    signals. A GLM-5.2 cross-model code-read review ran in parallel as a second
    opinion. Status → **done**.

- (M1) Measured ratios for the changed tokens, in rest and active states
  (computed against `--bg #0f100e`, `--panel #161814`, active-row `#1b1d18`):
  - `--ink #d9d6c9` — 14.4 / 13.5 / 13.0 (well above AA)
  - `--dim #9a988c` (was `#76746a` ~3.6–4.1) — 6.6 / 6.2 / 5.9
  - `--meta #8f8d82` (new; replaces text usages of `--faint`) — 5.4 / 5.1 / 5.1
  - `--faint #4a4a42` is non-text only (`.conn-down .conn-dot` background);
    every other usage was swept onto `--meta`. A computed-ratio check is locked
    in `ui.spec.mjs` flow 1.
- (M2) Narrow-viewport breakpoints chosen and the nav-drawer approach: a single
  `@media (max-width: 900px)` block. The breakpoint matches the existing one.
  The sidebar becomes a **drawer** — `#nav-toggle` (hidden on desktop) flips a
  `navOpen` state on `<App>`; `.nav:not(.nav-open) #nav-list { display: none }`
  hides the list; a `useEffect` on `sel` closes the drawer after a person picks
  something. `max-height: 60vh` keeps the expanded drawer from eating the
  screen. The vignette softens (180→60 px), the masthead stacks, `.mast-sub`
  hides, the tab strip wraps, configure collapses to one column. Spec: flow 8
  asserts no horizontal overflow at 400×800 on boot + configure, and that the
  compose input stays inside the viewport.
- (M3) Model-picker behavior when the provider list is unavailable; workdir
  validation approach (client check vs server endpoint):
  - `ModelField` (`components/primitives.tsx`) renders a `<select>` over
    `/api/admin/models` with a single `custom…` escape row that reveals a text
    input. When the list is empty, it renders the input + an inline
    `provider list unavailable — type a model id or set an API key` note at the
    field (honest signal, not silent free text). Used in both the wizard and
    configure essentials. The old shared `<datalist id="model-suggestions">`
    was removed.
  - `WorkdirInput` calls a new read-only `GET /api/admin/path-check?path=…`
    (server.mjs) on blur and shows `path does not exist` / `not a directory` /
    `not writable by the agent` inline. Server-side check because the browser
    can't see the filesystem; loopback-only matches the existing authority
    model. Text input stays as the input — the picker is the inline validation
    state, not a file picker.
  - Wizard wrapped in `<form>`; `#na-create` is `type="submit"` with
    `disabled={!name.trim()}`. Enter submits.
- (M4) Tab-ARIA decision (complete vs drop); telemetry-feed live-region
  decision:
  - Dropped `role="tablist"` from both `#agent-tabs` and the rail `.tele-filters`
    (no `aria-controls` exists, so the role was a half-pattern). They are
    button groups with `aria-pressed` per button.
  - Conversation feed is `role="log" aria-live="polite"` (announces replies);
    telemetry feed is `aria-live="off"` (high-volume, not announced).
  - Global `:focus-visible { outline: 2px solid var(--focus); outline-offset:
    1px }` is the single keyboard affordance, source-ordered last so it wins
    over per-control `outline: none`. Min hit target 24 px (32 px at narrow).
    Alarm pulse + entrance animations gated on
    `@media (prefers-reduced-motion: reduce)` — animation off, alarm stays
    visible as a static orange dot + border.
- (M5) Power/theme-toggle decision (shipped vs default-only); identity-chip
  derivation:
  - **Shipped.** `#theme-toggle` in the masthead flips a `cockpit` state on
    `<App>` (persisted in `localStorage.elanus.cockpit`). Default is warm;
    Tim can opt back into cockpit nouns (instruments / SESSIONS / TELEMETRY /
    transmit) per working-with-tim's "don't dilute the cockpit" guardrail.
  - `AgentChip` derives hue from a 6-color on-brand palette (amber, teal,
    coral, sage, violet, gold) via a stable hash of the agent name. Two
    agents → two visually distinct chips. Used in nav (sm), converse header
    (md), welcome (md), and the conversation empty state (lg). `nav-sigil`
    no longer overloads the brand kite (`⟁`) — that lives only in the
    masthead now.
  - Warm empty state: `Start a conversation with {agent}. Replies and asks
    stay in this thread.` paired with the large chip (was `nothing yet — say
    something below.`). Coding-sessions surface: "Coding sessions" →
    "Coding runs"; raw ids stay in `title=` tooltips, never visible columns.
  - Compose button: `transmit` → `Send` (warm) — the cockpit toggle restores
    `transmit` for Tim. The post-submit reset value in `submitCompose`
    updated to match.
- (M6) Radius/button idiom chosen; scope-residual resolution (recolor vs
  relocate):
  - **Two tiers via tokens.** `--r-sharp: 3px` for inputs/tabs/buttons/badges
    (cockpit idiom); `--r-card: 6px` for cards/modals/banners/popovers. Form
    inputs dropped from 6 px onto the sharp tier — the worst "two people
    built this" seam. The split-button primary (`link ⌄`) now uses the same
    `--ink` background as every other primary.
  - **Scope-residual: recolor, not relocate.** The shared ("every agent")
    save button uses `.cfg-shared-save` — amber border, `⚑ every agent`
    glyph, `aria-label` extended with `— affects every agent using this
    add-on`. Per-agent save uses `.cfg-agent-save` (teal outline). Both
    stay next to each other (the labels were already correct), but the
    blast radius is now unmistakable at a glance.
  - Killed the duplicated autonomy `<p>` (id moved onto the cost-card `<em>`)
    and the spend-ceiling card (cap renders once at the input). Collapsed
    welcome's redundant setup/capabilities buttons into one. Removed the
    empty `<p id="welcome-hint">` when history is fine. Added the Signals
    empty state (`.rail-empty`). Wrapped cost / installed / trust /
    agent-requests in `<details class="setup-fold">` (installed open by
    default; agent-requests auto-opens when there are pending proposals).
    Raw-TOML save now calls `window.confirm` matching the off-switch pattern.
    Nav glyphs gained explanatory `title=` tooltips.

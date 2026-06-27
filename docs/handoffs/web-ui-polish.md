---
status: in-progress
author: Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-27
---
# Handoff: web UI polish — affordances, visual context editor, search, icons, light mode

Five UI nitpicks Tim raised on the web app, batched into one handoff because they
share a surface (`ui/web/src/App.tsx` + `styles.css`) and a theme: **the app reads
as too text-heavy and not visual/discoverable enough.** Four are self-contained CSS
+ small-JSX changes. **One — M2 — grew, in conversation, from "a context-step wizard"
into a genuinely reusable primitive: an _embedded narrow agent_ ("agentic wizard")
React component that runs an agent conversation against a switchable profile plus a
small set of purpose-built tools, used first to author context steps and reused
across the app wherever config/decisions are complex.**

The cleanly-shippable core is **M1, M3, M4, M5** (affordance, search, icons, light
mode): bounded, low-risk, mostly CSS and local state. **M2 is gated** — its direction
is set (client-side tools + a generic server bridge; Decision 1 decided), but the
bridge's mechanism + a couple of details (Decisions 2–4) want confirmation, and it's a
new reusable primitive + backend work, so do not implement M2 until those are settled.

## Read these first
- `ui/web/src/App.tsx` — the whole SPA (2161 lines). Key components: `ConfigureView`
  (1699), `ContextStageTile` (1862), `ConverseView` (2024), masthead toggle (1179),
  `submitCompose` send path (1102–1123), `TranscriptMsg` (2155).
- `ui/web/src/api.ts` (`publish()`) + `ui/web/src/live.ts` (SSE) — the conversation
  transport M2 reuses: POST `/api/publish` to `in/agent/<agent>` `{prompt, session}`,
  replies stream back over `/api/stream` matched by correlation id.
- `src/exec.rs` — the agent turn runner. `handle_exec` (2260), the genai tool loop
  (`into_tool_calls` 527, exec loop 586–669). **Tools execute server-side, inline;
  there is no browser tool-fulfillment path today** — central to M2's fork.
- `docs/config.md` + the config-model increments (agent Git-proposal round-trip /
  acceptance; memory `project-elanus.md`) — how a "save context block" tool should
  persist into the `<root>/config` repo `live` branch rather than writing files raw.
- `ui/web/src/styles.css` — all styling. `:root` color vars at lines 5–23.
- `docs/handoffs/configuration-ux.md` — the configure pane's design language (M1, M2 touch it).
- `docs/handoffs/web-ui-fidelity.md` + `chat-rendering.md` — chat-pane idiom (M3).
- The cockpit/plain "vocabulary toggle" — `App.tsx:26–33, 403–407, 1179–1181`. Already
  swaps warm copy ⇄ cockpit nouns; M4 builds on this, M5 reuses its masthead slot.

## Decisions to confirm (the wonky bits)

These mostly bit **M2**. **All decisions resolved by Tim 2026-06-27 — M2 is GO.**

1. **M2 — DECIDED (Tim, 2026-06-27): tools execute entirely client-side; the server is
   a generic bridge.** The component's tools are **plain JS handlers in the browser**;
   the server side stays **as generic as possible** so any future consumer's tools work
   without Rust changes. This is the bigger build — today **all tools run server-side,
   inline in the `elanus exec` genai loop** (`exec.rs:586–669`), with no path for the
   model to call out to the browser — but it's built **once**. The central new piece is
   a **generic "browser tool bridge"**: a per-conversation set of client-declared tool
   schemas reaches `exec`; when the model calls one, the turn **suspends**, the call is
   emitted to the browser, the browser runs the JS handler and **posts the result back**,
   and the turn **resumes**. The Rust side knows nothing about "context blocks" — only
   how to relay a generic `{name, args} → result` round-trip. **The remaining open
   sub-questions (2 below) are mechanism, not direction.**

2. **M2 — the bridge's suspend/resume mechanism + trust model (impl decisions, confirm
   the shape).** Sub-questions for the bridge:
   - **Where tool schemas come from:** the publish payload carries
     `client_tools: [{name, description, parameters(JSON Schema)}]` per conversation;
     `exec` adds them to the genai tool list as "remote" tools whose executor is the
     bridge, not local code. Confirm schemas ride the payload (vs. a registration call).
   - **The round-trip:** when the model calls a client tool, `exec` emits the call (a
     dedicated topic, e.g. `obs/.../tool/<tool>/call` already exists at `exec.rs:865`,
     or a new `signal/`-class request) and **awaits** a result the browser sends back
     via `/api/publish` (a `tool_result` keyed by call_id). Need a parked-turn await
     with a timeout. Confirm bus round-trip vs. a dedicated `/api/tool-result` endpoint.
   - **Authority/trust:** handlers run in the browser with the **owner's existing
     authority** — they can only do what the webUI already lets the user do (call the
     same APIs), so this is *not* a server privilege escalation (fits elanus's
     homogeneous-authority + audit model). But the server-side agent can now trigger
     client actions, so a **prompt-injected** agent could call a client tool with bad
     args. Mitigate: keep toolsets tight, render every client tool call/result inline
     so the user sees it, and let sensitive handlers (e.g. `save`) confirm in-UI.
     Confirm this threat treatment is acceptable for the first `context-author` toolset.
   **DECIDED (Tim): fine — ship the bridge as described (payload-borne schemas, bus
   round-trip keyed by call_id with timeout, owner-authority handlers, audited + inline).**

3. **M2 — profile default & dropdown. DECIDED (Tim): the component defaults to a system
   `helper` profile that _mirrors `default` until overridden_.** So `<AgentAssistant>`'s
   default profile is `helper`, not `default` directly: resolve `helper` to fall back to
   the owner's `default` profile when `helper` hasn't been customized, giving the
   assistant its own scopeable slot out of the box without any setup. The dropdown still
   lists profiles from `/api/admin/agents` so the user can override per-invocation.
   Impl: seed/resolve a `helper` profile that inherits `default`; don't hard-require it
   to exist on disk (synthesize the mirror).

4. **M2 — drag-and-drop. DECIDED (Tim): native HTML5 drag-and-drop**, no new dep; keep
   ↑/↓ arrows as the keyboard-accessible fallback. (`moveContextStage`, `App.tsx:912–920`
   supplies the reindex logic.)

5. **M5 — what "configurable" theme means for storage.** There's no settings store
   beyond localStorage today (cockpit flag, per-agent conversation id). **Recommendation:**
   persist `elanus.theme` = `system | light | dark` in localStorage, default `system`,
   apply via `document.documentElement.dataset.theme`. No server round-trip. Confirm
   that's enough, or whether theme should live in the config repo / profile so it
   follows Tim across devices.

6. **M4 — how far to push icons.** "Too many words across the entire app… replace
   with icons where appropriate." Risk: an all-icon UI hurts discoverability (the very
   thing Tim flagged elsewhere). **Recommendation:** icon + accessible label, with text
   shown for primary/destructive actions and on hover/focus (tooltip) elsewhere; lean
   on the existing `IconButton` primitive and `aria-label`. This is a sweep with
   judgment per call site, not a mechanical replace — keep it reviewable. Confirm scope
   (which panes are in vs. out for this pass).

## Milestones

### M1 — Advanced bar reads as one button (configure pane)
**Problem.** The `<details className="cfg-advanced">` disclosure (`App.tsx:1780`,
CSS `styles.css:689–718`) has `cursor:pointer` but **no hover feedback**, and the
`<h3>` + `<span class="dim-note">` inside the `<summary>` read as separate elements,
not one clickable bar.

**Do.**
- Add a `:hover` (and keep `:focus-visible`) rule on `.cfg-advanced > summary` —
  background lift (e.g. `background: var(--hover)` once M5 defines it, or
  `rgba(255,255,255,0.04)` for now) + a subtle border/transition so the whole bar
  visibly responds as one unit.
- Add a disclosure caret (▸/▾ rotating on `[open]`) at the left of the summary so it
  reads as a single expandable control. Make the entire summary the hit target
  (already is — verify the `dim-note` doesn't swallow clicks).

**Acceptance.** Mousing anywhere over the advanced bar highlights the whole bar as a
single button; a caret indicates open/closed state; keyboard focus shows the same
affordance; clicking anywhere on the bar toggles it. No JS behavior change (still a
native `<details>`).

### M2 — the `<AgentAssistant>` primitive (client-side tools) + visual context-step editor  ✅ GO (all decisions resolved)
**The idea (Tim's, refined in conversation).** Not a static wizard — a **reusable
React component**: an embedded *narrow agent* ("agentic wizard") that helps you
navigate complex config or decisions. It runs an agent conversation against a
**profile** (default `"default"`, switchable via dropdown) plus a small set of
**client-side tools** the call site passes in as plain JS handlers. The tight tool
scope is the point: 2–3 functions eliminate the CLI/skills exploration the agent would
otherwise need, so the agent can't wander. The first consumer authors **context steps**
(tools: list available context blocks, save one); the same component gets reused
anywhere config is fiddly. Working name `<AgentAssistant>` (naming open).

**Architecture (Decision 1, decided): client-side tools + a generic server bridge.**
Tools execute in the browser; the server side is built once and is tool-agnostic.

**Problem it replaces.** Context steps render as a vertical list of `ContextStageTile`s
(`App.tsx:1815–1829, 1862`) reordered by ↑/↓ arrows, added via a `<select>` + "add"
button. Tim wants a **visual, blocks-based** view with **drag-and-drop** reordering,
and a single **`+ New`** button that opens the assistant (not a form wizard).

**Do — the generic browser tool bridge (backend, build once).**
- Accept per-conversation client tool schemas (`client_tools` on the publish payload,
  per Decision 2) and register them with the genai run as **remote** tools — executor =
  the bridge, not server-local code.
- On a remote tool call: **suspend** the turn, emit the call to the browser
  (`{call_id, name, args}`), **await** the matching `tool_result` (posted back via the
  bus / an endpoint per Decision 2) with a timeout + cancel path, then **resume** the
  genai loop feeding the result. The Rust side never knows what the tool *does*.
- Reuse/extend the existing tool trace (`obs/.../tool/<tool>/call|result`,
  `exec.rs:865`) so client tool activity is audited like any other tool.

**Do — the `<AgentAssistant>` component (frontend).**
- Props: `{ profile?: string (default "helper"), tools: ClientTool[], title, intro,
  onDone? }`, where `ClientTool = { name, description, parameters(JSON Schema),
  handler(args) => Promise<result> }`. Component sends the schemas with the opening
  message, runs the conversation over the existing `submitCompose` publish path +
  `live.ts` SSE (its **own embedded session id** + corr namespace so it doesn't pollute
  the main chat), and **dispatches each incoming tool call to the matching `handler`**,
  posting the result back. Render each tool call/result inline so the user sees what it did.
- **Default profile = `helper` (Decision 3).** `<AgentAssistant>` defaults to a system
  `helper` profile that **mirrors `default` until overridden** — resolve `helper` to fall
  back to the owner's `default` profile when no on-disk `helper` exists/customization is
  present (synthesize the mirror; don't hard-require the file). This gives the assistant
  its own scopeable slot for free. A profile **dropdown** from `/api/admin/agents`
  (`App.tsx:470–477`) lets the user override per-invocation.
- First consumer — the `context-author` tools (pure client JS): `list_context_blocks`
  (reads the already-loaded package `manifest.stages` catalog, `contextStageDefs()`
  `App.tsx:720–735`) and `save_context_block(stage)` (adds to `cfgContextChain` /
  saves through the existing config-save path the configure pane already uses — no new
  server write path needed).

**Do — the visual editor.**
- **Blocks view.** Restyle the chain (`.cfg-context-chain`, `styles.css:897–928`) as a
  visual pipeline of cards/blocks that narrate what each step *is* (name, what it
  injects, resident vs. exec, enabled). Built-in seed = first immovable block.
- **Drag-and-drop** reorder (Decision 4), ↑/↓ kept as keyboard fallback; reuse
  `moveContextStage`'s `order = (i+1)*10` reindex.
- **`+ New`** opens `<AgentAssistant tools={contextAuthorTools}>` in a modal.

**Acceptance.** A reusable `<AgentAssistant>` exists, takes a switchable profile + an
array of client-side tools (name/description/schema/handler), runs a scoped agent
conversation in its own session, and **fulfills the agent's tool calls in the browser**,
results round-tripping back through the generic server bridge with the agent resuming
correctly. The `context-author` tools list available context blocks and add a new one
via the existing config-save path. Context steps render as visual blocks; drag-and-drop
reorder persists the same `context.stage` TOML as today; ↑/↓ still work for keyboard;
`+ New` opens the assistant; existing configs load/save unchanged. Every client tool
call/result is visible inline and traced. **Hold until Decisions 2–4 are confirmed.**

### M3 — chat pane: drop "current conversation" panel, add search
**Problem.** `ConverseView` (`App.tsx:2024`) shows a `.conv-current` panel
(`App.tsx:2070–2073`, CSS `styles.css:268–289`) Tim finds unhelpful, and there's **no
way to search conversations**.

**Do.**
- Remove (or demote) the `.conv-current` left column. Replace with a **search input**
  that filters the conversation list. Data already comes from
  `/api/conversations?agent=…` into the `conversations` prop (`App.tsx:572–589`); the
  `recent` list is sliced to 6. Add client-side filtering by `title`/`preview` over the
  full `conversations.list` (not just the 6-slice) as the user types.
- Keep the recent-conversations row (`.conv-recent-list`, `App.tsx:2074–2084`) as
  results / quick-switch; show filtered matches there.
- Stretch (confirm need): if client-side over the loaded list is too shallow, wire a
  server query param — but start client-side, it's likely enough.

**Acceptance.** The static "current conversation" panel is gone; a search bar filters
the agent's conversations live as you type; selecting a result switches to it
(reuses existing switch handler); clearing the search restores the recent list.

### M4 — fewer words, more icons (app-wide, judgment per site)
**Problem.** Tim: "too many words across the entire app." There's already a
warm⇄cockpit vocabulary toggle (`App.tsx:26–33, 1179`); this milestone is about
**replacing text labels with icons where it aids, not hurts, scanability** — see
Decision 5.

**Do.**
- Sweep the masthead nav, configure pane section headers, and action buttons. Where a
  control's meaning is conventional (close, add, delete, copy, expand, send, settings),
  use an icon via the existing `IconButton` primitive
  (`ui/web/src/components/primitives.tsx:10–21`) with an `aria-label` + tooltip.
- Keep text for primary CTAs and anything ambiguous; do **not** icon-ify where it would
  hurt the discoverability Tim values elsewhere. One reviewable pass per pane.

**Acceptance.** Common conventional actions across at least the masthead, configure,
and chat panes use icons with accessible labels/tooltips; no control loses its
meaning; screen-reader labels preserved (`aria-label` on every icon-only button);
the change is reviewable as a coherent diff, not a blind find-replace.

### M5 — light mode + system-theme auto-switch (configurable)
**Problem.** Only a dark palette exists (`:root`, `styles.css:5–23`); no
`prefers-color-scheme` handling, no toggle. Tim's Mac flips system light/dark on
ambient light frequently, so **follow the system by default**, with an override.

**Do.**
- **Tokenize.** Audit `styles.css` for hardcoded colors (~71 hex/rgba, ~60% not yet
  variables — Explore flagged `#1b1d18` hover, `#121410` code bg, `#201c14` agent bg,
  `#1c211d` user bg, `#9aa7c0` tool/sage, etc.). Promote the recurring ones to new
  `:root` vars (`--hover`, `--code-bg`, `--agent-bg`, `--human-bg`, `--tool`…) so a
  theme swap is a variable override, not a hunt.
- **Light palette.** Add `:root[data-theme="light"] { … }` overriding the tokens to a
  WCAG-AA-compliant light scheme (mirror the existing AA discipline noted in the dark
  `:root` comments — text/bg ≥ 4.5:1).
- **Switching.** On mount, if `elanus.theme === 'system'` (default), set
  `document.documentElement.dataset.theme` from
  `matchMedia('(prefers-color-scheme: light)')` and **subscribe to its change event**
  so it re-applies live when the Mac flips (Tim's ambient-light case). Add a real
  theme control in the masthead — note `id="theme-toggle"` at `App.tsx:1179` is
  currently (mis)used by the cockpit toggle; the actual theme control should own that
  slot or a neighbor. Persist override as `elanus.theme` (`system|light|dark`) in
  localStorage (per Decision 4).

**Acceptance.** With OS in dark, app is dark; switch OS to light and the app follows
**without reload**; a masthead control overrides to force light/dark/system and the
choice persists across reloads; light mode passes AA contrast on primary text/surfaces;
no hardcoded color blocks the swap (spot-check agent/human message bubbles, code blocks,
hovers).

## Suggested sequencing & models
- **Land M1 + M3 + M4 + M5 first** (the ungated core). M1 is a CSS warm-up; M5 is the
  largest of these (tokenization sweep) and the highest-value; M3 and M4 are medium.
- **M2 after Tim confirms Decisions 2–4** — it's a new reusable primitive
  (`<AgentAssistant>`) plus a generic backend browser-tool bridge, not a polish; build
  the bridge + component + `context-author` client tools first, wire the context-step
  UI as its first consumer. Expect follow-on handoffs as the primitive gets reused.
- Per `handoff-workflow`: dispatch impl to a clean-context worker (Opus/GPT-5.5 medium),
  verify with a separate stronger agent against the Acceptance clauses (must `npm`-build
  the UI and run `ui/web/test/ui.spec.mjs`). Orchestrator commits — one scoped commit per
  milestone (or per ungated batch). No git in workers; UI probes in /tmp.

## Log
- 2026-06-27 — Planner (Claude/Opus, high). Wrote handoff from Tim's 5 nitpicks.
  Grounded against App.tsx/styles.css via two Explore passes; anchors cited inline.
  M2 flagged as gated pending wizard-scope decisions. Working tree at write time has
  uncommitted edits on `model-providers` (App.tsx, live.ts, web.rs, exec.rs, etc.) —
  impl should branch from a clean base and not sweep those in.
- 2026-06-27 — Tim reframed M2: not a static wizard but a **reusable embedded-agent
  React component** (`<AgentAssistant>`) — a narrow agent over a switchable profile +
  injected purpose-built tools, reused across the webUI for complex config/decisions;
  context-step authoring is its first consumer. Third Explore pass mapped the
  conversation transport (publish→SSE) and confirmed **the architectural fork**: all
  tools run server-side inline today, no browser tool-fulfillment path exists. M2
  rewritten around the primitive; Decisions 1–2 replaced (tool-execution fork +
  whitelisted server toolset catalog), profile-dropdown decision added.
- 2026-06-27 — Tim **decided Decision 1: tools run entirely client-side; the server
  should be as generic as possible.** M2 reframed: build a **generic browser tool
  bridge** (server suspends a turn, round-trips a `{name,args}→result` to the browser,
  resumes — tool-agnostic, build once) + an `<AgentAssistant>` whose `tools` prop is an
  array of client JS handlers. `context-author` tools become pure client JS over the
  already-loaded `manifest.stages` + existing config-save path. Remaining open items =
  bridge mechanism + trust (Decision 2), DnD dep (4), profile dropdown (3). Authority
  note: client handlers run with the owner's browser authority (no server escalation),
  but a prompt-injected agent could mis-call them → keep toolsets tight, render every
  call/result inline, confirm sensitive handlers in-UI.
- 2026-06-27 — Tim resolved the last open items: **D4** native HTML5 drag-and-drop (no
  new dep, ↑/↓ kept as keyboard fallback); **D3** default profile = a system `helper`
  that mirrors `default` until overridden; **D2** the bridge shape is fine as written.
  **M2 is GO.** Status → in-progress. Implementation dispatched per handoff-workflow:
  GPT-5.5 (codex) high for M2, GPT-5.5 medium for the M1/M3/M4/M5 core, GLM-5.2
  (opencode) high to verify; orchestrator (Claude) commits. Clean base first: committed
  the in-flight SSE-reconnect fix and the model-providers provider-save tail as two
  separate scoped commits so the impl doesn't entangle them.

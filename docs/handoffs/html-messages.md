---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: let an agent deliberately answer with HTML — and teach it that it can

Journey [../journeys/07-chatting.md](../journeys/07-chatting.md) promises an
agent can answer with real interface elements — small forms and buttons that
continue a conversation without rebuilding context. Sprint 1's platform-trust
work already made the *rendering* possible: at full trust the converse feed runs
agent messages through `ui/web/src/Markdown.tsx` with `rehype-raw` on, so raw
HTML becomes live DOM ([platform-trust.md](platform-trust.md) M4). Two things are
still missing, and they are Tim's demo-day finding:

1. **The agents don't know they can.** Nothing in their context tells them HTML
   is an option, or that it only renders at full trust. So they never try.
2. **Rendering is a sniff, not a decision.** Today the feed renders *whatever*
   HTML happens to be in *any* message at full trust. There's no recorded signal
   that the agent *meant* the body to be HTML, so a third-party UI can't tell a
   deliberate form from an incidental angle-bracket, and the reduced-trust escape
   isn't anchored to intent.

Tim's decision: add a `format` field (`"markdown"` default, `"html"`) to
`send_message` and `ask_human`, **record it on the ledger event**, render by it
deliberately, and **teach both modes** in the agent's context — advertised only
when trust is full.

## Wonky bits / decisions to confirm

1. **The teaching composes the two blocks; it does NOT add a third computed
   block.** The trust-aware advertisement already has a home: the `platform`
   computed block (`packages/platform/scripts/main`) exists precisely to tell the
   agent "whether the raw HTML it sends in chat will render" — its own header
   says so — and it already branches on full vs reduced trust. So M3 puts the
   *conditional capability line* there (only full trust names the `format="html"`
   verb), and puts the always-present *how/when etiquette* in the
   `comms-etiquette` skill (`kits/core/packages/comms-etiquette/SKILL.md`), which
   already owns `send_message`/`ask_human` and is only visible to agents that hold
   the comms verbs. Trust is computed **once** (in `platform`); the skill teaches
   the mechanics and defers the "may I right now?" to the platform block. *Fable:
   the alternative is a dedicated trust-reading computed "chat" block that emits
   the HTML section only at full trust — more moving parts, a second place that
   reads trust. I chose composition (simplest honest mechanism, per the brief).
   Confirm.*

2. **`format` records intent; the trust gate is unchanged and singular.** Raw HTML
   becoming live DOM stays gated on one thing: `trust === full` (the existing
   `allowHtml` in `Markdown.tsx`). `format` does not widen that gate — a
   `format="html"` body at reduced trust renders **escaped**, exactly as today.
   What `format` buys: (a) it lands on the ledger event payload so a third-party
   UI knows the agent deliberately chose HTML; (b) it lets the renderer treat a
   full-HTML body as HTML rather than markdown-with-incidental-tags; (c) it is the
   concrete verb the teaching points at. *Fable: confirm we keep `trust===full` as
   the sole rendering gate and never let `format` alone unlock raw HTML.*

3. **Default `markdown` keeps today's "inline HTML for small touches" behavior.**
   A `format="markdown"` (or absent) message renders as markdown, and at full
   trust inline raw HTML in it still renders (today's `rehype-raw` path) — that is
   the "small touches" mode Tim wants preserved. `format="html"` is the "the whole
   body is an HTML fragment" mode (a form, a button-bar). Concretely the renderer
   can, for `format==="html"` at full trust, render the body as HTML without
   markdown block-processing so a `<form>`/`<table>` isn't mangled by markdown
   paragraph rules. *Fable: this is the one place the two modes actually diverge in
   output; if you'd rather both modes just route through the existing
   markdown+rehype-raw path (simpler, but markdown can mangle block HTML), say so.*

4. **`ask_human` renders in the ask affordance, not the plain feed.** An ask is
   drawn by `AskMessage` (`ui/web/src/App.tsx:2315`), a different component from
   the plain `msg-body`. If `format="html"` is offered on `ask_human`, the same
   trust gate must be applied there too, or an ask's HTML would render unguarded /
   not at all. Small, but easy to miss — call it out in M2.

**Product language.** In the interface this never surfaces as "format",
"markdown", or "rehype-raw". The agent-facing docs (skill + platform block) are
builder-altitude and may name the verb; the person just sees a rendered message.

## Milestones

### M1 — `format` on the two verbs, recorded on the ledger
Add an optional `format` property (`enum: ["markdown","html"]`, default
`"markdown"`) to the `send_message` and `ask_human` tool schemas
(`src/exec.rs:1509` and `:1523`). In the `send_message` arm (`src/exec.rs:1905`)
carry it into the emitted payload (`src/exec.rs:1921`,
`json!({ "text": text, "session": session })` → add `"format"`); same for the
`ask_human` arm's payload (`src/exec.rs:1971`). Validate the value: anything other
than `markdown`/`html` is rejected or coerced to `markdown` (decide; coerce is
friendlier). The payload is the ledger event body (`emit_message`,
`src/exec.rs:865`), so recording it there is the whole "recorded on the ledger"
requirement.

**Acceptance:** a unit test drives the `send_message` tool with
`format="html"` and asserts the emitted `in/human/<owner>` event's payload has
`format == "html"`; a call with `format` omitted records `"markdown"` (or no
field, and the reader defaults to markdown — pick one and test it); an invalid
`format` does not error the run. `cargo test` green.

### M2 — Render by `format`, deliberately, still trust-gated
`conversation_messages` (`src/web.rs:2596`) and the feed helper
`push_human_feed_message` (`src/web.rs:2787`) must carry `format` through onto the
message JSON the converse endpoint returns (today they emit `{id,type,who,cls,
text,ts,...}` — add `format`). In `ui/web/src/App.tsx` the feed row
(`App.tsx:2307`) renders `<Markdown text={m.text} allowHtml={allowHtml} />` where
`allowHtml={systemStatus?.trust === 'full'}` (`App.tsx:1301`). Make it deliberate:
pass `m.format` to `Markdown` and let the component decide — `format==="html"`
&& full trust → render as HTML; otherwise markdown (with inline HTML still live at
full trust for the small-touches case). At reduced trust a `format="html"` body
renders **escaped**. Apply the same gate in `AskMessage` (`App.tsx:2315`) if
`ask_human` carries `format` (wonky bit 4).

**Acceptance:** `ui.spec.mjs` seeds two converse messages, one `format="html"`
carrying a `<button>`, one default: at **full** trust the html one's rendered DOM
contains a real `<button>` element while the default renders its text as markdown;
at **reduced** trust the html one shows the markup as visible escaped text, no
live element. Follows the `data-sel`/`waitForSelector` discipline. Rebuild +
re-embed the SPA before running the spec (web-embed staleness note in memory).

### M3 — Teach it: the platform block names the verb, the skill teaches the modes
- **`packages/platform/scripts/main`** — in `block_text`, at **full** trust add a
  concrete, actionable line naming the verb: an agent may reply with a full HTML
  document/fragment by setting `format="html"` on `send_message`/`ask_human`
  (good for forms and buttons that continue the conversation), and inline HTML in
  an ordinary markdown message also renders. At **reduced** trust the existing
  "shown as escaped text" line stands — do **not** advertise `format="html"`
  there.
- **`kits/core/packages/comms-etiquette/SKILL.md`** — add a short "Answering with
  HTML" section under the human-comms verbs: the two modes (`format="html"` for a
  whole-body form/button reply — journey 07; inline HTML in markdown for small
  touches), *when* to reach for each, and that it only renders as live elements at
  full trust — "check the platform block; if it says reduced trust, your HTML will
  show as text, so answer in plain markdown." Keep it etiquette, not a spec.

**Acceptance:** `elanus context render <profile> <session>` on a comms-holding
profile shows the `platform` block naming `format="html"` at full trust and
**omitting** it at reduced trust (flip `trust` in `bus.toml` between renders); the
`comms-etiquette` skill documents both modes and the trust caveat. No code path
reads trust in a second place (grep confirms only `platform` computes it).

## Read these first
- The why: [../journeys/07-chatting.md](../journeys/07-chatting.md) (HTML that
  continues a conversation).
- The rendering already built: [platform-trust.md](platform-trust.md) M4;
  `ui/web/src/Markdown.tsx` (the `allowHtml`/`rehype-raw` gate);
  `App.tsx:1301` (trust → `allowHtml`), `App.tsx:2307` (the feed row).
- The verbs: `src/exec.rs` — `send_message` schema `:1509`, arm `:1905`, payload
  `:1921`; `ask_human` schema `:1523`, arm `:1944`, payload `:1971`; the shared
  emit `emit_message` `:865`.
- The projection to widen: `src/web.rs` — `conversation_messages` `:2596`,
  `push_human_feed_message` `:2787`.
- The teaching homes: `packages/platform/scripts/main` (the trust-branched
  `block_text`), `kits/core/packages/comms-etiquette/SKILL.md` (the human-comms
  section), `kits/core/packages/comms-etiquette/elanus.toml` (it owns the verbs).
- The rule for wording: [../layering.md](../layering.md).

## Log
- 2026-07-02 — Created from Tim's demo-day findings. Verified in the worktree:
  `Markdown.tsx` already gates raw HTML on `allowHtml` and the converse feed
  already passes `allowHtml={trust==='full'}` (`App.tsx:1301`), so *rendering*
  exists — the gaps are (a) no `format` intent recorded on the ledger and (b) the
  agent is never told. The `platform` computed block already branches on trust and
  already exists to tell the agent whether its HTML renders, so M3 extends it
  rather than adding a second trust-reading block (decision 1). Judgment calls for
  Fable: composition over a new computed block (1); `trust===full` stays the sole
  rendering gate, `format` is intent only (2); default-markdown keeps inline-HTML
  small touches while `format="html"` is whole-body (3).
- 2026-07-02 — All milestones implemented and adversarially verified (Opus
  impl/verify under Fable orchestration); landed on sprint-recon-2026-07.
  Status flipped to done.

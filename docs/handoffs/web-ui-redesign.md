---
status: M1 done (tokens · unified red · fonts · logo) + phrasing done; M2-M5 (per-view, motion) follow-up
author: Claude Opus 4.8 (planner) — design direction
last-updated: 2026-07-07
---

# Web UI redesign — shrike-ifying the flight recorder

A full visual + UX redesign of the lanius web UI (`ui/web/`), built on the new
shrike/thorn brand identity (`brand/logos/`). The thesis in one line: **the UI is
already a flight recorder you can talk to — the brand makes it a butcher bird's
flight recorder.** This is an evolution of the existing cockpit aesthetic, not a
teardown; most of the work is a token swap, a type system, and weaving the logo
system in — then per-view polish and the UX walkthroughs.

Grounded in the current UI (recon 2026-07-07): `ui/web/src/App.tsx` (2690 lines),
`styles.css` (1276 lines, fully token-based), `components/`, and a large
**load-bearing e2e selector contract** (`ui/web/test/ui.spec.mjs`, ~65+ stable
ids) that this redesign must preserve or co-migrate.

> **Voice & copy is a companion:** [web-ui-copy.md](web-ui-copy.md). The app also
> reads too wordy and in a private vocabulary — do the plain-language overhaul in
> the same pass (delete the cockpit/plain toggle; name things as they are). These
> are text-only changes and don't touch the e2e *id* contract.

---

## The one big idea: the thorn IS the algedonic channel

The current design reserves **international-orange `#ff4f00` for the algedonic
channel ALONE** (`styles.css:1-13` — Beer's pain/pleasure signal: the signal lamp,
urgent rows). The brand reserves **thorn-red `#E5484D` for the impaled / the
thorn** — nothing else gets red. **These are the same discipline.** Unify them:

> **Red is the thorn. The thorn is where attention concentrates. Brand accent and
> algedonic alarm are one reserved channel — only the *intensity* varies.**

- **Resting accent** (brand): thorn-red `#E5484D` — the wordmark tittle, the active
  nav state, the one-thing-that-matters highlight. Used sparingly, like the
  impaled berry.
- **Algedonic / alarm** (pain): the thorn *drawing blood* — an intensified hot
  thorn (`#FF4A3D`, hotter and more saturated than resting). The signal lamp, a
  failed run, a blast-radius warning. It reads as "the same red, escalated."

This retires the orange/red collision, honors Tim's algedonic discipline, and
makes the brand's red-restraint and the UI's alarm-restraint the same rule. It is
the redesign's organizing principle — everywhere else is grey and ink.

---

## Type system (fonts — the part Tim asked for)

The real app self-hosts fonts (no artifact CSP limit), so pick *real* faces.
Today: IBM Plex Mono (body) + Instrument Serif (mast h1), from Google Fonts
(`index.html:20-22`). The mono voice is the product's soul (it's a CLI-native
harness) — **keep mono central, but add a proper reading face and make it a
system, not a default.**

**Two core families + one editorial accent. Self-host all (SIL OFL, `/fonts/`,
`@font-face`, `font-display:swap`) — drop the Google Fonts link.**

1. **Commit Mono** (or keep IBM Plex Mono as the zero-cost fallback) — *the
   systems voice.* Labels, badges, timestamps, telemetry, code, session ids,
   tabular data (`font-variant-numeric: tabular-nums`), the CLI-native micro-
   labels. Neutral, terminal-bred, no personality tax. This is the "camera /
   flight recorder" reading — machine truth.
   - *Why not stay on Plex Mono:* Plex is fine but soft; Commit Mono is tighter
     and reads cleaner in dense tables. Interchangeable — decide at build with
     both rendered. Either way: **mono stays the voice, not a gimmick.**

2. **Hanken Grotesk** — *the human voice.* UI text, buttons, headings, body copy,
   the reading surface (chat messages, journey/setup prose, dense config views
   where all-mono fatigues). A warm humanist grotesque that harmonizes with the
   round-geometric wordmark and is *deliberately not Inter/Space Grotesk* (the AI-
   default trap). It carries the Lily-facing warmth the personas need without
   going soft.
   - *Alternates, defensible:* **Uncut Sans** (more neutral-modern, "flag-tech"),
     or **Geist Sans** (Vercel's — on-the-nose for the flagship-tech brief, open,
     but becoming a default). Pick Hanken for warmth + distinctiveness.

3. **Instrument Serif** (already loaded — keep as an *editorial accent only*) —
   the "logbook" moments: a big masthead number, an empty-state headline, a
   section epigraph. A flight recorder is a logbook; a restrained serif nods to
   that. Never body, never UI chrome. *If simplifying, this is the cut.*

**Scale & roles.** Mono for everything ≤13px and all data/labels; Grotesk for
≥14px reading and headings (600–720 weight, tight tracking `-0.01em` to `-0.02em`
on large sizes, `text-wrap: balance` on headings); serif for the rare display
moment. The masthead wordmark becomes the **08 SVG logo** (retire the Instrument-
Serif "lanius" h1 — the wordmark IS the type now).

---

## Color & tokens

Rebrand is a **two-block token swap** (`styles.css:5-63` dark default,
`:65-119` light) — the architecture already supports it; nothing is hardcoded
per-component except `AGENT_PALETTE` (`App.tsx:39-46`, see Migration). Map:

| role | today | → new (dark ground) | → new (light ground) |
|---|---|---|---|
| ground `--bg` | `#0f100e` | `#0F1115` (ink, cooler) | `#F4F5F7` (paper) |
| surface `--panel` | `#161814` | `#171A20` | `#FFFFFF` |
| edge `--panel-edge` | `#24261f` | `#242830` | `#E0E3E8` |
| ink `--ink` | `#d9d6c9` (bone) | `#E7EAEE` | `#16181D` |
| dim `--dim` | `#9a988c` | `#8B919B` | `#61666F` |
| **accent `--thorn`** (NEW, replaces scattered) | — | `#E5484D` | `#E5484D` |
| **algedonic `--pain`** (was `--orange`) | `#ff4f00` | `#FF4A3D` | `#FF4A3D` |
| shrike grey (NEW, secondary) | — | `#C9CFD6` | `#C9CFD6` |

**Voice colors** (`--agent`/`--human`/`--work`/`--ask`/`--tool`) stay as a
*muted, desaturated* family so the thorn is always the loudest thing on screen —
retune them toward the shrike's cool grey-blue palette (the current amber/teal
set is warm-cockpit; shift to slate/steel with just enough hue to distinguish
speakers). Keep the documented AA-contrast discipline (`--focus`, the a11y
comments). **Radii:** keep the sharp-vs-card split (`--r-sharp 3px` cockpit chrome
/ `--r-card` cards) — it's on-brand (precise, engineered); nudge card radius to
match the logo tile's squircle feel.

---

## The logo system in the UI

- **Favicon / app icon / PWA:** `05-mask-tile` (self-theming, best at 16px).
  Replaces whatever's there; wire as `/favicon.svg` + the apple-touch/PWA icons.
- **Masthead:** the **08 wordmark** SVG (inline, `currentColor`) replaces the
  Instrument-Serif `lanius` h1 (`App.tsx:1437-1464`). On the welcome/empty states,
  the **09 lockup** (thorn + wordmark) as the hero.
- **Thorn as accent glyph:** the `01/04` thorn becomes the system's active/marker
  glyph — the active-nav indicator, the "you are here" tick, list bullets in
  setup, the tittle rhyme. It replaces generic dots/carets.
- **The signal lamp becomes the thorn** (`#signal-lamp`, `App.tsx`): when the
  algedonic channel fires, the thorn glyph lights to `--pain` and gets the pulse
  (`@keyframes alarm`) — the shrike's warning. Perfect metaphor: the thorn is
  where the pain is.
- **Agent chips:** replace the hardcoded `AGENT_PALETTE` 6-hex array with
  monograms on a shrike-grey field + a thorn accent, or a deterministic *tint of
  the neutral* (not rainbow) so the palette stays disciplined.
- **Sticker (`06`/`10`):** not in the app chrome — for the repo README, the
  loading splash, and physical swag. `10-shrike-wordmark` (bird + word) is the
  README/hero lockup.

## Texture & motion (keep the recorder soul, refine it)

- **Keep** the scanline + vignette (`styles.css:125-150`) — it *is* the flight-
  recorder/camera texture, and it's on-brand. Retune to the cooler ink ground;
  drop opacity slightly so it reads as a whisper, not a CRT costume.
- **Keep** the `settle`/`arrive` entrance animations and the alarm pulse; retune
  timing to feel precise (the current 0.7s settle can tighten to ~0.5s). Preserve
  the `prefers-reduced-motion` handling (`styles.css:1244-1276`) — non-negotiable.
- **New micro-moment:** an impaled-berry beat — when a worker result lands / an
  event is captured, a tiny thorn-red tick "pins" it onto the rail (the larder
  metaphor made literal, 120ms, reduced-motion-safe). One delightful moment, not
  scattered effects.

---

## Component treatment (per surface)

- **Mast** (`App.tsx:1437`): wordmark logo left; right cluster (theme, vocab
  toggle, AI-panel, signal-thorn, conn status) set in mono micro-labels, tighter.
- **Nav** (`App.tsx:1624`): the flat vertical list stays; active item marked by
  the thorn glyph + a hairline thorn-red rule, not a filled block. Agent chips
  restyled (above). The workers `<details>` and "+ new agent" keep their ids.
- **Tabs / stage-head** (`#agent-tabs`): sharp-radius mono tabs; active tab
  underlined in thorn, not boxed.
- **Chat feed** (`ConverseView`): messages in Grotesk (reading), meta/labels in
  mono; you/agent/ask/system/failed classes retinted to the muted voice family;
  `failed` uses `--pain`. Compose bar (`#compose-input`) gets a calmer field with
  a thorn send-affordance.
- **Cards / badges / config rows** (`ConfigureView`): grant/risk badges in mono
  with a single thorn dot for "needs attention"; the shared-vs-agent save buttons
  (`.cfg-shared-save` amber today) → shared/blast-radius uses `--pain` framing
  (it's an algedonic "this affects everyone" signal), agent-scoped stays neutral.
  This *fixes the honesty gap* the config journey flagged: blast radius now reads
  in color, not just label.
- **Providers vault / code-sessions tree / comms:** restyle to tokens; the
  session-tree and comms plane are data-dense → lean mono + tabular-nums, thorn
  only for state that demands action (failed, awaiting-approval).
- **AI helper panel** (`#ai-panel`): the shrike's perch — a calm right rail; the
  helper's tool-calls render as mono "the bird did X" lines; keep the no-dead-end
  world-c state.

---

## UX walkthroughs (the redesigned flows, per persona)

Each is the *felt* experience after the redesign — grounded in the real steps
(recon §5) and the journeys (`docs/journeys/`).

### 1. First run — "set up the aviary" (Lily / Daniel)
Land on **welcome**: the **09 lockup** hero, one honest line of stack health
(root · credential · broker) in mono, and a single thorn-marked primary action —
"set up an agent" (or, if a harness is detected, "set up by chatting" opening the
helper panel). Into **setup**: the capability catalog reads as outcomes, not
kits; the new-agent wizard (`#na-*` ids preserved) is a calm single column; cost
visibility states the hard-cap honestly (no fake dollar precision). Success lands
in the agent's **converse** with the thorn "you are here" on the nav. *Felt:*
Lily sees her new pet named and perched; Daniel sees predictable, labeled steps.

### 2. Converse — "talk to the bird" (everyone)
Select an agent → **converse**. Recent conversations in a quiet mono list; the
feed reads in Grotesk with mono timestamps; an incoming worker result gets the
impaled-berry pin. Asks render as answerable thorn-marked cards inline. Reply-
branch forks a `web-*` session with a subtle thorn "branched from here" marker.
*Felt:* the conversation is the surface; the machinery whispers.

### 3. Dispatch & watch a worker — "the larder fills" (Tim / Daniel)
From converse, dispatch a coding worker; it appears in the **runs**
(`CodeSessions`) tree. Each session line: tool · model · effort · duration in
mono/tabular-nums, a live state chip (running/failed/…), and — the payoff of the
situational-awareness work — its **intent** and a thorn-pin as results land. A
failed run is the one red thing. *Felt:* you can see the whole larder at a glance
and what each impaled item is.

### 4. Configure — "trim the agent" (Daniel / Ganesh)
Gear tab → **configure**. Essentials, sandbox, packages, context chain. The
redesign's honesty win: **blast radius reads in color** — shared/every-agent
saves carry the algedonic `--pain` framing; agent-scoped is neutral. Grant/risk
badges are legible mono with a thorn dot when approval is pending. *Felt:* Ganesh
sees where the risk is without decoding labels.

### 5. The helper panel — "ask the shrike" (Lily)
Toggle `#ai-panel`. The perch opens (shrinks the deck, never overlays — the M2
layout fix). The helper answers in plain Grotesk, its tool-calls a quiet mono
ledger ("read your status", "opened setup"). World-c → the no-dead-end nudge.
*Felt:* a knowledgeable bird on your shoulder, not a chatbot bolted on.

### 6. Providers — "feed the birds" (Daniel)
`ProvidersView`: add a credential (native login or API key), test reachability
(thorn only on failure), select per agent. The empty ModelField links straight
here. *Felt:* one obvious path, honest state.

---

## Constraints & migration (do not skip)

- **The e2e selector contract is load-bearing** (`ui.spec.mjs`, ~65+ ids:
  `#view-*`, `#na-*`, `#cfg-*`, `#setup-*`, `#ai-panel*`, `data-sel`/`data-tab`/
  `data-provider`/`data-providers-link`). A visual redesign restyles freely but
  **must preserve these ids/attributes or co-migrate them in the same change**
  as `ui.spec.mjs`. Treat it as the API of the UI.
- **Embedded-SPA build discipline** (`src/web.rs:56`, `include_dir!`): after any
  UI change, `npm run build` → `touch src/web.rs && cargo build` → run e2e. A
  redesign that skips this QAs a stale binary (it has bitten before). See
  [[elanus-web-embed-staleness]].
- **`AGENT_PALETTE`** (`App.tsx:39-46`) is a hardcoded hex array *outside* the
  token system — it won't pick up the rebrand; retune it explicitly.
- **Cockpit/warm vocabulary toggle** (`LABELS`, `App.tsx:31`): decide whether the
  new brand voice *replaces* it or keeps it. Recommendation: keep the toggle but
  make "warm" the default for the Lily-facing surfaces (welcome/setup) and
  "cockpit" opt-in for power users — the brand supports both registers.
- **Server-driven vocabularies** (liveness states, grant states, cost hard-cap vs
  soft-limit) are fixed from `src/web.rs` — restyle, don't reinterpret.
- **Theme:** keep the three-way `system|light|dark` + `data-theme` + the
  `lanius.theme` key. Rename the key only if you accept a one-time reset.

## Milestones (phased, each shippable)

- **M1 — Foundation:** self-host the two font families; swap the two token blocks
  to the shrike palette; unify red (`--thorn` + `--pain`, retire `--orange`);
  wire the mask-tile favicon + wordmark masthead. Everything else inherits.
  *Acceptance:* the app is on-brand at the token level, all e2e ids intact, e2e
  green, light+dark both correct.
- **M2 — Chrome:** nav, mast, tabs, badges, the thorn active-marker + signal-
  thorn, agent-chip retune. *Acceptance:* navigation + status chrome fully
  restyled; selectors intact.
- **M3 — Surfaces:** converse feed, configure (incl. the blast-radius color
  honesty), providers, code-sessions tree, comms, helper panel. *Acceptance:*
  each view restyled to tokens; the config blast-radius reads in color.
- **M4 — Motion & delight:** retuned settle/arrive, the impaled-berry pin,
  texture retune; reduced-motion preserved. *Acceptance:* motion feels precise;
  reduced-motion parity.
- **M5 — Walkthrough polish & QA:** the six flows above walked end-to-end per
  persona; empty/error/world-c states; full `ui.spec.mjs` green against a freshly
  re-embedded binary. *Acceptance:* the walkthroughs feel as written; e2e green.

## Read these first
- `brand/logos/` + `CONCEPTS.md` (the identity) and the logo gallery artifact.
- `ui/web/src/styles.css` (tokens `:5-119`, texture `:125-150`, motion `:160/
  436/1244`), `ui/web/src/App.tsx` (views `:1437-1700`, nav `:1624`, tokens/JS
  palette `:39-46`), `ui/web/index.html` (font links to replace).
- `ui/web/test/ui.spec.mjs` — the selector contract.
- `docs/journeys/01-setup.md`, `07-chatting.md`, `08-dispatching-a-worker.md`,
  `06-configuration.md`, `15-agentic-configuration.md`, `characters.md`.
- The situational-awareness handoff (its intent/sitrep data feeds flow 3).

## Log
- 2026-07-07 (Opus, planner): wrote the direction from the shrike brand + a full
  recon of the current UI. Organizing idea: **unify the brand thorn-red and the
  algedonic alarm into one reserved red channel** (respects Beer's algedonic
  discipline and the brand's red-restraint at once). Type: keep mono as the
  systems voice (Commit Mono / IBM Plex Mono), add **Hanken Grotesk** for reading
  + headings (deliberately not Inter), Instrument Serif as an editorial accent;
  wordmark logo replaces the serif h1. It's an evolution of the flight-recorder
  aesthetic, not a teardown — mostly a token swap + type + logo, then per-view
  polish. Hard constraints flagged: the ~65-id e2e contract and the embedded-SPA
  build loop. A visual style-guide artifact accompanies this doc.

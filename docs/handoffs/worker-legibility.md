---
status: done
author: Fable 5 (planner, session c185ae6f)
last-updated: 2026-07-13
---

# Handoff: worker legibility — the evidence-settled mechanical core

The worker UI splits one worker across four projections and strips its identity
on the way. This handoff lands the **mechanical, evidence-settled slice** of
[docs/bugs/worker-surfaces.md](../bugs/worker-surfaces.md) (the Sol/GPT-5.6
investigation — read it first; every anchor below was re-verified against the
working tree): carry purpose into the Runs projection, record the launch edge
where it's already known, make History's availability honest, default workers
to Chat where a chat exists, and rename the visible word Converse → Chat.

**Explicitly OUT of scope** (named deferrals, see the end):
- The taste-level **worker home redesign** (semantic Activity grouping, the
  legible "what is happening?" landing surface, reconciling the 6/154/14
  worker counts) → gets its own **journey**. The existing ConverseView trace
  fallback stays verbatim.
- The **native-subagent identity** question (SubagentStart/Stop hooks, codex
  collab agents as legible children) → gets a **spike**. Nothing here touches
  the generated hook set.

## Design laws in force

- **Simple core**: the projections (`code_projection.rs`, `comms_view.py`) are
  userland read-models; no kernel special cases, no new magic strings. All API
  changes are additive/backward-compatible.
- **Safety = audit**: nothing here gates; M2 records a fact, M3 reports a fact.
- **Honest UI over clever UI**: `? / ?` becomes explicit "model unknown" /
  "effort not supplied"; History's `available` stops meaning "a file exists".

## Wonky bits — decided (Fable rulings, 2026-07-11)

1. **launched_by scope = option (b).** The child cannot cheaply learn the
   parent's tool-call event id (that id is minted by the parent's recorder for
   the very Bash call the child runs inside). So: record `launched_by_event`
   **only where the lanius seam already carries an id** (the spawn path's
   `code-spawn-*` correlation, codeagent.rs:1075), null elsewhere; the UI keeps
   window-reconstruction (parent session + launch timestamp) for the rest. The
   TRUE causal edge — an env handshake where the parent's exec hook exports the
   current tool-call id into the child — is **deferred to the
   native-subagent-identity spike** (same hook surface, one decision).
2. **History liveness = reuse the query path, no new probe verb.** A dedicated
   liveness request in the history package would be scope creep. Probe with the
   existing query endpoint and a trivial limit, short timeout, and a **2–3s
   in-process cache on the web relay** so `/api/status` polling can't hammer
   the actor. The API reports **three distinct states**: reachable /
   endpoint-exists-but-dead / absent.
3. **Parent purpose = the parent's intent TEXT on child rows** — purpose over
   plumbing ("Claude Opus planned the change, then launched GPT-5.6/high to
   implement it" — the bug doc's thesis). Truncate ~80 chars on the row; full
   text in detail; parent id stays available as secondary text/tooltip.
4. **Intent enrichment is a JOIN, not a new fold.** Baseline intent already
   lives durably in `code_sessions.intent` (written at launch,
   codeagent.rs:3884 `codesession::set_intent`; codesession.rs:480-509). The
   Runs projection (`code_session_stats`, code_projection.rs:31-48) is a
   *different table* — enrich by reading `code_sessions.intent` and filling the
   stat, NOT by adding an intent arm to `match leaf`. **Follow the established
   neighbor idiom, not a hand-rolled SQL JOIN:** `native_overrides` +
   `fill_native` (code_projection.rs:759-790) already query `code_sessions`
   into a `HashMap<elanus_session, native_session>` and backfill each
   `SessionStat` — extend that same map (or add a parallel `intent_overrides`)
   to carry intent and fill a new `SessionStat.intent`. Only `args` (session/
   start payload, codeagent.rs:3905-3939) has no durable home → one new
   nullable column on `code_session_stats`.
5. **Test attribution is per-language.** The Runs projection is Rust with
   inline tests (`code_projection.rs:989` `mod tests`, cargo test). Python
   projection tests (`kits/stdlib/packages/comms/scripts/test_comms_view.py`)
   belong to M4's comms-thread signal only. Don't hunt for python tests for M1.
6. **Rename scope**: visible copy now; internal identifiers (`AgentTab`
   `'converse'`, `view-converse`, route helpers, test selectors — the bug doc
   counted 157 case-insensitive uses) only if genuinely one-line cheap. The
   browser URL is already `/agents/:agent`, so no route migration exists.

## Milestones

Each independently landable, one scoped commit each.

### M1 — Runs identity enrichment (projection JOIN + API + UI)

The projection hands the API an identity-stripped record while the sources
carry purpose (bug doc: "the largest immediate loss happens in the
projection"; code_projection.rs:31-56, 263-289).

- **Intent**: LEFT JOIN `code_sessions.intent` on `elanus_session` into the
  list/tree/detail queries feeding `SessionStat` / `SessionDetail`
  (code_projection.rs:70-133). New optional `intent` field on both wire
  shapes (additive; absent/null when never recorded).
- **Args**: one new nullable `args` column on `code_session_stats`, populated
  in the existing `session/start` arm (code_projection.rs:262-290) from the
  payload's `args` (serialize the array to JSON text). Surfaced in **detail
  only** — args can contain the full launch prompt; keep it off collapsed
  rows. `CREATE TABLE IF NOT EXISTS` needs an idempotent `ALTER TABLE … ADD
  COLUMN` guard for existing projections. **The established neighbor idiom is
  in db.rs:585-642** — `let _ = conn.execute("ALTER TABLE code_session_stats
  ADD COLUMN args TEXT", []);` immediately after the `CREATE TABLE IF NOT
  EXISTS` in `init_schema` (code_projection.rs:28-66); the fire-and-ignore
  return swallows the "duplicate column" error on an already-migrated db. Do
  NOT invent a PRAGMA-check or a rebuild-from-cursor path.
- **UI promotion (ui/web/src/CodeSessions.tsx)** — intent/model/effort become
  PRIMARY identity text:
  - every collapsed row (list + tree children, ~line 113 and the child rows
    ~line 807): intent (truncated, tooltip full), model + effort;
  - child rows additionally show the **parent's intent text** (~80 chars,
    ruling 3), parent id demoted to secondary/tooltip (detail already shows it
    at ~line 732);
  - detail heading (~lines 717-732): intent as the heading line; args in a
    disclosure row;
  - **`? / ?` dies at both sites** (lines 114, 718): render "model unknown"
    and/or "effort not supplied" per missing fact — never a bare `?`.
- The client-side live-merge path in CodeSessions.tsx (~lines 171-311) already
  folds session/start payloads; extend it with `intent` so a live row is as
  legible as a projected one.

**Acceptance**: `/api/code/sessions` list and detail return `intent` (and
detail returns `args`) for a session launched with a prompt; a session with no
recorded model/effort renders the explicit unknown wording, not `? / ?`; a
child row shows its own intent AND its parent's intent text. All existing
consumers unbroken (fields additive).
**Tests**: cargo unit in `code_projection::tests` — JOIN returns intent;
args column round-trips through `session/start`; null model/effort stays null
on the wire (the UI owns the wording). ui.spec.mjs — a runs row shows the
intent text and the explicit-unknown wording; detail shows args.

### M2 — launched_by durable edge (scoped per ruling 1)

- New nullable `launched_by_event` column on the **durable child session
  record** (`code_sessions`, owned by db.rs `init_schema` — NOT the derived
  projection; the projection may JOIN it into detail if free). Add via the
  same `let _ = conn.execute("ALTER TABLE code_sessions ADD COLUMN
  launched_by_event TEXT", []);` idiom already used for every other
  `code_sessions` column growth (db.rs:585-642).
- Populate at the lanius seam where an id already exists: `spawn`
  (codeagent.rs:1051, correlation minted `code-spawn-{}` at 1075, env forward
  1102-1103) already mints `code-spawn-<uuid>` and passes the reply
  correlation; the child's `launch()` (codeagent.rs:4106-4109, ENV_REPLY_TO
  read ~4041-4042) derives its parent — record the spawn correlation
  alongside, as the launch-edge fact. Blocking nested launches (no correlation
  exists) record null.
- Also stamp it into the `session/start` observation payload
  (codeagent.rs:3905-3939, published at 3939) so the trace stays the source of
  truth (record-not-gate).
- UI: where present, detail's "spawned workers" / parent linkage uses the
  explicit edge; where null, the existing reconstruction (parent + timestamp
  window) stays — no fake precision.

**Acceptance**: a `lanius code spawn` child's durable record carries the spawn
correlation as `launched_by_event`; a direct nested launch records null; no
schema change beyond the one column; broker/ACL untouched.
**Tests**: cargo unit — spawn path records the edge, nested path records null;
`session/start` payload carries it.

### M3 — History honesty (liveness + copy)

`/api/status` says `available: true` when only `run/pkg-history/http.json`
exists (web.rs:497-534, `history_endpoint` web.rs:2257-2262) while the actual
proxy 503s (web.rs:1179-1223).

- **Probe** (ruling 2): on status assembly, if the endpoint file exists, issue
  the existing query with a trivial limit (e.g. `{"kind":"agents","limit":"1"}`
  through the same `proxy_history` path) under a short timeout; cache the
  verdict in-process 2–3s (a `Mutex<(Instant, state)>` on the Hub is enough).
- **Wire shape**: `history.available` becomes the honest liveness boolean;
  add `history.state` ∈ `"reachable" | "unreachable" | "absent"` (absent = no
  http.json; unreachable = file exists, actor dead). Keep `endpoint` and
  `grant` unchanged — grant remains the revoked-vs-parked discriminator
  (web.rs:526-530 comment still holds). Mirror nothing onto comms in this
  handoff (same disease, but scope discipline — note it in the Log if trivial
  to do both, ask first).
- **UI copy**: the History pane + the Nav hint (Nav.tsx:82) explain that
  History is **transcript reconstruction from the history package**, not
  archived Activity; the unreachable state says the package/daemon is down
  (the existing `lanius daemon` hint), the absent state says the package isn't
  running/approved. SessionsView.tsx keeps its shape; only copy and the
  state-word plumb change.

**Acceptance**: with the endpoint file present but the actor dead, `/api/status`
reports `available: false, state: "unreachable"`; with the actor answering,
`reachable`; with no file, `absent`. Status polling under the cache window
issues at most one probe per 2–3s. UI shows the reconstruction explanation.
**Tests**: cargo unit for the three-state classification (file absent / file
present + connect refused / stub answering) and the cache (second call within
the window doesn't re-probe). ui.spec.mjs — copy assertion for the
reconstruction wording.

### M4 — Chat-first default where a chat exists (GATED on the DM sprint)

Opening a worker hard-selects telemetry (Nav.tsx:13-18, 49-76 — line 76 is
`selectAgent(name, 'telemetry')`; App.tsx:323-330 defaults new agents to
`converse`). The worker-dm-unification sprint
([worker-dm-unification.md](worker-dm-unification.md)) is landing worker DM
threads into the same conversation list (its App.tsx ~552 already loads
conversations for worker nouns). Don't fight it — build on it.

- Nav.tsx stops hard-forcing `'telemetry'` for workers: pass no tab and let
  selection resolve.
- Default resolution: when the comms projection has ≥1 conversation for the
  worker noun (the same `conversations` state Nav already receives), the
  default tab is Chat; otherwise fall through to ConverseView's **existing
  trace fallback unchanged** (ConverseView.tsx:47-84 — the traffic-only home
  redesign is the deferred journey). Activity stays one click away in the tab
  strip either way.
- Decide from data the projection returns, not from re-deriving "is this a
  worker" beyond what Nav already does — no new taxonomy.

**Acceptance**: a worker noun with a DM thread opens on Chat; a traffic-only
worker opens on the trace fallback exactly as today; Activity reachable in one
click from both; no flash-of-wrong-tab while conversations load (respect
ConverseView's existing resolving guard).
**Tests**: python `test_comms_view.py` already covers the thread-fold (M4 of
the DM handoff); this milestone adds ui.spec.mjs — worker with a thread lands
on Chat, worker without lands on trace fallback.

### M5 — Chat rename (visible copy)

- Visible labels, a11y text (`aria-label`s, IconButton labels), and the
  welcome action: Converse → **Chat**.
- Inventory per the bug doc's list (labels, helper/navigation tool schemas
  exposing `converse` as an enum, tests/selectors, docs): rename **visible
  product words only**. Internal identifiers (`AgentTab` value `'converse'`,
  `view-converse`, route helpers) stay unless the rename is genuinely one-line
  cheap with zero behavioral surface (ruling 6). Where a helper tool schema
  exposes the word to an agent, treat the enum value as an identifier
  (compat) and the description text as copy (rename).
- Update e2e selectors that assert visible copy.

**Acceptance**: no user-visible "Converse" remains in the SPA (grep the built
dist for the rendered strings); tests green; routes and wire shapes unchanged.
**Tests**: ui.spec.mjs label assertions updated; the #8 UI assertions pass.

## Sequencing & gate

**GATE OPEN (2026-07-12).** The worker-dm-unification sprint landed (107c332)
and reliability Phase B landed (24ecdfb); the working tree is clean.
Implementation proceeds.

**Landing order (SEQUENCING CONSTRAINT from the QB):** M1 → M3 → M5 → M4 first
(none touch src/codeagent.rs), then **M2 LAST**. Another agent (Sol) may land a
small #11 patch in src/codeagent.rs (resume child env injection); M2 is the only
milestone that edits codeagent.rs/codesession.rs. Before dispatching M2, check
`git status --short src/codeagent.rs` — if dirty with someone else's in-flight
work, **DEFER M2 entirely** (note it here + in chainlink #8) rather than
colliding. M4 depends *functionally* on the DM sprint's projection, now live.

Build ritual for anything touching the SPA: `npm run build` → cargo build
(build.rs embed-freshness handles staleness) → run ui.spec.mjs against the
Rust server.

**Model tiering** (Tim's ruling 2026-07-11): impl workers on **Sonnet 5**
where possible; verifier **Opus high**. Planning stays Claude/Fable per the
handoff-workflow skill.

## Deferred (and why)

- **Worker home redesign / semantic Activity grouping / count reconciliation**
  → journey. Taste-level; the bug doc supplies constraints, not a wireframe.
- **Native-subagent identity** (hook subscriptions, codex collab children,
  AND the option-(a) env handshake for a true `launched_by` tool-call edge) →
  spike. One hook-surface decision, made once.
- **Comms liveness probe parity** with M3 — same disease; ask before folding in.
- **Identifier-level converse rename** (the ~157-use cleanup) — only if free.
- **History's long-term survival as a pane** (bug doc open question 5) — not
  decided here; M3 only makes the current pane honest.

## Read these first

- [docs/bugs/worker-surfaces.md](../bugs/worker-surfaces.md) — the evidence
  base; every anchor here was verified against it.
- [docs/handoffs/worker-dm-unification.md](worker-dm-unification.md) — the
  in-flight sprint M4 builds on (and must not fight).
- `src/code_projection.rs` (schema 31-66, wire shapes 68-133, fold 262-366,
  tests 989+), `src/codesession.rs` (intent 471-538), `src/codeagent.rs`
  (spawn 1041-1105, launch-parent 4050-4061, session/start 3845-3876),
  `src/web.rs` (status 497-534, history proxy 1179-1223, history_endpoint
  2257-2262), `ui/web/src/CodeSessions.tsx`, `ui/web/src/views/Nav.tsx`,
  `ui/web/src/views/ConverseView.tsx` (trace fallback 47-84),
  `ui/web/src/App.tsx` (selectAgent 323-330).

## Log

- 2026-07-11 (planner, session c185ae6f): status → proposed. Milestone
  structure + rulings from Fable: W1 = option (b) (seam-carried ids only, env
  handshake deferred to the native-subagent spike); W2 = reuse the query path
  with trivial limit + 2-3s relay-side cache, three-state wire word; W3 =
  parent intent text ~80ch on child rows, id secondary. Test attribution
  corrected: Runs projection tests are Rust/cargo, python tests belong to the
  comms fold only. **GATE: no implementation until the worker-dm-unification
  diff is committed** — the unstaged tree spans the same files. Impl tier:
  Sonnet 5; verify: Opus high.
- 2026-07-12 (planner, session a39b40fa, for Fable): status → in-progress.
  **GATE OPENED** — DM-unification (107c332) + reliability Phase B (24ecdfb)
  landed, tree clean. Anchors re-verified: M1/M3/M4/M5 within tolerance; **all
  drift is in src/codeagent.rs** (M2 territory), pushed ~+50-100 lines by the
  two sprints — session/start now 3905-3939 (published 3939), intent publish
  3885-3890, `set_intent` write 3884, spawn correlation 1075 / env-forward
  1102-1103, `launch()` parent 4106-4109. Anchors above updated. Two idiom
  refinements baked in: (a) M1 intent enrichment follows the existing
  `native_overrides`/`fill_native` map idiom (code_projection.rs:759-790), not
  a raw SQL JOIN; (b) `args` + `launched_by_event` columns use the db.rs
  `let _ = execute("ALTER TABLE … ADD COLUMN …")` fire-and-ignore idiom
  (db.rs:585-642). **Sequencing per QB: M1→M3→M5→M4 first (no codeagent.rs),
  M2 LAST and only if `git status src/codeagent.rs` is clean (Sol may land #11
  there) — else DEFER.**
- 2026-07-13 (implementation + verification): status → done. Implemented M1-M5
  in tree: Runs list/detail now carry intent, explicit missing model/effort
  wording, detail-only launch args, parent intent on child rows, and the
  `launched_by_event` durable edge; `/api/status` reports history
  `reachable`/`unreachable`/`absent` through a short cached probe; worker nav
  defaults to Chat when a comms thread exists; visible Converse copy is now
  Chat. Verification: `npm run build` passed; `cargo test --lib` passed
  (631/0); targeted M1/M2/M3 Rust tests passed; `node test/ui.spec.mjs`
  passed all new #8 assertions and ended with one pre-existing/unrelated
  provider reload failure (`providers: configure reload shows the saved named
  provider`), after the same flow had already proven provider JSON/TOML
  persistence.

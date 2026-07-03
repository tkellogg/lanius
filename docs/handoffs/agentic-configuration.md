---
status: planned
author: Fable (planner), from Tim's journey
last-updated: 2026-07-03
---

# Agentic configuration — the helper agent

> Journey: `docs/journeys/15-agentic-configuration.md`. An agent that has access
> to everything the UI has access to: reads transparent, mutations careful. Its
> charter is to get you set up; once you're set up, it shifts to merely helping.
> It must not require API billing if the user already has a Claude Code / Codex /
> opencode login — and it must not cause an "oh shit" moment if they don't want
> API billing at all.

The striking thing the exploration found: **almost every seam this journey needs
already exists.** Profile blocks with seed-once/stored-wins semantics
(`src/context_store.rs:533`), KB pointer frontmatter on blocks
(`src/render.rs:187-192`, live example
`kits/core/profiles/architect/blocks/10-kb-llm-strengths.md`), the `[kb]` marker
+ `kb write` runtime path (`src/kb.rs:141,194`), the `[[tool]]` seam feeding
`search_knowledge` (`src/pkgtool.rs`), a reusable web chat component with
client-side tools (`ui/web/src/components/AgentAssistant.tsx`), a synthetic
`helper` profile the server already injects (`src/web.rs:2120`
`profiles_with_helper`), and multi-turn headless coding-session resume
(`src/codeagent.rs:7968` `resume_capture`). This handoff is mostly *assembly*,
plus one genuinely new kernel capability (M4).

## Wonky bits / decisions to confirm

1. **Ship as a new stock kit `kits/helper`, not into `kits/core`.** The helper
   is a product surface, not harness doctrine; a kit is also our only grouping
   mechanism (no package dependencies — the journey accepts this). `elanus init`
   already seeds every stock kit (`src/initcmd.rs:141-188`), so it arrives
   pending-approval out of the box. The kit carries the profile *and* both KB
   packages *and* the exec handler, so "approve the helper kit" is one gesture
   that brings the whole organism.

2. **The real profile shadows the synthetic one.** `profiles_with_helper`
   (`src/web.rs:2120`) synthesizes a `helper` profile mirroring `default` when
   none exists on disk. Keep that as the degraded fallback; when
   `profiles/helper/` exists (kit installed), the on-disk profile must win and
   no duplicate row may appear. (Verify: the current code clones `default` only
   when absent — confirm and add a test.)

3. **The user-KB ships as an empty `[kb]` package, because there is no runtime
   "create a KB" primitive.** `elanus kb write` only writes into an existing
   `[kb]`-marked package (`src/kbcli.rs:85`). Rather than build `kb new`, ship
   `packages/kb-user/` with an empty `kb/` (a README page saying what belongs:
   who the user is, what they're trying to do with the platform, *why* they
   configure things the way they do — the config itself is already on disk).
   The helper fills it via `kb write kb-user …`. Building a runtime KB-creation
   verb is deliberately deferred.

4. **No package dependencies — tolerated, per the journey.** The charter blocks
   point at kb-elanus pages, and `search_knowledge` only exists if `kb-search`
   is approved. Blocks already tolerate missing/stale pointers (pointer is a
   courtesy, summary is the payload — `docs/handoffs/kb-core.md` wonky bit 4),
   and the charter tells the agent to fall back to `elanus kb search` / raw
   grep. The helper's charter also names `kb-search` as a thing to suggest
   enabling (via `find_capability`). No dependency mechanism gets built here.

5. **Activation: side panel first, setup-chat second.** The journey offers two
   activations. Do the right-side panel toggled by an AI button in the mast
   (`ui/web/src/App.tsx:1320-1334`) as the always-available surface — it works
   from any view, which is what "helping" (post-setup) needs. Then make
   `SetupView` (`App.tsx:1567`) *offer* the chat prominently when the helper is
   ready, keeping the existing form wizard as the fallback path. Full
   "setup tab defaults to chat, current UI becomes a sub-tab" is deferred until
   the panel proves the interaction.

6. **The LLM bootstrap paradox resolves by detection order.** The helper needs
   an LLM before it can help you set up an LLM. Resolution: a deterministic
   (no-LLM) detection step decides which of three worlds we're in —
   (a) a dispatcher-usable ApiKey provider exists → helper runs native;
   (b) no provider, but a logged-in coding CLI exists (`claude`/`codex`/
   `opencode` on PATH) → helper runs harness-backed (M4);
   (c) neither → the *static* SetupView guides provider creation (M3), with the
   cheap-provider suggestions, and the helper lights up afterward. No dead ends,
   no billing surprise.

7. **Harness-backed turns (M4) are scoped to the helper, not generalized.**
   Backing arbitrary native agents with a coding harness is a kernel-sized
   feature (it forks the dispatcher's execution path). Here we build the
   narrowest version that makes world (b) real, prove it with a spike first,
   and write the generalization up as its own follow-up handoff if it earns it.
   Note the strategic rhyme: this is the same decoupling the ACP note in
   `docs/_questions.md` points at.

8. **Naming.** The journey says "Lanius"; the rename is its own questions-file
   item. Everything here uses `elanus` and inherits the rename when it happens.

## Milestones

### M1 — the `helper` kit: charter, progress, and the two KBs

`kits/helper/` containing:

- **`profiles/helper/`** — `profile.toml` (agent noun `helper`, owner-scoped,
  model mirrors default; `[skills]` visibility tuned to the helper's diet) and
  `blocks/`:
  - `00-charter.md` — the goal block. Two-phase charter: *while setup is
    incomplete, your goal is to get the human set up; once the progress block
    reads done, your goal is to help.* Reads transparent, mutations careful:
    reads go through `shell` (`elanus status`, `config get`, `packages`,
    `kb list`, `agent catalog`, `history`, …) exactly as the web UI's routes
    shell the same CLI; mutations ride the config-proposal / `elanus approve`
    flow — never silent. Uses `{{today}}`/`{{profile}}` vars like the architect
    charter (`kits/core/profiles/architect/blocks/00-*.md`).
  - `10-setup-progress.md` — the task-list block, seeded with the setup
    checklist (broker up, owner credential, LLM path chosen, first agent
    created, first package approved, KB search enabled, …) each `[ ]`/`[x]`.
    Seed-once/stored-wins makes it agent-maintained after first render; the
    charter instructs the helper to update it via `elanus block set` after each
    completed step (scope=agent, owner=helper).
  - `20-kb-elanus.md` — pointer block with `{kb, path, lines, sha}` JSON
    frontmatter into the kb-elanus overview page, inline summary as payload
    (mirror `10-kb-llm-strengths.md`).
- **`packages/kb-elanus/`** — `[kb]` marker + `kb/` pages: platform overview
  (topic planes, mailboxes, grants); kits-and-packages (what approving means);
  llm-access (the three worlds from wonky bit 6; suggested cheap API providers:
  Fireworks, OpenRouter, DeepSeek, Z.ai; what each is good for); model-guidance
  (capability floor: flag likely-underpowered picks — small parameter counts,
  "mini"/"nano"/"lite" tiers — and what degrades); setup-checklist (the long
  form behind the progress block); mutation-doctrine (reads transparent, writes
  gated).
- **`packages/kb-user/`** — `[kb]` marker, `kb/README.md` only (what belongs
  here, per wonky bit 3).
- **`packages/helper-chat/`** — the exec-handler package that makes the profile
  spawnable, mirroring the `chat`/`kb-pipeline` pattern
  (`kits/core/packages/kb-pipeline/elanus.toml`).
- `src/web.rs` `profiles_with_helper`: on-disk profile wins (wonky bit 2).

**Acceptance:** fresh root → `elanus init` → approve the helper kit →
`elanus agent catalog` lists `helper`; a rendered turn's system prompt contains
charter + progress + pointer blocks in priority order; `elanus kb list` shows
`kb-elanus` and `kb-user`; with kb-search approved, `search_knowledge` returns
kb-elanus pages; `elanus kb write kb-user kb/owner.md …` as the helper succeeds
and commits; `GET /api/admin/agents` shows exactly one `helper` (the real one).

### M2 — UI activation: the AI panel + setup offers chat

- An **AI button** in the mast-right cluster (`App.tsx:1320-1334`) toggles a
  right-side panel (not a modal) mounting `AgentAssistant` with
  `profile='helper'`, available from every view. Panel open/closed persists in
  localStorage.
- **Client tools** for "access to everything the UI has": start with reads +
  navigation — `get_status`, `list_agents`, `list_packages`, `list_providers`,
  `read_conversation`, and `navigate` (drives `sel`, so the helper can *take
  you* to the thing it's describing). Follow the `contextAuthorTools` wiring
  (`App.tsx:2101`). Mutations stay server-side behind the existing gated flows;
  the panel adds no new write authority.
- **`SetupView`**: when detection (M3) says the helper is runnable, render a
  prominent "set up by chatting" entry that opens the panel pre-seeded with a
  first message; the form wizard remains below as the fallback.

**Acceptance:** playwright `ui.spec.mjs` additions — panel toggles from every
`sel.kind`; a helper reply that calls `navigate` switches the view; client-tool
call/result round-trips over `obs/agent/helper/<session>/tool/*`. Build
discipline: `npm run build` → `touch src/web.rs && cargo build` before the e2e
run (the embedded-dist staleness trap).

### M3 — LLM acquisition: detection + the no-dead-end setup path

- **Deterministic detection** (no LLM required), surfaced in `GET /api/status`
  (or a small `/api/setup/llm`): `{ native: <first dispatcher-usable ApiKey
  provider or null>, harness: [<logged-in coding tools on PATH>], world:
  a|b|c }`. Reuse the provider vault + `Consumer::Dispatcher` validity
  (`src/provider.rs:241`) and the harness detection codeagent already does.
- **World (c) static path:** `SetupView` walks provider creation — reuse
  `ProvidersView` add/test (key over STDIN, reachability probe
  `src/models.rs::probe`) and render the cheap-provider suggestions (content
  sourced from the kb-elanus llm-access page so agent and UI tell one story).
- **Underpowered-model advisory:** a soft warning in `ModelField` / the wizard
  when the chosen model name matches a small-capability heuristic (`\d+[bB]`
  ≤ ~14B, `mini|nano|lite|flash-lite|tiny`), phrased as "may struggle with
  agent work," never a block. Same heuristic exposed to the helper via the
  model-guidance KB page.

**Acceptance:** with no providers and no CLIs, SetupView shows the guided
provider path and the AI button explains what's missing instead of dying; with
only a logged-in `claude`, detection reports world (b); adding a valid ApiKey
provider flips detection to (a) live (SSE/status refresh); the advisory fires
on `some-7b-model` and not on `claude-opus-4-8`.

### M4 — harness-backed helper turns (spike first)

World (b): the helper's turns execute via a headless coding session instead of
the genai dispatcher.

- **Spike (gate for the rest):** by hand, prove a two-turn helper conversation
  over `elanus code claude --headless` + `resume_capture`
  (`src/codeagent.rs:7968`) where the worker (i) reads status via the elanus
  CLI, (ii) updates the progress block, (iii) writes a kb-user page — with
  elanus reachable from inside the cage via MCP-on-launch
  (`src/codeagent.rs:3902-3925`) or the skill plugin. Record turn latency.
  If latency or the tool loop is unacceptable, stop and re-plan; the fallback
  is world (c) only.
- **Build:** a routing seam in the dispatcher: when the helper profile's model
  resolves to no dispatcher-usable provider but detection has a harness, a
  mailbox delivery to `in/agent/helper` starts/resumes a headless coding
  session (charter + blocks projected via the existing `turn_injection()`
  coding-agent block projection; final text returns as the correlated
  `in/human/<owner>` reply, exactly as the native path does). Reuse the
  `drive_code_deliveries`/`route_completion` machinery (`src/dispatcher.rs`)
  rather than inventing a second resume loop. Scope: the helper profile only;
  an explicit profile opt-in key (e.g. `[model] harness_fallback = true`) so
  nothing else changes behavior.
- **Follow-up handoff, not here:** generalizing harness-backed execution to all
  native agents (and its ACP intersection).

**Acceptance:** on a machine with no ApiKey provider and a logged-in claude
CLI, the web AI panel produces a real multi-turn helper conversation (blocks
consulted, progress block updated, user-KB written) with zero API-key
configuration; the session appears in code-sessions observability like any
worker; killing the CLI mid-turn produces the failure-mail contract, not a
hang.

## Read these first

- `docs/journeys/15-agentic-configuration.md` — the intent (reason against it)
- `docs/handoffs/memory-blocks.md` — blocks substrate, seed-once/stored-wins,
  `turn_injection()`
- `docs/handoffs/kb-core.md`, `kb-search.md` — KB anatomy, pointer meta, the
  `[[tool]]` seam
- `docs/handoffs/model-providers.md` — provider vault, validity matrix,
  ProvidersView
- `docs/handoffs/coding-agents.md` (planner symmetry §), `mcp-on-launch.md`,
  `coding-skill-materialization.md` — the M4 ingredients
- `ui/web/src/components/AgentAssistant.tsx` + `App.tsx:2101` — the chat panel
  and client-tool wiring to copy
- `kits/core/profiles/architect/` — the charter + pointer-block idiom to mirror

## Residuals (named, deferred)

- Package dependency mechanism — avoided by kit grouping (wonky bit 4).
- Runtime `kb new` — avoided by shipping kb-user empty (wonky bit 3).
- Setup tab fully chat-first (UI becomes a sub-tab) — after the panel proves it.
- Harness-backed execution for arbitrary agents + ACP convergence — follow-up
  handoff after the M4 spike.
- Lanius rename — separate item.

## Log

- 2026-07-03 (Fable): explored web UI / packages+blocks+KB / LLM paths; wrote
  this handoff. Key finds: synthetic `helper` profile already exists
  (`web.rs:2120`); `AgentAssistant` client-tool loop already round-trips over
  the bus; dispatcher hard-refuses NativeLogin (`provider.rs:266-269`) so
  world (b) genuinely requires M4; no model-capability flagging exists anywhere
  yet.

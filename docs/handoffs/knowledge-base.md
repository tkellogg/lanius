---
status: done
author: Opus (planner) under Fable; Tim's design review folded in 2026-07-02
last-updated: 2026-07-02
---

# Handoff: the knowledge base as a first-class object (design)

**Design-only.** Implementation is sprint 4; this records the design
decisions and a build order, not code milestones. From Tim's journey 14
([../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md),
"tell the agent"): *"knowledge bases — we haven't added this yet as a 1st
class object, but a searchable KB can be better than skills, sometimes.
Likely a good approach might be to have a knowledge base, and then use memory
blocks on certain agents to bring certain concepts into high awareness by
providing file + line references."* Plus the backlog's LLM-strengths thread
(which model for what — currently living in `.claude/skills/handoff-workflow/
SKILL.md:81-96` and Tim's own notes, i.e. in *Claude's* config, invisible to
elanus agents).

The design stance: **compose the KB from four substrates that already ship**
rather than building a knowledge subsystem. The storage research probe
([../notes-scaling-and-storage.md](../notes-scaling-and-storage.md) §3c)
independently reached the same fork and should be read alongside.

## The design decisions

### D1 — Storage: the corpus is a `kb/` subfolder any package may carry; the pointers are memory blocks
**[TIM 2026-07-02]** Refined from one-package-per-KB to: **a KB is a `kb/`
subfolder inside a package** — any package can carry one. A dedicated
knowledge package (`kb-llm-strengths`) is just the degenerate case (a package
that is *mostly* its `kb/`). This keeps the building-block nature: enable a
package and its knowledge comes with it (a `discord` package could ship
`kb/discord-api-notes.md` beside its scripts). Plain markdown files, one
topic per file, file+line anchors per journey 14, greppable for free.

**Local vs global editability = packages' copy-vs-link install.** A *linked*
package's `kb/` is the shared canonical copy (edits visible everywhere it's
linked); a *copied* install is a local fork the agent may freely rework. No
new machinery — the install mode already expresses the ownership intent.

- **Pointers = `context_blocks`.** A "high-awareness concept" is a vanilla
  memory block (`ContextBlock`, `src/context_blocks.rs:31-43`) whose
  `content` is the concept summary and whose `meta` (`:42`, free JSON,
  currently unused for this) carries `{kb, path, lines, sha}`. It rides the
  existing render pipeline, `elanus block` CLI (`src/blockcli.rs:85-189`),
  priority/placement, and build-log audit (`src/db.rs:327-343`) with **zero
  schema change**. The `sha` lets a consolidation pass detect a stale pointer
  (file changed under the block). **[TIM] Deliberately the stopping point:**
  pointers-as-blocks is the essence of search and ambient associative
  memory — a deep topic in its own right — and blocks-with-`meta` is enough
  to work with today; the deeper associative machinery is future work, not
  sprint 4.
- **Rejected:** sqlite-rows-as-corpus (blocks are prompt-sized context
  material, not a browsable corpus); Dolt/DoltLite on paper (research doc
  §3c: merge sweet spot doesn't arise, Rust embedding weak — but see wonky
  bit 5, Tim wants a hands-on spike during the build); a dedicated
  config-repo-style git repo *separate from the packages* (the kb/ trees get
  git per D2, but as part of the package layout, not a second reviewed
  config plane).

### D2 — Authority: sandboxes gate writes; provenance is just git commits
**[TIM 2026-07-02 — replaces the proposal-gated design.]** "We just leverage
sandboxes. If sandboxes can't work here, then we haven't done sandboxes
well." Writing a `kb/` is a plain file write, gated by the agent's sandbox
grant (`fs_write` on that package's tree — the existing cage, nothing
custom). A *copied* (local) KB is inside the agent's own writable world; a
*linked* (shared) KB needs the grant. The human has global edit permission by
construction of how sandboxes are set up — nothing custom on top.

**Provenance = git commits.** No provenance footers, no accept-time
verification machinery. The `kb/` trees live under git (which also buys
remote backup — a property Tim explicitly values); who-changed-what-when is
the commit log plus the ledger's ordinary obs trail. Tim holds the door open
on git *specifically* being wrong, but the shape (a versioned store with
cheap remote replication) is settled. The review layer that the old
proposal design supplied now lives in D4's ratification pipeline instead —
review-by-consolidation, not review-at-write.
- **Fast lane unchanged:** an agent's own scratch knowledge stays in
  `notes/`/agent blocks; the ladder is notes (mine) → kb/ (ours,
  sandbox-gated + git-logged) → memory block (high-availability pointer).

### D3 — Search: a package daemon, tool-first, FTS by default, indexer swappable
**[TIM 2026-07-02 — replaces the grep-first lean.]** Search is **absolutely
and only a package daemon** — never kernel code — precisely so anyone can
override the indexing and experiment without ceremony: replacing search is
swapping a package. The primary agent surface is a **tool** (not just a CLI)
for the same reason: tools are what a swapped-in indexer package can supply
uniformly; a skill + thin CLI ride alongside for journey-14 availability.
- **Default indexing is FTS, not grep.** Tim's ruling: solid FTS (or
  FTS+vector) is genuinely good, and with the corpus deliberately spread
  across many packages' `kb/` folders (D1), ripgrep-across-the-union is the
  clunky option, not the simple one. The stock `kb-search` daemon indexes the
  union of every enabled package's `kb/` into its own package-local FTS5
  index (sqlite in the package's state dir — still zero kernel schema).
- **Multi-vector as a secondary package.** The best-known retrieval
  (multi-vector) needs embedding setup, so it ships as an *alternative*
  search package you install instead of the stock one — the swappability
  proof, dogfooded. Same tool name, different engine.
- The resident-stage auto-surface rung stays deferred (expensive, always-on;
  start pull-only).

### D4 — Consolidation: a diff pipeline — cheap compactor proposes, strong ratifier approves
**[TIM 2026-07-02 — the shape is a list of unified diffs, two LLM stages.]**
Script checks stay rung 1 (variety ladder: link validity via `meta.{path,
lines,sha}`, orphans, staleness — no LLM). Then the pipeline:
1. **A cheap ambient compactor agent** sweeps the corpus and produces
   **unified diffs** (consolidations, link fixes, conflict annotations) — a
   list of diffs is the deliverable, nothing applied.
2. **An expensive high-end agent ratifies** each diff — applies it, or sends
   it back *with feedback* the compactor learns from.

This is deliberately elanus's **first auto-approve pipeline** — the ratifier
standing in for the human at a quality tier the human trusts. Two design
consequences Tim named:
- **The compactor needs memory** — its own memory blocks (what it tried,
  what got bounced, the ratifier's feedback), *not* a KB of its own.
- **This must be SET UP, never default-on**: which two models (both read
  from the LLM-strengths KB, D5), how often it runs, token budgets per pass —
  explicit configuration the human walks through, in the spirit of the
  work-estimation package's opt-in.

### D5 — First tenant: the LLM-strengths KB, mutable, preference-dominated
- Seeded from the model-tiering knowledge that today lives in
  `.claude/skills/handoff-workflow/SKILL.md:81-96` (plan = Claude/Fable only,
  never GPT/GLM; implement = Opus/GPT-5.5/GLM medium; verify = Opus/GPT-5.5
  high, Fable for the hardest; planning never flexes) and Tim's second-level
  model notes — one file per model, plus one file per *role* (planner/
  implementer/verifier) cross-linking them.
- **Mutable by design, preferences dominate:** benchmark claims are the
  floor, Tim's lived experience overrides them — which is exactly why entries
  carry D2's provenance kinds (`human-preference` outranks `benchmark` when
  they conflict, and the consolidation actor flags the conflict rather than
  resolving it). This is also why the KB must be *elanus-visible*, not
  Claude-config: the work-estimation and escalation packages, and any
  dispatching planner, should read the same tiering facts.
- **Reconciliation duty:** the seed must not fork the truth. The
  handoff-workflow SKILL.md keeps its copy (it configures Claude Code, which
  can't read the KB) but gains a pointer line ("canonical copy: the
  llm-strengths KB; update both"); the consolidation actor's link-validity
  pass watches the pair. High-awareness pointers: a memory block on
  dispatching profiles carrying `meta` refs into the role files.

### D6 — What makes it "first-class" (and what doesn't)
First-class means: a **name** (`elanus kb list` shows installed KBs — a
manifest marker on the package, the same move `[[harness]]` made for
harnesses), a **search surface** (D3), a **write path** (D2), and
**teachability** (a `knowledge` skill + CLI help + the pointer-block
pattern, per journey 14). **[TIM]**: first-class is admittedly squishy — the
real bar is that it *feels melded into the ecosystem*: a fairly default
agent simply knows how to use KBs, write to them, and search them, the way
it knows skills. That means the knowledge skill ships in stdlib and the
default profile carries the awareness. It does **not** mean a kernel data
model: no `kb` table, no new topic plane, no kernel code beyond (possibly) a
manifest field — the kernel stays small ([../layering.md](../layering.md)).
If sprint 4 finds it needs a kernel table, that's a design smell to bring
back here.

### D7 — Interconnection & introspection: many packages, one KB feel
**[TIM 2026-07-02 — new decision.]** Two requirements hold in tension:
packages stay building blocks (enable one and its `kb/` becomes available,
disable it and it's gone), yet the full collection must **feel like one KB**.
- **Unification is the search daemon's job (D3):** it indexes the *union* of
  enabled packages' `kb/` folders, so "one KB" is an emergent read-side
  property, not a storage merge. Enabling a package is the whole gesture —
  its knowledge joins the union on the daemon's next index pass.
- **Introspection — the discovery gap:** if an agent *doesn't* have a KB (or
  skill, tool, context program — anything package-carried) enabled, how does
  it learn it *should*? Tim's answer: **one more package**, supplying a
  privileged discovery tool that searches across *available* packages — not
  just enabled ones — covering everything a package can carry. "You don't
  have the discord KB enabled, but it exists and matches your query." This
  generalizes agent-launching's catalog introspection from launching to
  *capability* discovery; the tool is privileged because it reads the
  instance's package universe rather than the agent's own visibility set.
  Requesting enablement then rides the existing config-proposal machinery.

## Wonky bits / open questions for the review

1. ~~Package-files vs one-kb-one-package granularity.~~ **RESOLVED [TIM]:
   a `kb/` subfolder any package may carry (D1); copy-vs-link install
   expresses local-vs-global editability.**
2. ~~Where does accept-time provenance verification run?~~ **RESOLVED [TIM]:
   nowhere — provenance is git commits + the ordinary ledger trail, nothing
   custom (D2). Review happens in D4's ratification pipeline, not at write.**
3. **Block-pointer staleness window.** A pointer block's `lines`/`sha` go
   stale the moment the file is edited; until the groundskeeper's next pass,
   agents may read a mispointed high-awareness block. Acceptable (the block
   carries the concept summary inline; the pointer is for *going deeper*)?
   Or should `elanus kb` writes touch affected pointers synchronously
   (coupling writes to the block store)? *Lean: acceptable + groundskeeper;
   the summary is the payload, the pointer is a courtesy. Confirm.*
4. **Search authority.** Is KB content readable by every agent (homogeneous
   authority says yes) or scoped? Recall gates by correspondent because its
   content is *conversations*; a KB is curated shared knowledge — the whole
   point is availability. *Lean: world-readable within the instance; the
   sensitive-notes use case stays in `notes/`/blocks. Confirm — this is the
   one place D2's "no trust boundary" could bite if a KB ever holds secrets.*
5. **Dolt hands-on spike [TIM, upgraded from a trigger to a task].** The
   paper verdict (research doc §3) stands as the null hypothesis, but Tim
   wants hands on it: during the sprint-4 build, stand the KB corpus up on
   Dolt beside the files+git form and judge for real (see the restored
   `_questions.md` item). Git's remote-backup property is the bar to beat.
6. **Where does the kb git repo live?** D2 settles git-as-provenance but not
   the repo boundary: per-package `kb/` repos, one repo over the packages
   tree, or fold into an existing repo. Small mechanism call for the
   implementer; remote-backup ergonomics should decide it.

## Build order for sprint 4 (acceptance sketches)

1. **B1 — the corpus convention + manifest marker.** `kb/` recognized in any
   package via a manifest `[kb]` marker; `kb-llm-strengths` ships the seeded
   role/model files (D5); `elanus kb list` names every enabled kb. *Accept:
   install on a scratch root, list shows it; the handoff-workflow SKILL.md
   pointer line exists; git init/commit on write proven.*
2. **B2 — search: the FTS daemon + tool.** The `kb-search` package daemon
   indexes the union of enabled packages' `kb/` into package-local FTS5; a
   `search_knowledge` tool (primary), skill + `elanus kb search` CLI
   alongside (D3). *Accept: an agent given only the tool finds the "who
   verifies" answer with file+line from a cold start; enabling another
   kb-carrying package makes its content findable on the next index pass;
   daemon read-only; kill-and-restart safe.*
3. **B3 — pointer blocks.** The `meta` pointer convention + a seeded
   high-awareness block on a dispatching profile (D1/D5). *Accept: `elanus
   context render` shows the block; its `meta` resolves to a real file+line;
   a deliberately broken pointer is detected by B5's checker.*
4. **B4 — write path.** Sandbox-gated direct writes + git commit per write
   (D2), via the knowledge skill's taught pattern (+ optional `elanus kb
   write` convenience). *Accept: an agent with the grant writes and the
   commit records it; without the grant the cage refuses; the log
   reconstructs who-wrote-what; remote backup configured is push-able.*
5. **B5 — groundskeeper, script rung.** Cron package: link/orphan/staleness
   checks, owner report (D4 rung 1). *Accept: seeded breakage classes each
   reported; zero LLM calls.*
6. **B6 — the diff pipeline (setup-gated).** Cheap compactor emits unified
   diffs; strong ratifier applies or bounces with feedback; compactor keeps
   memory blocks of the feedback; explicit setup flow for the two models,
   cadence, and token budgets (D4). *Accept: a seeded contradiction yields a
   diff; a bad diff gets bounced with feedback that lands in the compactor's
   blocks; nothing runs before setup; cost per pass measured and logged.*
7. **B7 — the discovery package.** The privileged available-packages search
   tool (D7). *Accept: an agent lacking the discord package, asked a
   discord-api question, is told the package exists and what enabling it
   would add; the enable request rides the existing proposal flow.*

Order rationale: read before write (B2 proves value with zero risk), pointers
before the checker that validates them, script rung before the LLM pipeline
(variety ladder), discovery last (it needs the union to be worth searching).

## Read these first
- The why: [../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md)
  ("tell the agent" — the four availability tiers; KB named as the missing
  one); [../_questions.md](../_questions.md).
- The storage fork, independently analyzed: [../notes-scaling-and-storage.md](../notes-scaling-and-storage.md)
  §3c (and §2's block-upsert bug — being fixed in
  [storage-hardening.md](storage-hardening.md), a dependency for B3's
  pointer blocks under concurrency).
- The block substrate: `src/context_blocks.rs:31-43` (`meta` at `:42`),
  `src/context_store.rs:370` (`upsert_block`), `:489-512` (`seed_defaults`),
  `src/blockcli.rs:85-189`, `src/db.rs:304-343`; [memory-blocks.md](memory-blocks.md).
- The package/review substrate: `src/config_repo.rs:48-54` (proposals),
  `src/manifest.rs:19-57` (manifest decls — where a `[kb]` marker would sit),
  `src/kit.rs:1-15` (distribution); [../layering.md](../layering.md).
- The patterns to copy: `packages/notes/SKILL.md` (files-as-knowledge),
  `kits/stdlib/packages/history/` (read-only query daemon + DSL,
  `SKILL.md:41-72`, LIKE search `scripts/main:177-206`),
  `packages/recall/SKILL.md:15-25` + `packages/recall/elanus.toml:19-31`
  (provenance rules; resident stage, `src/manifest.rs:205-230`).
- The first tenant's seed: `.claude/skills/handoff-workflow/SKILL.md:81-96`
  (the model-tiering rules), Tim's second-level model notes.

## Log
- 2026-07-07 — Confirmed shipped+merged on main (all four build handoffs — kb-core,
  kb-search, kb-groundskeeper, kb-discovery — merged at `80a23c7`); status flipped
  to `done` (was stale at `planned`). 559 tests green.
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull + journey
  14, as a design-review document (implementation = sprint 4). Grounded
  against the worktree: `meta` on `ContextBlock` is unused free JSON (the
  pointer home), no FTS exists anywhere (search must start as LIKE/grep),
  notes/history/recall supply the three patterns (files-as-knowledge,
  read-only query daemon, provenance + resident stage), and the storage
  research doc independently recommends files+sqlite-pattern over Dolt
  unless shared-entry merge appears. Decisions for the review: corpus as
  package-files + pointers as blocks (D1); proposal-gated shared writes with
  recorded provenance (D2, gate-vs-audit open in wonky 2); pull search now,
  FTS and resident-stage deferred (D3); script-first groundskeeper (D4);
  LLM-strengths as first tenant with preference-over-benchmark provenance
  (D5); first-class without a kernel data model (D6).
- 2026-07-02 (later) — Tim's design review folded in; status → planned for
  sprint 4. Rulings: kb/ is a subfolder any package carries, copy-vs-link =
  local-vs-global (D1); sandboxes gate writes, provenance is git commits,
  nothing custom — the proposal-gated design is replaced, review moves to the
  ratification pipeline (D2); search is a package daemon, tool-first,
  DEFAULT FTS (grep-first overruled), multi-vector as a swappable secondary
  package (D3); consolidation is a two-stage diff pipeline — cheap compactor
  emits unified diffs, strong ratifier applies-or-bounces-with-feedback,
  compactor keeps memory blocks, explicitly setup-gated (models, cadence,
  budgets) — elanus's first auto-approve pipeline (D4); first-class = melded:
  a default agent just knows KBs (D6); NEW D7: the union must feel like one
  KB (search unifies) and a privileged discovery package answers "what
  package should I have?" across everything packages carry. Pointers=blocks
  is deliberately the stopping point — the deeper associative-memory topic is
  future work. Dolt upgraded to a hands-on spike during the build.

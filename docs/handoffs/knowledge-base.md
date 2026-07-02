---
status: design-review
author: Opus (planner) under Fable
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

### D1 — Storage: the corpus is files in a package; the pointers are memory blocks
- **Corpus = a knowledge package** (a directory of markdown files, like
  `packages/notes/` already models: plain files, one topic per file,
  `packages/notes/SKILL.md:6-13`). A KB named `kb-<name>` is a package whose
  payload is its files; multiple KBs are multiple packages. Why files over
  sqlite rows: files give the **file+line anchor** journey 14 asks for; they
  are diffable/reviewable through the config-repo proposal machinery
  (`src/config_repo.rs:48-54` — agents propose on a branch, acceptance is the
  human gesture); they're greppable by any agent with shell access, which is
  most of the read load for free; and the notes skill has already proven the
  shape in daily use.
- **Pointers = `context_blocks`.** A "high-awareness concept" is a vanilla
  memory block (`ContextBlock`, `src/context_blocks.rs:31-43`) whose
  `content` is the concept summary and whose `meta` (`:42`, free JSON,
  currently unused for this) carries `{kb, path, lines, sha}`. It rides the
  existing render pipeline, `elanus block` CLI (`src/blockcli.rs:85-189`),
  priority/placement, and build-log audit (`src/db.rs:327-343`) with **zero
  schema change**. The `sha` lets a consolidation pass detect a stale pointer
  (file changed under the block).
- **Rejected:** sqlite-rows-as-corpus (blocks are prompt-sized context
  material, not a browsable corpus — and `db.rs` has no FTS to make rows
  searchable anyway); Dolt/DoltLite (the research doc's §3c verdict: its
  branch/merge sweet spot only pays if two agents must merge conflicting
  edits to the *same shared entry*, and D2 keeps writes proposal-gated so
  that never arises; its Rust embedding story is a real cost); a new
  config-repo-style dedicated git repo (the config repo already exists and
  packages already live under its review model — a second repo is a second
  thing to harden).

### D2 — Authority: agent-writable via proposals; provenance stamped, recall-style
- **Who may write:** any of the user's agents may *propose* KB edits; the
  write lands through the config-repo proposal→accept path, with the
  acceptance-autonomy comfort setting deciding how much lands unasked
  ([../layering.md](../layering.md) "How adding and proposing actually work").
  This is not a trust boundary between agents (Tim's doctrine: homogeneous
  authority, safety = audit) — it's the same review seam every package edit
  already gets, and it's what keeps a prompt-injected worker from silently
  poisoning shared knowledge: the *change log* is the safety.
- **Provenance:** every KB entry carries a provenance footer (author identity,
  date, source: benchmark/anecdote/human-preference, session/correlation) —
  written by the proposing agent, verified at accept time from the proposal's
  ledger trail, mirroring recall's rule that authority-bearing facts come
  only from kernel-stamped sources, never message bodies
  (`packages/recall/SKILL.md:15-25`). Consumers can weigh "Tim said" over
  "a benchmark claimed".
- **Fast lane:** an agent's *own* scratch knowledge needs no review — that's
  what `notes/` and agent-scope blocks already are. The KB is specifically
  the **shared, load-bearing** tier; the design keeps the ladder legible:
  notes (mine, unreviewed) → KB (ours, proposed+accepted) → memory block
  (high-availability pointer).

### D3 — Search: a read-only query daemon on the history package's pattern; FTS deferred
- Expose search as the **history package's shape** (`kits/stdlib/packages/
  history/` — an approved read-only HTTP daemon + a SKILL.md teaching the
  query DSL): a `kb` query endpoint doing case-insensitive substring/LIKE
  matching over the corpus files + block pointers, returning
  `{file, line, excerpt, provenance}`. Agents reach it three ways, matching
  journey 14's availability ladder: the **skill** (teaches the endpoint +
  that plain grep over the package dir also works), the **CLI** (`elanus kb
  search <query>` as a thin client — CLI-is-the-API), and — for daemon-driven
  agents — optionally a **resident context stage** later (recall's pattern,
  `packages/recall/elanus.toml:19-31`) that auto-surfaces matches; the stage
  is explicitly *not* first-build (it's the expensive, always-on rung; start
  pull-only).
- **No FTS5 yet.** The tree has zero FTS anywhere (grounding: no `fts5`/
  `VIRTUAL TABLE`/`MATCH` in src/; history search is `LIKE '%…%'`,
  `kits/stdlib/packages/history/scripts/main:177-206`). A KB measured in
  dozens-to-hundreds of markdown files doesn't need an index; grep/LIKE is
  variety-matched. FTS5 is named as the *one* net-new substrate to introduce
  **if and when** corpus scale demands it — a contained later change since
  D3's query surface hides the engine.

### D4 — Consolidation: an OOTB package, script-first, cheap-LLM second
Per the variety-ladder doctrine (journey 14: "scripts are Ashby-pilled"),
the consolidation actor is a package (`kb-groundskeeper`) whose recurring
`cron` (the manifest already supports it, `src/manifest.rs:19-57`) runs a
**script pass first**: link validity (do `meta.{path,lines}` block pointers
still resolve? does the anchored text's sha still match?), orphan files
(no inbound pointers), and staleness flags — pure mechanics, no LLM. Only
the judgment calls — conflicting entries, missing links between related
topics, summarize-and-merge suggestions — go to a **cheap-tier LLM turn**
(the tier itself read from the LLM-strengths KB, D5 — the system consults
its own knowledge to staff its own maintenance). Its output is **proposals**
(D2), never direct writes: the groundskeeper is an agent like any other.
Deliverable rungs: (1) script checks + a report mailed to the owner;
(2) cheap-LLM conflict/missing-link proposals; (3) auto-accept for the
script-provable fixes only (dead-pointer repairs), per the autonomy comfort
setting.

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
harnesses), a **search surface** (D3), a **write path with provenance**
(D2), and **teachability** (a `knowledge` skill + CLI help + the pointer-
block pattern, per journey 14). It does **not** mean a kernel data model:
no `kb` table, no new topic plane, no kernel code beyond (possibly) a
manifest field — the kernel stays small ([../layering.md](../layering.md)).
If sprint 4 finds it needs a kernel table, that's a design smell to bring
back here.

## Wonky bits / open questions for the review

1. **Package-files vs one-kb-one-package granularity.** D1 says one package
   per KB. Alternative: one `knowledge/` dir in the root (like `notes/`)
   holding all KBs, no package machinery. The package form wins on
   distribution (a KB ships in a kit — the LLM-strengths KB should be OOTB)
   and on the proposal/review path, at the cost of package ceremony for what
   is "just files". *Fable/Tim: confirm package-per-KB.*
2. **Where does accept-time provenance verification actually run?** D2 says
   "verified at accept time from the proposal's ledger trail" — the config
   repo's accept path is kernel code; checking a provenance footer against
   ledger facts there adds kernel logic for a package-layer concern. The
   honest cheap version: provenance is *recorded and auditable*, verified by
   the groundskeeper after the fact, not gated at accept. *Lean: record +
   audit, don't gate — matches "safety = audit". Confirm.*
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
5. **DoltLite re-evaluation trigger.** Per the research doc: if a future KB
   requirement introduces *shared-entry concurrent editing with merge*, that
   (and only that) reopens the storage question. Named here so sprint 4
   doesn't relitigate.

## Build order for sprint 4 (acceptance sketches)

1. **B1 — the corpus + manifest marker.** A `kb-llm-strengths` package with
   the seeded role/model files (D5) and a manifest `[kb]` marker; `elanus kb
   list` names it. *Accept: install on a scratch root, list shows it, files
   carry provenance footers; the handoff-workflow SKILL.md pointer line
   exists.*
2. **B2 — search.** The read-only query daemon + `elanus kb search` + the
   `knowledge` skill (D3). *Accept: an agent given only the skill finds the
   "who verifies" answer with file+line from a cold start; daemon is
   `mode=ro`; kill-and-restart safe.*
3. **B3 — pointer blocks.** The `meta` pointer convention + a seeded
   high-awareness block on a dispatching profile (D1/D5). *Accept: `elanus
   context render` shows the block; its `meta` resolves to a real file+line;
   a deliberately broken pointer is detected by B4's checker.*
4. **B4 — groundskeeper, script rung.** Cron package, link/orphan/staleness
   checks, owner report (D4 rung 1). *Accept: seeded breakage classes are
   each reported; zero LLM calls; runs green on a corpus with no breakage.*
5. **B5 — write path.** `elanus kb propose` (or plain config-repo proposal
   flow) + provenance footer stamping (D2). *Accept: an agent's proposed
   edit lands only after acceptance; the ledger reconstructs
   who-proposed-what; a direct write to the package dir by an agent is
   visible in the audit trail.*
6. **B6 — groundskeeper, judgment rung.** Cheap-LLM conflict/missing-link
   proposals, tier chosen by reading B1's own KB (D4 rung 2). *Accept: a
   seeded contradiction (benchmark vs preference) yields a proposal that
   flags, not resolves; cost per pass measured and logged.*

Order rationale: read before write (B2 proves value with zero risk), pointers
before the checker that validates them, script rung before LLM rung
(variety ladder), the write path only once search shows the corpus is worth
maintaining.

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

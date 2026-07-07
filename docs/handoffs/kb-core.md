---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: knowledge base core — the corpus convention, the name, the pointers, the write path

Decomposed from [knowledge-base.md](knowledge-base.md) build steps **B1 + B3 +
B4** (rulings D1, D2, D5, D6). This is the foundation the other three KB
handoffs stand on: the `kb/` corpus convention and its `[kb]` manifest marker,
`elanus kb list`, the seeded `kb-llm-strengths` package, pointer memory blocks,
and the sandbox-gated + git-logged write path. No search here (that is
[kb-search.md](kb-search.md)), no consolidation (that is
[kb-groundskeeper.md](kb-groundskeeper.md)).

The whole design stance (D6): **compose the KB from substrates that already
ship** — the kernel gains at most one manifest field; there is no `kb` table, no
new topic plane. If this handoff finds it needs a kernel table, that is a design
smell to bring back to the design doc.

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **`[kb]` marker required, or inferred from a `kb/` dir?** D1 says "any package
   may carry a `kb/`"; D6 says first-class means "a name" via a manifest marker,
   "the same move `[[harness]]` made." A package might carry a `kb/` dir for its
   own private files without wanting it listed/indexed as knowledge. **My call:
   the `[kb]` marker is the explicit opt-in** — `elanus kb list` and the search
   union (B2) key on the marker's presence, not merely a `kb/` dir on disk. The
   files stay plain and greppable either way. *Fable: confirm marker-required
   (explicit, mirrors `[[harness]]`) over dir-inferred (more magical, but a
   package can't carry a private `kb/`).*

2. **Where the kb git repo lives (design wonky bit 6, left to the implementer by
   remote-backup ergonomics).** D2 settles git-as-provenance but not the repo
   boundary. Options: (a) the **package directory** is the git boundary — a repo
   at `<pkg>/.git`, each kb write is one commit touching only `kb/` paths, and
   remote backup is one remote per package that opts into it; (b) one umbrella
   repo over the packages tree; (c) a repo rooted at each `kb/` subfolder. **My
   call: (a), the package-dir boundary.** It captures the `kb/` alongside the
   `[kb]` manifest that declares it (a coherent unit), and it respects D1's
   copy-vs-link ownership: a *copied* package under `<root>/packages/<pkg>` is
   its own repo (local fork), a *linked* package's shared source dir carries the
   repo (edits visible everywhere it's linked). (b) breaks the linked/copied
   symmetry (linked packages live outside `<root>/packages`); (c) fragments
   provenance away from the manifest. The cost of (a) — many small repos to back
   up — is the ergonomic tradeoff Tim named; *Fable decides if that cost is worth
   the clean ownership mapping.*

3. **Reuse `config_repo::harden()` — extract it to a shared git helper.** kb
   writes commit **agent-authored content**, the same untrusted-content surface
   `config_repo` hardened against (docs/security.md entry 19: hooks off, no
   operator global/system gitconfig — which can carry a `[filter] smudge=…` that
   executes under our flags — no fsmonitor, no signing, `GIT_TERMINAL_PROMPT=0`).
   `harden()` is currently private to `src/config_repo.rs:64`. **My call: extract
   it (and the `git()`/`run_git()` shape) into a small shared helper both modules
   call**, rather than duplicate the discipline. The kb write path does *direct*
   writes then `git add/commit` (no untrusted **clone/checkout** round-trip, so
   the smudge-on-checkout vector is smaller than config_repo's), but hooks-off +
   no-global-config still matter and must not be re-derived from scratch. *Fable:
   confirm the extraction vs a copy.*

4. **Pointer-block staleness (design wonky bit 3).** A pointer block's
   `lines`/`sha` go stale the instant the target file is edited, until the
   groundskeeper's next pass (B5). Design lean, which I follow: **acceptable** —
   the block carries the concept summary inline (that is the payload); the
   pointer is a courtesy for going deeper, and B5's checker catches drift. We do
   **not** couple kb writes to synchronous pointer-block updates.

5. **Concurrent-write correctness dependency.** Seeding the one pointer block
   (M3) is a single write, so it is safe today. But the general "agent writes a
   pointer block" path rides `context_store::upsert_block`
   (`src/context_store.rs:390`), which
   [notes-scaling-and-storage.md](../notes-scaling-and-storage.md) §2 shows is
   **not transactional and produces duplicate rows under concurrency** for
   `global`/`agent`/`session` scope. [storage-hardening.md](storage-hardening.md)
   fixes it. **My call: M3 depends on storage-hardening landing first** for any
   *concurrent* pointer-block writing; the single seeded block does not block on
   it. Flag, don't silently ignore.

## Milestones

### M1 — the `[kb]` marker + `kb/` convention + `elanus kb list`
Add an optional `[kb]` table to the manifest, mirroring how `[[harness]]` sits in
`Manifest` (`src/manifest.rs:19-57`): a `#[serde(default)] pub kb: Option<KbDecl>`
where `KbDecl` carries optional `title` and `description` (both `#[serde(default)]`;
`#[serde(deny_unknown_fields)]`). Presence declares "this package's `kb/`
subfolder is a knowledge base." Add `elanus kb list` as a new `Cmd::Kb { cmd:
KbCmd }` (the `Cmd` enum + dispatch in `src/main.rs`, mirroring `Cmd::Config`/
`Cmd::Block`), served by a new `src/kbcli.rs` module (mirror `src/configcli.rs`/
`src/blockcli.rs`). `list` iterates `packages::discover_for_profile`
(`src/packages.rs:71`) filtering `manifest.kb.is_some()`, printing name, title,
`kb/` file count, and the resolved `kb/` path. `--json` for machine use.

**Acceptance:** on a scratch root, install a package carrying `[kb]` + a `kb/`
file; `elanus kb list` names it with its title and file count. A package with a
`kb/` dir but no `[kb]` marker is **not** listed. `cargo test` green (a manifest
parse test in the style of `harness_declarations_parse_with_defaults`,
`src/manifest.rs:738`).

### M2 — the seeded `kb-llm-strengths` package (first tenant, D5)
Ship `kits/stdlib/packages/kb-llm-strengths/` with `elanus.toml` (`[kb]` marker,
title "LLM strengths"), a `SKILL.md`, and a `kb/` seeded from the model-tiering
knowledge that today lives **only** in Claude's config
(`.claude/skills/handoff-workflow/SKILL.md:81-96`) plus Tim's second-level model
notes: **one file per model** (`kb/claude.md`, `kb/opus.md`, `kb/gpt-5.5.md`,
`kb/glm-5.2.md`, `kb/fable.md`) and **one file per role**
(`kb/role-planner.md`, `kb/role-implementer.md`, `kb/role-verifier.md`),
cross-linked by relative path + line anchors (journey 14). Encode the invariants
verbatim: plan = Claude/Fable only, never GPT/GLM; implement = Opus/GPT-5.5/GLM
medium; verify = Opus/GPT-5.5 high, Fable for the hardest; **planning never
flexes**. Then **reconciliation** (D5): add a pointer line to
`.claude/skills/handoff-workflow/SKILL.md` at the model section — "canonical
copy: the `kb-llm-strengths` KB (`kb/role-*.md`, `kb/*.md`); update both" — so
the two copies do not fork (the SKILL.md keeps its copy because it configures
Claude Code, which cannot read the KB).

**Acceptance:** the package installs on a scratch root and shows in `elanus kb
list`; the role/model files exist and cross-link; `grep -ri "who verifies" kb/`
finds the verifier facts with a file+line; the handoff-workflow SKILL.md carries
the canonical-copy pointer line.

### M3 — pointer blocks (D1/D5)
Establish the pointer convention: a vanilla `ContextBlock`
(`src/context_blocks.rs:31-43`) whose `content` is the concept summary and whose
`meta` (`:42`, free JSON, currently unused) carries `{ "kb": "<pkg>", "path":
"kb/role-verifier.md", "lines": "12-28", "sha": "<content-sha256>" }`. Teach
`blockcli::set` (`src/blockcli.rs:85`) to accept `--meta <json>` (today it does
not), and seed **one high-awareness pointer block** on a dispatching profile
(`kits/core/profiles/architect` — the profile that launches agents) via
`context_store::seed_defaults` (`src/context_store.rs:510`), its `meta` pointing
into `kb/role-*.md`. (See wonky bit 5 on the concurrency dependency for the
general write path.)

**Acceptance:** `elanus context render` for the architect profile shows the
block; its `meta` resolves to a real file + line range + a matching sha; a
deliberately broken pointer (wrong `sha`) is left detectable by B5's checker (not
built here — assert only that the fields are present and machine-readable).
`cargo test` green.

### M4 — the write path (D2): sandbox-gated direct write + git commit per write
A kb write is a plain file write into a package's `kb/` tree, gated by the
agent's existing sandbox grant (`fs_write` on that tree — the cage,
`src/sandbox.rs`, nothing custom), **followed by one git commit** using the
extracted hardened-git helper (wonky bit 3). Provide the taught pattern in a
`knowledge` skill (ships in stdlib per D6 — the default agent "just knows" how to
write knowledge) and an optional `elanus kb write <pkg> <path>` convenience verb
that does write-then-commit atomically with the same hardening. The commit
author is the fixed kernel identity (as in `config_repo`); who-wrote-what-when is
the commit log plus the ordinary obs trail (no provenance footers — D2).

**Acceptance:** an agent holding the `fs_write` grant on a `kb/` tree writes a
file and the commit records it (git log shows the change with the kernel
committer); an agent **without** the grant is refused by the cage; the git log
reconstructs who-wrote-what-when; a configured remote is push-able (remote-backup
property, D2). `cargo test` green.

## Read these first
- The settled design: [knowledge-base.md](knowledge-base.md) — D1 (corpus =
  `kb/` subfolder; copy-vs-link = local-vs-global), D2 (sandboxes gate writes,
  provenance = git commits), D5 (LLM-strengths first tenant), D6 (first-class
  without a kernel data model), build steps B1/B3/B4, wonky bits 3 and 6.
- The why: [../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md)
  ("tell the agent" — memory blocks are high-availability pointers; KB is the
  missing tier).
- The manifest substrate: `src/manifest.rs:19-57` (where `[kb]` sits, mirror the
  `harness: Vec<HarnessDecl>` field `:34` + `HarnessDecl` `:268`), the parse +
  hash discipline `:310-461`, the parse tests `:738`.
- The block substrate: `src/context_blocks.rs:31-43` (`meta` at `:42`),
  `src/context_store.rs:390` (`upsert_block` — and its non-transactional
  dup-row hazard, [notes-scaling-and-storage.md](../notes-scaling-and-storage.md)
  §2), `:510` (`seed_defaults`), `src/blockcli.rs:85-189`;
  [memory-blocks.md](memory-blocks.md).
- The git discipline to reuse: `src/config_repo.rs:57-129` (`harden()`, `git()`,
  `run_git()`, `commit_path()` `:331`) — the untrusted-content hardening
  (docs/security.md entries 18, 19).
- The concurrency dependency: [storage-hardening.md](storage-hardening.md).
- The CLI-module pattern to mirror: `src/configcli.rs`, `src/blockcli.rs`, and
  their `Cmd`/dispatch wiring in `src/main.rs`.
- Patterns to copy: `packages/notes/SKILL.md` (files-as-knowledge, pure skill
  text — the `knowledge` skill's shape); the seed content
  `.claude/skills/handoff-workflow/SKILL.md:81-96`.

## Log
- 2026-07-07 — Confirmed shipped+merged on main (sprint-4 KB arc, merged
  `80a23c7`); status flipped to `done` (was stale at `planned`). 559 tests green.
- 2026-07-02 — Decomposed from knowledge-base.md B1/B3/B4 by Opus (planner) under
  Fable. Grounded against the sprint-4 worktree: `[[harness]]` at
  `src/manifest.rs:34` is the marker precedent; `ContextBlock.meta`
  (`src/context_blocks.rs:42`) is unused free JSON, the pointer home;
  `blockcli::set` does **not** yet accept `--meta` (M3 adds it);
  `config_repo::harden()` (`:64`) is private and must be extracted for reuse (M4);
  `storage-hardening.md` exists and is the concurrency dependency for concurrent
  pointer-block writes; `kits/core/profiles/architect` is the dispatching profile
  for the seeded pointer block. No FTS anywhere in the tree (that is kb-search).
  Judgment calls flagged: marker-required (1), package-dir git boundary (2),
  extract harden() (3), staleness acceptable + groundskeeper (4), storage
  dependency (5).

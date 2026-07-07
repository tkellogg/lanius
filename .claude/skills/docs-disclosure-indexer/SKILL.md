---
name: docs-disclosure-indexer
description: Maintain lanius docs/ progressive-disclosure indexes and useful docs-to-code/code-to-doc references. Use when adding, reorganizing, or auditing docs/ README indexes, AGENTS.md docs guidance, or references between design docs and implementation files.
---

# Docs Disclosure Indexer

## Workflow

1. Read the relevant docs before writing indexes. For a full refresh, read every
   `docs/**/*.md` file, including existing `README.md` files.
2. Treat each folder README as a routing table. It should answer: what is in
   this folder, which file should an agent read first for each kind of task, and
   which files can be skipped unless the task matches.
3. Keep summaries short and concrete. Do not duplicate long arguments, decisions,
   or examples that already live in child docs.
4. Link child README files from parent README files. Link parent context from
   child README files when it helps orientation.
5. Add docs-to-code and code-to-doc references only where they make maintenance
   easier:
   - docs should name implementation files for concepts that have clear code
     ownership;
   - code comments should point at docs for architectural invariants,
     policy/security assumptions, topic/config contracts, or non-obvious UI
     flows;
   - avoid decorative references in obvious glue, generated files, or places
     already covered by nearby comments.
6. Update root `AGENTS.md` so future agents know the indexes exist and which
   skill to use for this work.
7. Validate with `rg` and a docs/link-oriented review. Run tests only if code
   behavior changed.

## README Shape

Use this shape unless the folder demands something simpler:

```markdown
# <folder name>

Purpose sentence.

## Start here
- <task/situation>: [file](file.md) ...

## Contents
- [file](file.md) — one-line scope and when to read it.
```

For dense folders, add a short "Skip unless" clause to entries whose files are
specialized. For ordered journey folders, preserve the intended reading order.

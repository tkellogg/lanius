---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-08
---

# Handoff: KB formalization — a parseable file format (frontmatter + relative links) a script can read without an LLM

A knowledge-base entry is markdown. Today that markdown is free-form: some files
carry cross-links, none carry frontmatter, and the one link that reaches outside
its own tree does so with a fragile `../../../../docs/...` escape. This handoff
makes a KB entry **deterministically parseable** — a small, defined frontmatter
block plus a single, strict link convention — so a plain script (regex or a
markdown parser) can pull out an entry's metadata and its links with **no LLM in
the loop**. Then it adds the deterministic parser, teaches the groundskeeper to
validate the format (including dead-link detection, which does not exist today),
writes the convention down as an entry in our own lanius KB, and retrofits the
shipped KB files to conform.

This composes on the KB substrate that already shipped
([kb-core.md](kb-core.md), [kb-groundskeeper.md](kb-groundskeeper.md),
[kb-search.md](kb-search.md), [kb-discovery.md](kb-discovery.md)) — it adds **no
kernel table and no new dependency**. It also closes a standing `_questions.md`
item: "KB should have a README that instructs what sorts of information go into
it… the README acts like the `description` field of a skill."

## The format, in one screen (the thing to confirm)

**A KB entry is a markdown file with optional YAML frontmatter and inline links.**

Frontmatter — a `---`-delimited block at the very top, single-line scalars only:

```
---
title: Role: planner (phases 1–2)
description: Who plans, and the rule that never flexes.
tags: [roles, planning]
---
# Role: planner (phases 1–2)
...body...
```

- **`title`** (required) — the entry's human title. The body's first `# heading`
  stays for readers; `title` is the machine field so a script never has to guess.
- **`description`** (required) — one line, the "what is in here / when to read
  this" — the per-entry analog of a skill's `description` and of the `[kb]`
  marker's package-level `description`.
- **`tags`** (optional) — an inline list `[a, b, c]`, for grep/filtering. Absent
  = no tags.
- Unknown keys are **ignored** (forward-compatible), not an error. No dates
  (staleness is already tracked by the pointer-block sha, kb-core M3 — a
  frontmatter date would just be one more thing to go stale).

**Links** — every reference to another internal file is ALWAYS a relative-path
inline markdown link:

```
see [role-verifier.md](role-verifier.md)      ✅ relative, inline
see [the design](../kb-core/overview.md)       ✅ relative across dirs, inline
```

and never:

```
[x](/abs/path.md)          ❌ absolute path
[x](https://…) for an internal file, or a bare URL   ❌ (real external web URLs are fine)
[x][ref] … [ref]: path     ❌ reference-style link
```

A link's target is resolved **relative to the directory containing the KB file**
(standard markdown/POSIX). A `#fragment` or query is stripped before resolution;
a `scheme:` target (`http:`, `https:`, `mailto:`) is a real external link and is
not resolved on disk; a `#`-only target is an in-page anchor. Everything else is
an **internal reference**, and the resolvable-link contract is: **it must resolve
to a file that exists inside the package's own tree** — the target travels with
the package.

**Referencing something outside the portable unit** (a repo design doc, a file in
another package, anything that would not ship inside this package) is **prose or a
real URL, never a resolvable relative link**. A package installs into a user's
root *without* the repo's `docs/`, so a relative link that escapes the package
(e.g. `../../../../docs/channels.md`) is broken-after-install — not "fragile,"
broken. Write "the channels design doc in the lanius repo" or link a real
`https://…` URL instead.

That is the whole contract. A ten-line script — or `regex \]\(([^)]+)\)` — can
now list an entry's outbound links and read its metadata, exactly as Tim asked.

## Decisions to confirm / wonky bits (my calls flagged)

1. **Frontmatter is YAML `---`, hand-parsed — not `serde_yaml`.** The repo has
   **no** YAML dependency (checked `Cargo.toml`); SKILL.md frontmatter is read by
   `manifest::skill_md` (`src/manifest.rs:767`), a deliberately minimal
   `---`-delimited single-line-scalar reader ("Deliberately not a full YAML
   parser — the required fields are single-line scalars"). **My call: mirror
   `skill_md` exactly** for the KB reader — `title`/`description` as
   `key: value` lines, `tags` as one `[a, b]` line split on commas. No new
   dependency, same greppable discipline, and it matches the `---` docs Tim
   named. *Confirm hand-rolled over pulling in serde_yaml.*

2. **The parser lives in Rust core (`src/kb.rs`), exposed to scripts via a CLI
   verb.** The two script consumers (kb-search `scripts/index`, kb-discovery's
   scan) already shell to `lanius kb list --json` rather than re-derive the KB
   set, and the codebase already treats "the CLI is the API" as the seam
   (`query_tokens` is the one place it mirrors logic into a script, and even that
   is a source of drift risk). **My call: one canonical `parse_kb_entry` in
   `src/kb.rs`; the groundskeeper (Rust) calls it directly; scripts get it
   deterministically through a new `lanius kb parse` verb (`--json`)** rather than
   re-implementing a regex per language. One source of truth for the format.
   *Confirm core+verb over duplicating the regex into each script.*

3. **Malformed format is a WARNING, never a hard error.** Consistent with how the
   rest of the system treats config/manifest validity (a bad package is
   visible-but-inert and loud, not fatal — `src/packages.rs:66`). A missing
   frontmatter block, a missing required field, an absolute/reference-style link,
   or a dead relative link is a **finding in the groundskeeper sweep** (`lanius kb
   check`), surfaced in the owner report — it does not block a write, a search, or
   an index build. This is what keeps the change **backward-compatible**: the 15
   shipped files that have no frontmatter today keep working the instant this
   lands; they merely show up as findings until M4 retrofits them. *Confirm
   warn-not-block.*

4. **One rule: an internal relative link must resolve INSIDE the package's own
   tree.** (Tech-lead ruling — settled, not open.) A package is a portable unit:
   it installs into a user's root *without* the repo's `docs/`, so a relative link
   that escapes the package (e.g. `kb-lanius/kb/channels.md:7` →
   `[docs/channels.md](../../../../docs/channels.md)`) is **broken-after-install,
   not merely fragile**. Dead-on-disk and escapes-the-package are the **same
   defect** — the target does not travel with the package — so they are **one
   finding class**, `dead_link` (a.k.a. `unresolved_link`): it fires when a
   relative internal target either does not resolve on disk *or* resolves to a path
   outside the package tree. WARN-level, not block (posture unchanged). To
   reference something outside the portable unit (a repo design doc, a file in
   another package), use **prose or a real URL — not a resolvable relative link**
   (see the format section). There is no separate "escaping" advisory tier.

5. **No mandatory per-package `kb/README.md`.** The `_questions.md` "KB needs a
   README" item is satisfied by (a) the **package-level** `[kb] description` in
   `lanius.toml` (already shipped, already what `discover`/`kb list` key on) and
   (b) the **per-entry** frontmatter `description`. Adding a third README file per
   KB is redundant. The *global* "how to write a KB entry / what belongs in a KB"
   guidance is the one primary artifact this handoff ships (M3), as an entry in
   the lanius KB — which is exactly where Tim pointed. A package MAY still drop a
   `kb/README.md` as a hand-written index; it's an ordinary entry, nothing
   special. *Confirm we don't mandate a per-KB README file.*

6. **`title` duplicates the body's `# heading` — accept it.** The heading is for
   humans reading the raw file; `title` is for scripts. Recommend they match but
   do **not** enforce equality (that's a needless failure mode). The format check
   does not compare them.

## Milestones

### M1 — the deterministic parser in `src/kb.rs` (no consumer change yet)
Add `parse_kb_entry(text: &str) -> ParsedEntry` to `src/kb.rs`, mirroring
`manifest::skill_md`'s hand-rolled `---` reader. `ParsedEntry` carries:
`frontmatter: Option<Frontmatter { title: Option<String>, description:
Option<String>, tags: Vec<String>, unknown_keys: Vec<String> }>`, `links:
Vec<Link { text: String, target: String, line: usize }>`, and a
`body_start_line` (so a consumer can index the body without the frontmatter).
Link extraction is a single pass over the body recognizing inline
`[text](target)` and, so it can *flag* them, the disallowed reference-style
`[text][id]` / shortcut forms. Classify each target: `External` (has a
`scheme:`), `Anchor` (`#…`), `Absolute` (`/…`), `Relative(path, fragment)`.
Provide `classify_link(target)` and a `resolve_relative(kb_file_dir, package_root,
target) -> LinkResolution { path, resolves: bool }` helper that both M2 consumers
use. **`resolves` is true only when the target both exists on disk AND lies inside
`package_root`** (the portable unit) — a target that escapes the package tree
resolves to `false`, the same as a missing file (wonky bit 4: one contract, the
target must travel with the package). Pure functions over inputs (like
`classify_pointer`, `src/groundskeeper.rs:171`), fully unit-testable.

**Acceptance:** unit tests in `src/kb.rs` cover: frontmatter present/absent,
missing required field, unknown key ignored, `tags` list parsed; each link class
(`role-verifier.md` → Relative+resolves, `../x/y.md` → Relative, `/abs` →
Absolute, `https://…` → External, `[a][b]` → flagged reference-style);
`resolve_relative` returns `resolves: true` for an in-package target that exists
and `resolves: false` for both a missing file AND a resolves-but-escapes-the-package
target (e.g. `../../../../docs/x.md`), against a scratch package tree. `cargo test`
green. No behavior visible to any consumer yet.

### M2 — expose it (`lanius kb parse`) and validate it (groundskeeper sweep)
(a) Add `lanius kb parse <pkg> <path> [--json]` (a `KbCmd` variant + `kbcli.rs`
function, mirroring `kb list`/`kb search`) that prints the parsed frontmatter +
classified links — the deterministic, no-LLM extraction surface scripts call.
(b) Extend the groundskeeper sweep (`src/groundskeeper.rs::sweep`, surfaced by
`lanius kb check`) with new finding classes over every file in every enabled
`[kb]` package: `missing_frontmatter` / `missing_field` (warn), `bad_link`
(absolute, reference-style, or bare-URL internal ref — warn), and `dead_link` (a
relative internal ref that does not resolve inside the package tree — either
missing on disk OR escaping the package, one class per wonky bit 4 — warn). These
join the existing `broken`/`stale`/`orphans` report shape and flow through
`report_summary` + the `--mail` owner report. Zero LLM calls (this is rung 1's
discipline). Do **not** change the write path, search, or index behavior.

**Acceptance:** on a seeded corpus containing one frontmatter-less file, one file
with an absolute link, one with a relative link that escapes the package tree
(the `../../../../docs/…` shape — a `dead_link`), and one with a legitimate
in-package resolving cross-file link, `lanius kb check --json` reports exactly the
first three as findings and leaves the fourth clean; `lanius kb parse` emits the
frontmatter and link list as JSON; `grep`/instrumentation confirms **zero** LLM
calls; `cargo test` green for the pure finding logic. (Optional, if cheap: the
kb-search indexer strips a recognized frontmatter block from the chunk body via
`kb parse` so YAML keys aren't indexed as prose — otherwise leave indexing
untouched and note it as a residual.)

### M3 — the primary artifact: write the convention into the lanius KB
Add one entry to the lanius KB — `kits/helper/packages/kb-lanius/kb/writing-kb-entries.md`
— that is the canonical, human-facing spec: what a KB entry *is* (markdown +
frontmatter + relative inline links), the exact frontmatter fields, the link rule
with examples of right and wrong, "relative to the file's own directory," and —
answering the `_questions.md` README item — **what belongs in a KB vs a memory
block vs `docs/`** (durable, greppable, one-topic-per-file knowledge; not
fast-changing state, not per-agent memory). This entry itself conforms to the
format (it has frontmatter and any links are relative-inline) — it is its own
worked example. Cross-link it from `kb-lanius/kb/overview.md`'s knowledge-base
paragraph and from the stdlib `knowledge` skill (the taught write pattern) so an
agent about to "write something down" is pointed at the format and at KB
introspection to find the right home.

**Acceptance:** `writing-kb-entries.md` exists, carries conforming frontmatter,
states both the frontmatter fields and the relative-inline link rule, and
explains what goes in a KB; `lanius kb parse` on it returns the expected
frontmatter; `overview.md` and the `knowledge` skill link to it (relative-inline);
`lanius kb check` reports **zero** findings for this new file. A grep for the
format rule (`grep -ri "relative" kb/`) finds it.

### M4 — retrofit the shipped KB files to conform
Bring the 15 shipped KB files into the format so the sweep is green on ship:
- **kb-llm-strengths** (8 files: `claude.md`, `fable.md`, `glm-5.2.md`,
  `gpt-5.5.md`, `opus.md`, `role-planner.md`, `role-implementer.md`,
  `role-verifier.md`) — links already conform (same-dir relative inline); **add
  frontmatter only** (`title` from the `# heading`, a one-line `description`,
  `tags`).
- **kb-lanius** (7 files: `overview.md`, `channels.md`, `kits-and-packages.md`,
  `llm-access.md`, `model-guidance.md`, `mutation-doctrine.md`,
  `setup-checklist.md`) — **add frontmatter**, and **de-link `channels.md:7`'s
  escaping reference** `[docs/channels.md](../../../../docs/channels.md)`: it
  points outside the portable package, so make it a **plain-text mention** ("the
  channels design doc in the lanius repo") — or a real URL — rather than a
  relative link (wonky bit 4; the format rule).

**Acceptance:** every shipped KB file parses with valid frontmatter; `lanius kb
check` over the default profile's enabled KBs reports **zero** `missing_frontmatter`
/ `missing_field` / `bad_link` / `dead_link` findings (no relative link escapes a
package tree); a new `cargo test` (style of
`seeded_kb_llm_strengths_installs_and_lists`, `src/kb.rs:579`) asserts the shipped
files conform. The e2e that exercises the KB stays green.

## Read these first
- The parser precedent to mirror: `src/manifest.rs:767-798` (`skill_md` — the
  minimal `---` single-line-scalar reader; the deliberate "not a full YAML
  parser" stance) and the fact that `Cargo.toml` carries **no YAML dep**.
- The KB core + write path: [kb-core.md](kb-core.md), `src/kb.rs` (where
  `parse_kb_entry` lands; `kb_dir`, `enumerate`, the symlink discipline; the
  existing tests at `:458`).
- The sweep to extend: `src/groundskeeper.rs` (`sweep` `:211`, the pure
  `classify_pointer` `:171`, the `Report`/finding shapes `:63-101`, `list_kb_files`
  `:292`) and its CLI `src/kbcli.rs::check` (`:104`), `report_summary`, the
  `--mail` owner report.
- The CLI-verb pattern to mirror for `kb parse`: `src/kbcli.rs` (`list`/`search`)
  and the `Cmd::Kb { cmd: KbCmd }` dispatch in `src/main.rs`.
- The script consumers that benefit from the deterministic verb: kb-search
  `kits/stdlib/packages/kb-search/scripts/index` (heading-chunker; would strip
  frontmatter) and the discovery scan `src/discover.rs` (`kb_files`/`match_package`,
  the bounded content read `:190`).
- The files to retrofit and their current state:
  `kits/stdlib/packages/kb-llm-strengths/kb/*.md` (already relative-inline
  cross-linked — e.g. `role-planner.md`) and
  `kits/helper/packages/kb-lanius/kb/*.md` (no frontmatter; `channels.md:7` is the
  one package-escaping link — a `dead_link` to de-link in M4).
- The `_questions.md` item this closes: the "KB should have a README… acts like
  the `description` field of a skill… guide KB introspection" entry.

## Residuals / gating
- **Not gated on anything** — the parser, the sweep extension, and the retrofit
  are all local to `src/kb.rs` / `src/groundskeeper.rs` / the shipped packages.
- **Concurrency:** none introduced. The sweep is read-only over disk; the retrofit
  writes shipped files at author time, not through the concurrent-write path.
- **The kb-search frontmatter strip (M2 optional)** is a nicety, not a
  correctness requirement — if it complicates the pass, ship the parser + verb and
  leave the indexer untouched (frontmatter as a few extra indexed lines is
  harmless). Note it as a follow-up.
- **Link contract is settled** (wonky bit 4, tech-lead ruling): one `dead_link`
  class — "an internal relative link must resolve inside the package's own tree" —
  covering both missing-on-disk and escapes-the-package; no separate advisory
  tier. No open design choice remains here.
- **Enforcement stays advisory** — this handoff does not add a pre-write format
  gate. If a hard gate is wanted later (refuse a `kb write` of a malformed entry),
  that's a separate, deliberate step; keep it out of scope here.

## Log
- 2026-07-08 — Planned by Opus (planner) under Fable, grounded against the
  elanus-channels worktree. Confirmed: **no in-file link parsing exists anywhere**
  (the groundskeeper's orphans/pointers read `context_blocks.meta` in SQL, not
  file links — dead-link detection is new); **kb-llm-strengths already uses
  relative-inline links**, kb-lanius has none except one escaping link
  (`channels.md:7` → `../../../../docs/channels.md`); **no KB file has
  frontmatter**; `manifest::skill_md` (`src/manifest.rs:767`) is the exact
  hand-rolled `---`-reader precedent and there is **no YAML dependency** in
  `Cargo.toml`. Judgment calls flagged: hand-rolled YAML (1), parser in core +
  `kb parse` verb (2), warn-not-block (3), link-resolution scope (4), no
  mandatory per-KB README (5), title/heading duplication accepted (6).
- 2026-07-08 — Tech-lead ruling on wonky bit 4: **collapse** the two-tier
  dead/escaping distinction into ONE `dead_link` rule — "an internal relative link
  must resolve inside the package's own tree" (a package installs without the
  repo's `docs/`, so an escaping link is broken-after-install, the same defect as
  missing-on-disk). References outside the portable unit are prose or a real URL,
  not resolvable relative links. Updated the format section, wonky bit 4 (now
  settled), M1 (`resolve_relative` returns `resolves: false` on escape), M2 (one
  finding class), M4 (channels.md → plain-text mention), and residuals.

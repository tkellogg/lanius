---
title: Writing a KB entry
description: What a KB entry is, its frontmatter + relative-link format, and what belongs in a KB.
tags: [lanius, kb, conventions]
---
# Writing a KB entry

A knowledge-base entry is a plain markdown file inside a package's `kb/` tree.
This is the canonical spec for the format — the thing that lets a script (or a
plain `grep`) read an entry's metadata and links with **no LLM in the loop**. It
is its own worked example: it carries the frontmatter it describes, and every link
in it is a relative-inline link that resolves inside this package.

## Frontmatter

Every entry opens with a `---`-delimited block of single-line scalars, at the very
top of the file:

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
  stays for readers; `title` is the machine field, so a script never has to guess.
  They should match, but nothing enforces equality.
- **`description`** (required) — one line: "what is in here / when to read this."
  It is the per-entry analog of a skill's `description` and of the package-level
  `[kb] description` in `lanius.toml`.
- **`tags`** (optional) — an inline list `[a, b, c]` for grep/filtering. Absent
  means no tags.
- **Unknown keys are ignored** (forward-compatible), never an error. There is no
  date field — staleness is tracked by a memory block's pointer sha, not here.

## Links

Every reference to another internal file is a **relative-path inline markdown
link**, resolved relative to the directory containing the KB file:

```
see [overview.md](overview.md)               ✅ relative, inline, same dir
see [the design](sub/detail.md)              ✅ relative across dirs, inline
see https://example.com/spec                 ✅ a real external URL is fine
```

Never:

```
[x](/abs/path.md)                            ❌ absolute path
[x][ref] … [ref]: path                       ❌ reference-style link
[x](https://…) stand-in for an internal file ❌ (use a relative link on disk)
```

The one rule: **an internal relative link must resolve to a file that exists
inside the package's own tree.** A package installs into a user's root *without*
the repo's `docs/`, so a link that escapes the package (e.g.
`../../../../docs/channels.md`) is broken-after-install — not merely fragile,
broken. To reference something outside the portable unit (a repo design doc, a
file in another package), write it as **prose or a real URL**, never a resolvable
relative link. A `#fragment` is stripped before resolution; a `scheme:` target
(`http:`, `https:`, `mailto:`) is a real external link and is not resolved on disk.

The groundskeeper sweep (`lanius kb check`) reports any entry that breaks this —
`missing_frontmatter`, `missing_field`, `bad_link`, `dead_link` — as a WARN-level
finding. It never blocks a write. Introspect a single entry deterministically with
`lanius kb parse <package> <path>` (add `--json` for a script).

## What belongs in a KB

A KB holds **durable, greppable, one-topic-per-file knowledge that is "ours"** — a
shared fact other agents should see: model tiering, an API's quirks, a project
convention, a design's mental model. It is not:

- **fast-changing state** — that goes in an event, a topic, or a status block, not
  a file that would immediately go stale;
- **per-agent memory** — your own scratch stays in `notes/` or a memory block; a
  memory block may then carry a *pointer* into a KB file for high availability;
- **repo-internal design docs** — long-form design that ships with the source tree
  (the repo's `docs/`) is referenced by prose or URL, not copied into a KB.

The ladder is **notes (mine) → kb/ (ours) → memory block (a pointer into the kb)**.
For the wider picture of where knowledge lives, see [overview.md](overview.md).

---
name: knowledge
description: Read, search, and write knowledge bases (kb/ folders). Use when you learn something durable that is "ours" (not just your own scratch notes) — model tiering, an API's quirks, a project convention — or when you need a fact someone already wrote down.
---

# knowledge — using knowledge bases

A knowledge base is a `kb/` subfolder inside a package, declared by a `[kb]`
marker in the package's `lanius.toml`. It is plain, greppable markdown: one topic
per file, with file + line anchors when you cross-reference. Knowledge that is
*ours* (a shared fact other agents should see) belongs here; your own private
scratch stays in `notes/` or a memory block.

The ladder: **notes (mine)** → **kb/ (ours, sandbox-gated + git-logged)** →
**memory block (a high-availability pointer into the kb)**.

## Find and read

- `lanius kb list` — the enabled knowledge bases (name, title, file count, path).
- Read a file directly, or grep the tree: `grep -rin "who verifies" <kb path>`.
- A memory block may already point you at the exact file + lines (its `meta`
  carries `{kb, path, lines, sha}`) — follow the pointer for the deep copy.

## Write

You write a KB the same way you write any file, then it is committed for
provenance. Two ways:

- **Convenience verb (does write-then-commit atomically):**
  `lanius kb write <package> <path-inside-kb> --content "..."`, or pipe the
  content on stdin: `printf '%s' "$body" | lanius kb write kb-llm-strengths kb/gpt-5.5.md`.
- **By hand:** write the file under the package's `kb/` tree with your normal
  tools; the change is captured by the KB's git repo.

Rules of the road:

- **You need the write grant.** Writing a `kb/` tree is an ordinary sandbox
  `fs_write` on that package's directory. If you lack it, the cage refuses the
  write — ask the human to grant it (or work in a *copied* package, whose `kb/`
  is inside your own writable world).
- **One topic per file; cross-link by relative path + line.** Keep files small
  and greppable.
- **Provenance is the git log**, not a footer you add — do not stamp
  "written by …" lines; the commit records who-what-when.
- **Do not fork a paired copy.** If a file says "canonical copy: …; update
  both," update both.

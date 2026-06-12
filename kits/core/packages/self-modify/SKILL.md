---
name: self-modify
description: The edit→re-review loop for changing the harness — how to build or modify packages so your changes actually land, and why every edit goes back through human review.
---

# self-modify

You can extend this harness: packages are directories you can write (a
manifest of *requests*, scripts, a SKILL.md), and the dispatcher picks
them up by discovery. But understand the loop before you start, or your
work will sit inert and you'll wonder why.

## The loop, honestly

1. **Write or edit** a package under a directory on the package path
   (usually `packages/<name>/` in the harness root — check your cage
   covers it; if not, that's a grant to request, not a workaround to find).
2. **Every capability is a request.** `elanus.toml` declares what the
   package needs — subscribe/publish filters, fs prefixes, hook points,
   context stages, MCP servers. Declaring grants you NOTHING.
3. **Editing de-approves.** Grants pin to the manifest *and the code
   bytes*. The moment you change a script, every approval for that
   package silently reverts to pending — your edited package stops
   firing. This is not a bug; a grant authorizes code, not a name.
4. **The human commits.** Tell your owner what you changed and why, then
   point them at the review: `elanus packages` shows what's pending,
   `elanus approve <name>` lands it. Mail them
   (`emit_event` → `in/human/<owner>`) with a one-paragraph summary and
   the approve command. Do not nag; one clear message.

## What makes a change land on first review

- Smallest possible request set: ask for the filters and prefixes you
  need, not `#`. Over-asking reads as either sloppiness or exfiltration.
- A SKILL.md if other agents should use it (name + description
  frontmatter; the body is progressive disclosure — they read it on
  demand).
- Say what you tested. `elanus exec` against your own session, or emit a
  test event and show the trace line.

## Know your limits

Harness *kernel* work — the Rust under `src/`, broker behavior, the
dispatch machinery — is architect-grade: read the `escalate` skill and
hand it up rather than guessing. Package-land (scripts, manifests,
skills, stages) is yours.

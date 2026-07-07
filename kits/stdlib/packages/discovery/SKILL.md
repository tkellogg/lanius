---
name: discovery
description: When you lack a capability the task needs — a knowledge base, tool, skill, stage, MCP server, or harness — call the find_capability tool. It searches packages you DON'T have enabled and tells you which one carries what you need, what enabling it would add, and how to request it. Discovery's own existence is high-availability; the mechanics here are expando.
---

# discovery — find a capability you don't have enabled

Your tool array and skills show what you *have*. But the instance may carry more
than your profile enables — a `discord` package with API notes and a posting
tool, a `paging` package that escalates to an on-call pager, a KB you were never
given. You cannot discover those by ordinary means: a capability you lack is, by
definition, not in front of you. That is what `find_capability` is for.

## When to reach for it

The moment the task in front of you needs something you don't have — an API you
don't know, a system you can't reach, knowledge you're guessing at — **before you
give up or guess**, ask: *does a package I don't have enabled carry this?* Call
`find_capability` with a plain-words query.

```json
{ "query": "discord api" }
→ { "query": "discord api",
    "found": [
      { "package": "discord",
        "matched": ["kb/discord-api-notes.md", "skill discord", "package name"],
        "adds": { "kb": ["discord-api-notes.md"],
                  "skills": ["discord"],
                  "tools": ["send_discord"] },
        "enable": "package \"discord\" is available in this instance but not on the
                   \"...\" profile's path — request enablement through the existing
                   config-proposal flow, or ask the owner to enable it" } ] }
```

Each hit names the package, **what matched**, **what enabling it would add**
(knowledge files, skills, tools, stages, MCP servers, harnesses), and the
**enable path**. An empty `found` means nothing you lack matches — everything
matching your query is already on your path.

## Getting the capability

Discovery only *tells you it exists* — it grants nothing. To actually use the
capability, request enablement: it rides the ordinary **config-proposal flow**
(propose the change to your profile; a human or your autonomy level accepts it),
or ask the owner to enable the package. There is no special discovery enable
path — the same proposal machinery every config change uses.

## Availability tiers (journey 14)

The *existence* of `find_capability` is high-availability: know it is there and
reach for it whenever you hit the edge of what you have. The *mechanics* — that
it reads the instance's package universe rather than your visible set, that it is
a privileged read the owner approved into being — are expando: read them here
only when you need them.

## `lanius discover <query>` — the CLI behind it

The tool wraps `lanius discover --json <query>`, the kernel command that owns the
universe scan. A human or a harness at a shell can run `lanius discover "discord
api"` directly (`--json` for the machine shape, `--profile <name>` to ask on
another agent's behalf).

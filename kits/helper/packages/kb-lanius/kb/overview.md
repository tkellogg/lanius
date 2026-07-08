---
title: Lanius Overview
description: What lanius is — the orchestration layer, topic planes, agents, packages, KBs.
tags: [lanius, overview]
---
# Lanius Overview

Lanius is an orchestration layer around agents, packages, tools, and humans. The
kernel records events, routes messages, gates authority through package grants,
and gives agents a consistent way to inspect and mutate the system.

The bus is organized into topic planes. `in/agent/<name>` is an agent mailbox,
`in/human/<owner>` is a human mailbox, `in/package/<name>/...` addresses package
actors, `obs/...` records observations and results, and `signal/...` carries
attention or interrupt-style events. Messages are durable enough to reconstruct
what happened later.

Agents are profiles with an agent noun, owner, model configuration, visible
packages, prompt blocks, and sandbox policy. A profile can run foreground with
`lanius agent run` or be queued with `lanius agent spawn` when an approved exec
handler subscribes to its mailbox.

Packages supply capabilities: skills, context stages, tools, daemons, cron jobs,
or exec handlers. Discovery is visibility, not authority. A package can be on an
agent's path while its requested capability remains pending until approved.

Knowledge bases are plain `kb/` folders inside packages that declare a `[kb]`
marker. Use `lanius kb list` to find them, `lanius kb search` or the
`search_knowledge` tool to query them, and `lanius kb write` to add durable
knowledge where the package and grants allow it. Before you write one, read
[writing-kb-entries.md](writing-kb-entries.md) — the KB entry format (frontmatter
+ relative-inline links) and what belongs in a KB versus a memory block or `docs/`.

The helper follows a simple rule: reads are transparent through shell and the
lanius CLI; writes are gated through proposals, approvals, or explicit human
commands. That keeps setup conversational without making surprising changes.

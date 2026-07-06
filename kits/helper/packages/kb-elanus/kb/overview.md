# Elanus Overview

Elanus is an orchestration layer around agents, packages, tools, and humans. The
kernel records events, routes messages, gates authority through package grants,
and gives agents a consistent way to inspect and mutate the system.

The bus is organized into topic planes. `in/agent/<name>` is an agent mailbox,
`in/human/<owner>` is a human mailbox, `in/package/<name>/...` addresses package
actors, `obs/...` records observations and results, and `signal/...` carries
attention or interrupt-style events. Messages are durable enough to reconstruct
what happened later.

Agents are profiles with an agent noun, owner, model configuration, visible
packages, prompt blocks, and sandbox policy. A profile can run foreground with
`elanus agent run` or be queued with `elanus agent spawn` when an approved exec
handler subscribes to its mailbox.

Packages supply capabilities: skills, context stages, tools, daemons, cron jobs,
or exec handlers. Discovery is visibility, not authority. A package can be on an
agent's path while its requested capability remains pending until approved.

Knowledge bases are plain `kb/` folders inside packages that declare a `[kb]`
marker. Use `elanus kb list` to find them, `elanus kb search` or the
`search_knowledge` tool to query them, and `elanus kb write` to add durable
knowledge where the package and grants allow it.

The helper follows a simple rule: reads are transparent through shell and the
elanus CLI; writes are gated through proposals, approvals, or explicit human
commands. That keeps setup conversational without making surprising changes.

---
name: Risk and trust
description: Cheap setup and capability surfaces that give Ganesh confidence without building a full enterprise console.
---

# Risk And Trust

Ganesh does not need to love agents. He needs to know where risk comes from and
how it is mitigated.

This does not require a full admin product yet. The first pass should make local
risk visible.

## Questions Ganesh Asks

- What is installed?
- What is running?
- What can write files?
- What opens a local HTTP service?
- What can publish/subscribe on the bus?
- What was approved by a human?
- What changed since approval?
- Where is the data stored?
- How do updates roll out?
- How do I turn it off?

## Cheap UI Surfaces

System status card:

- active root
- database/log/history locations
- local ports
- active principal
- credential present/missing

Capability risk badges:

- writes files
- runs daemon process
- opens HTTP
- handles hooks
- exposes MCP
- agent-tunable
- pending approval
- changed since approval

Capability detail:

- source path
- manifest summary
- grants requested/granted
- approval state
- disable/remove action
- link to recent events

## Acceptance Criteria

- A user can tell the difference between installed, approved, running, and failed.
- A user can see the local filesystem/network footprint before enabling a capability.
- A user can copy or export a short security summary for the current root.


---
name: Claude Code setup
description: Early questions and journey notes for making Claude Code a concrete setup path in Elanus.
---

# Claude Code Setup

This topic is not specified yet. Do not let the UI pretend it is already clear.
The important current signal is that Daniel may arrive trying to "add Claude
Code to the agent system" before he understands Elanus.

## Character Pull

Daniel is not looking for a chat partner or a new hobby. He wants to know if
Elanus makes his existing coding-agent workflow more useful, safer, cheaper, or
easier to operate.

Tim wants the underlying architecture to remain real and inspectable: profiles,
packages, ledger, bus, approvals, context, and workdirs.

Lily may eventually understand this as "give my agent coding powers," not as
"configure a CLI bridge."

## Open Questions

- Is Claude Code launched by Elanus, observed by Elanus, or connected as an
  external tool?
- Is the unit a package, a capability, an agent template, or a profile mode?
- Does it run inside an agent workdir?
- Does it need approval/grants?
- Does it produce transcripts/history in the same place as other agents?
- Does it need a model/provider config, or does Claude Code own that itself?
- What is the smallest useful demo?

## UI Implications

The user-facing path should probably say "Claude Code" or "coding agent," not
"kit" or "package."

A first pass should show:

- what will be installed or connected
- what command/process will run
- what directory it can touch
- what data will be recorded
- how to test that it works
- how to disable/remove it

## Acceptance Criteria

- Daniel can find the Claude Code path from setup without knowing Elanus internals.
- The UI explains why using Claude Code through Elanus is better than using it
  directly.
- Tim can expand details and see the actual package/profile/config mechanics.
- If the integration is not configured, the UI says exactly what is missing.


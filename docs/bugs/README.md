# behavioral investigations

These documents explain places where the product people see and the system
that actually runs have drifted apart. They are not a ticket queue and they do
not assume that a familiar UI pattern is the right fix.

## Start here

- Worker navigation, Activity, History, Runs, chat, and subagent visibility:
  read [worker-surfaces.md](worker-surfaces.md).
- Claude Code exits accompanied by `adapter-summary.json` ENOENT and a refused
  session credential: read
  [claude-code-adapter-summary-credential-crash.md](claude-code-adapter-summary-credential-crash.md).

## How to use these documents

Each investigation should separate what was observed, what the code currently
means, and what remains unknown. Read one before changing the affected surface;
turn it into a journey or handoff only after the desired experience becomes
clear.

## Contents

- [worker-surfaces.md](worker-surfaces.md) — a live and code-level investigation
  of the Claude Code/Codex worker surfaces and the missing explanation between
  their different data planes.
- [claude-code-adapter-summary-credential-crash.md](claude-code-adapter-summary-credential-crash.md)
  — incident reconstruction separating a stale deployed Claude adapter from a
  same-principal credential collision during driven resume.

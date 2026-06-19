# handoffs

Forward-looking implementation handoffs: work plans with milestones and
acceptance criteria, written to be picked up by another implementer (often
Codex). Each handoff carries its own "read these first" list and a Log section
for resolving open questions as the work proceeds. These are work orders, not
design records — the design lives in the docs/ files each handoff cites.

Distinct from the repo-root `HANDOFF.md`, which is gitignored local working
context for the pass currently in flight.

## Contents

- [coding-agents.md](coding-agents.md) - launch and supervise Codex and Claude
  Code under elanus as one envelope, two adapters: cage, hook→bus record,
  mailbox delivery, memory/context via the prompt hook, and the planner/worker
  orchestration loop. **M0 launcher + M1 hook→bus bridge landed for the Claude
  Code adapter** (2026-06-19, `elanus code`, [../../src/codeagent.rs](../../src/codeagent.rs));
  M2–M5 and the Codex adapter remain — see the handoff Log for as-built decisions.
  Backed by the one coding-agents journey
  [../journeys/02-claude-code.md](../journeys/02-claude-code.md) (the why); the
  Codex and Claude Code adapter references are Appendices A and B of the handoff.
- [configuration-ux.md](configuration-ux.md) - the configuration-UX altitude and
  scope pass on the web UI (instance vs agent config, essentials vs advanced,
  the off switch). Backed by [../journeys/06-configuration.md](../journeys/06-configuration.md).

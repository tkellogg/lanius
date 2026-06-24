# core kit

The harness teaching itself: skills that explain elanus to the agents
running inside it, plus the escalation path to a stronger model.

- **harness-doctrine** — how this place works: the topic planes and what
  each delivery contract promises, the mailbox model, grants vs leases,
  the cage and the camera. Read before doing anything clever.
- **self-modify** — the edit→re-review loop: agents may build and change
  packages, but every edit de-approves; the human commits. How to propose
  changes that land.
- **escalate** — when a task outclasses your model (harness modification,
  kernel debugging, designs with tradeoffs), dispatch it to the
  `architect` profile instead of grinding: one `emit_event`, deliberately
  uncorrelated.
- **comms-etiquette** — how agents talk to each other: `deliver`/`spawn`/
  `inbox`, when to set priority (and what high-priority mail does
  mid-cycle), shared-room edit claims and the opt-in channel block, and the
  failure-mail contract. Read before dispatching work or coordinating with
  siblings.
- **profiles/architect** — the strong-model identity the escalation
  targets: high turn budget, full skill visibility. Point its `[model]`
  at the strongest model you have credentials for.

Install: `elanus kit add core`. The skills are content-only (no grants to
approve); the architect profile is yours to edit — especially the model
line and any `[sandbox]` policy you want it caged by.

Try it: `elanus emit in/agent/architect --payload \
'{"prompt":"introduce yourself and read your skills","profile":"architect"}'`

---
name: The helper
description: What the built-in helper agent is for — a UI concierge with view context — and how its first real encounter went wrong (2026-07-08 walkthrough)
---

# What the helper is supposed to be

The helper is not "another agent that happens to be preinstalled." Tim's intent
(stated during the 2026-07-08 walkthrough): the helper is a **UI concierge with
deep integration into the UI itself**. The load-bearing idea is the "Ask"
button — every potentially confusing UI element gets one nearby, and clicking it
opens the helper *already knowing what you're looking at*: the pane, the
selected agent, the row you were hovering, the error on screen. You don't
describe your problem to it; it can see your problem.

That reframes everything about how it should be configured and presented:

- Its context is the **current view**, refreshed as you navigate — not a
  workdir, not a repo.
- Its package set should reflect that job. In the walkthrough it had *the same
  packages enabled as the Claude Code agent*, which prompted exactly the right
  question: "what's the real difference here?" If the helper's config is
  indistinguishable from a coding agent's, the product hasn't decided what the
  helper is. A concierge doesn't need a coding harness; it needs read access to
  UI state and the docs/KB.
- It's a candidate answer to several other journeys' pain: Lily's configure-tab
  overwhelm ([06-configuration.md](06-configuration.md)), Daniel's "what does
  this row even mean," Ganesh's "what would this setting let an agent do." The
  Ask button is how the UI explains itself without every pane carrying a
  paragraph of copy.

# How the first encounter actually went

1. Tim opened the helper tab and typed a simple message. **No typing indicator,
   no response, no recourse** — the dead-air failure, same triple absence as
   the main chat pane (see [07-chatting.md](07-chatting.md) "Dead air").
2. Then, a surprise: a "Helper" agent appeared *live* in the left-hand agent
   list. His reaction — "What did I activate? Can I make it go away? I didn't
   mean to start something, I was just clicking on stuff."
3. Clicking into the new Helper agent, its Converse pane showed **no
   messages** — not even the one he'd just sent. So the message went somewhere
   (or nowhere), the agent it spawned doesn't have it, and no surface explains
   the relationship between the helper *tab* and the helper *agent*.

# The expectations that fall out

- **Opening a tab must not create a durable thing.** Looking is free; if
  merely visiting the helper tab spawns a resident agent, that violates the
  most basic UI contract (browse ≠ commit). If the helper genuinely needs to
  start on first contact, say so and start it visibly: "Starting your
  helper…" — and even then, starting on *message sent* is better than on
  *tab opened*.
- **Anything that appears has a "make it go away."** A creature the user
  didn't knowingly create, sitting live in the agent list with no dismiss
  affordance, reads as the software doing things behind their back. This is
  the audit-not-restriction principle in miniature: automagical is fine, but
  the UI must be able to explain *what happened and why*, and offer the undo.
- **One helper, one thread.** The helper tab and the helper agent must be the
  same surface showing the same conversation. A message typed in one place
  appearing in neither is the worst of all worlds.
- **The helper is exempt from feeling like infrastructure.** For Lily and
  Daniel this may be the very first agent they ever talk to. If its first
  answer is dead air, it has taught them the product's core loop doesn't work.

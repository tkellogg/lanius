---
name: escalate
description: Hand a task to the architect — a stronger model with a bigger budget — instead of grinding past your depth. One emit_event; know when to use it.
---

# escalate

Some work outclasses the model you're running on. The harness ships a
stronger identity for exactly that: the `architect` profile — a frontier
model, a high turn budget, full skill visibility. Escalating is one tool
call:

```json
emit_event {
  "type": "in/agent/architect",
  "payload": {
    "prompt": "<the task, self-contained — see below>",
    "profile": "architect"
  }
}
```

That's mail to the architect's mailbox; `payload.profile` selects its
identity for the run. The emission is **deliberately uncorrelated** — the
architect's run is its own flow, and its results come back as mail or as
durable artifacts, not as a reply stitched into your conversation. Fire
it and finish your own job (which may just be: report that you escalated
and to whom).

## When to escalate

- **Harness modification beyond package-land**: kernel code, broker or
  dispatcher behavior, anything where a wrong guess corrupts the system
  rather than just failing.
- **Designs with real tradeoffs**: when you notice you're enumerating
  options instead of knowing the answer.
- **Repeated failure**: the same error twice after honest attempts —
  stop, escalate, include both attempts.

Don't escalate work that's merely long, or questions a human should
decide (authority questions go to `ask_human` / the owner's mailbox, not
to a bigger model).

## Write the prompt like a handoff

The architect starts FRESH — sessions don't share memory. Its context is
only what you put in the prompt plus what it digs up itself. Include:
the goal, what you tried and what happened (paste the actual errors),
file paths involved, and where to leave the result. One self-contained
paragraph beats ten clarifying round trips, because there are no round
trips.

The architect costs real money per run. One well-fed escalation is
cheap; a vague one that has to re-discover your context is not.

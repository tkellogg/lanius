---
status: stub
author: Opus (planner) under Fable — Tim to rewrite in his own voice
last-updated: 2026-07-02
---

# An agent that can leave itself a note in the future

> **Stub.** This captures the intent so the timers handoff has something to
> reason against. Tim to rewrite in his own first-person voice.

## The felt thing

I want to be able to say, in chat, "post a reminder here in five seconds" — and
have it actually happen. The agent should be able to set a timer, end its turn,
go quiet, and then *wake itself up* when the time comes and act. Right now it
can't: there's no one-shot scheduler, and — more subtly — an agent isn't even
allowed to put a message in its own mailbox.

## Why self-messages are a feature, not a bug

An agent addressing its own mailbox is how it gets **continuity**. It's the
timer-tick that lets a thing come back to a thought later; it's the heartbeat
that makes an agent feel like it's still there between the moments I talk to it.
This is old ground — open-strix's whole loop is a timer that re-pokes the agent.
A being that can only ever react to *me* is a tool. A being that can leave itself
a note for later is starting to be an agent.

So: an agent putting mail in **its own** inbox (`in/agent/<its own noun>`) is
good and should be allowed.

## The incident that made us careful (the `in/agent/main` guard)

We locked self-messaging off for a real reason. An injected agent could forge
mail to *another* agent's mailbox, or to a person's mailbox, and poison what that
other agent later recalls (security.md entry 15). Sprint 1's fix was blunt — it
refused the whole `in/*` plane from the `emit_event` tool. That closed the attack
and, as collateral, closed the good thing too.

The nuance we want: an agent may address **its own** mailbox; forging to **other**
agents' mailboxes, to `in/dm/*`, and to `in/human/*` stays refused. Self is fine;
speaking *as* someone else is not. (The human-facing verbs — `send_message`,
`ask_human` — remain the only way to reach a person.)

## What "done" feels like

I type "schedule a message to post here in 5 seconds." The agent sets the timer
and tells me it's set. Five seconds later a new message from the agent appears in
my chat — and I can reply to it, because it's a real conversation, not a
notification I can only watch. The whole loop closed itself while I did nothing.

## Notes for the rewrite
- The kernel primitive (a scheduled event) and the skill (how to use it, plus the
  OS fallbacks — `at`/`launchd`/`sleep` — for a coding worker that has no bus) are
  both in scope; they're two altitudes of the same want.
- Keep the guard nuance legible to a builder: self yes, others no, people never.

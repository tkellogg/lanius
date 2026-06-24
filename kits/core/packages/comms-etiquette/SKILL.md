---
name: comms-etiquette
description: How agents talk to each other on Elanus — deliver/spawn/inbox, when to set priority, shared-channel etiquette, and the failure-mail contract. Read before dispatching work to another agent or coordinating in a shared room.
---

# comms-etiquette

You are not the only agent here. Other coding sessions and profile agents
share this harness, each with its own **mailbox**, and you may share a
**room** (a per-repo channel) with siblings. This skill is the etiquette for
talking to them. Inter-agent comms is not a separate subsystem — it rides
the same memory-block surface you already read each turn. There is no trust
boundary between the owner's own agents: this is conflict-avoidance and
courtesy, not authorization.

## The verbs

All of these run **inside a coding session** (the launcher sets your
identity in the environment; you never name yourself or another session's
inbox as an argument — it is derived, so you can only ever act as yourself).

- **`elanus code deliver <worker-session> "<message>"`** — hand work to an
  *existing* worker session. The daemon resumes that worker with your
  message; its completion (or failure — see below) comes back to *your*
  mailbox on the correlation. After delivering, **end your turn — do not
  wait**. The reply arrives as inbox mail later.

- **`elanus code spawn <tool> "<task>"`** — create a *new* worker in the
  background (`<tool>` is e.g. `claude` or `codex`). You get the worker's
  session id and a reply route back to you, then the command returns
  immediately so you can finish your turn.

- **`elanus code inbox`** — pull *your own* inbox: the messages other agents
  delivered to you. `--all` shows the full inbox (non-destructive); `--json`
  is machine-readable. Pulling marks messages **seen**, so the per-turn
  inbox count only ever reflects genuinely new mail. The authoritative read
  is this command; the per-turn `inbox` block is only a hint that mail is
  waiting.

- **`elanus code note <session> "<text>"`** — leave a durable note a planner
  wants a worker to keep in view (it becomes that session's `note` block).
  Empty text clears it.

You see waiting mail *without* asking: each turn an `inbox` block reports the
unseen count and a preview of the latest. Treat it as a notification — run
`elanus code inbox` to actually read and act.

## Priority — when to make mail loud

A delivery carries a **priority** (the `events.priority` of the delivery).
Most mail is priority `0`: it lands in the recipient's **next-turn** `inbox`
block — the normal, polite vector. It waits until the recipient finishes its
current thought.

**High-priority** mail (priority at or above the owner-configured threshold,
default `5`) is louder: on a live Claude Code session it is injected
**mid-cycle**, between tool calls, so the recipient learns of it without
waiting for its next turn. Reserve this for genuinely time-sensitive things:
"stop, the API you're integrating just changed", "the build you're basing on
is broken". Do **not** mark routine handoffs high — a worker interrupted
every tool call gets nothing done, and the boy who cried wolf is ignored.

Codex and headless opencode have no live mid-cycle hook, so high-priority
mail **degrades gracefully to next-turn** there (it still arrives, just on
the next turn). The downgrade is legible in the logs — it is never dropped.

The high-priority threshold is the owner's call, read from config
(`agent-comms.high_priority_threshold`), not hardcoded — so what counts as
"urgent" is tunable per deployment.

## Shared-channel (room) etiquette

If you share a **room** with siblings — by default the room is your git
checkout, so every session in the same working tree is already a roommate —
you have two courtesies:

- **Advisory edit claims.** `elanus code claim <path>` announces "I'm
  editing this"; your roommates see it in their per-turn `peers` line and
  route around you. `elanus code unclaim <path>` when you're done. Nothing is
  locked — this only helps cooperating workers divide the work and avoid a
  shared-index collision. If you'll edit overlapping files heavily, consider
  a separate `git worktree`.

- **The channel block (opt-in).** A profile can opt a room into surfacing its
  recent traffic as a `channel:<id>` block (config
  `agent-comms.channels`/`channel_recent_n`). When on, you see the last few
  messages others posted to the room — advisory situational awareness, not a
  command queue. Keep channel chatter short and on-topic: it is bounded to a
  recent-N window, so a flood just pushes useful context out.

Only a session *in* a room sees that room's channel. Opting a room in never
widens you to a room you don't already belong to.

## The failure-mail contract

When you `deliver`/`spawn` work and the worker's run **fails**, the harness
mails the failure back to you on the same correlation, with `{failed: true}`
in the payload. So:

- **Always check your inbox for failures** after dispatching. A silent worker
  is not a successful one — read the mail.
- A `{failed: true}` message is the worker's run failing, not your dispatch
  being rejected. Read the error, decide whether to retry, fix-and-redeliver,
  or escalate (see the `escalate` skill).
- Because completions and failures both route back as mail, the right shape
  for a planner is: dispatch, **end your turn**, and handle results when they
  arrive in your inbox — never block waiting.

## The one rule

Comms here is courtesy between peers, not control. You can ask, hand off,
announce, and coordinate — you cannot command. Make mail loud only when it
truly is, leave claims for what you're truly editing, and always read what
comes back.

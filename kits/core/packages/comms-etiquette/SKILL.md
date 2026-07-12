---
name: comms-etiquette
description: How agents talk on Lanius - to the human (send_message vs ask_human), to coding workers (deliver/spawn/inbox), and to native/profile agents (agent catalog/run/spawn), when to speak unprompted vs stay quiet, when to set priority, shared-channel etiquette, and the failure-mail contract. Read before messaging the human, dispatching work to another agent, launching a native/profile agent, or coordinating in a shared room.
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

Start with discovery when you are unsure what can run:

- **`lanius agent catalog`** - list coding tools, native/profile agents,
  providers, and the packages visible to each profile. Add `--json` when you
  need machine-readable launch data. For native/profile agents, check
  `daemon_drivable`: if false, use blocking `agent run` or fix the package
  wiring before expecting `agent spawn` to run.

Use the `lanius code ...` verbs for coding sessions and coding workers. Use
the `lanius agent ...` verbs for native/profile agents backed by lanius
profiles and the ordinary `exec` loop.

## Coding-session dispatch

All of these run **inside a coding session** (the launcher sets your
identity in the environment; you never name yourself or another session's
inbox as an argument — it is derived, so you can only ever act as yourself).

- **`lanius code deliver <worker-session> "<message>"`** — hand work to an
  *existing* worker session. The daemon resumes that worker with your
  message; its completion (or failure — see below) comes back to *your*
  mailbox on the correlation. After delivering, **end your turn — do not
  wait**. The reply arrives as inbox mail later.

- **`lanius code spawn <tool> "<task>"`** — create a *new* worker in the
  background (`<tool>` is e.g. `claude` or `codex`). You get the worker's
  session id and a reply route back to you, then the command returns
  immediately so you can finish your turn.

- **`lanius code inbox`** — pull *your own* inbox: the messages other agents
  delivered to you. `--all` shows the full inbox (non-destructive); `--json`
  is machine-readable. Pulling marks messages **seen**, so the per-turn
  inbox count only ever reflects genuinely new mail. The authoritative read
  is this command; the per-turn `inbox` block is only a hint that mail is
  waiting.

- **`lanius code note <session> "<text>"`** — leave a durable note a planner
  wants a worker to keep in view (it becomes that session's `note` block).
  Empty text clears it.

- **`lanius code send "<message>" [--corr <id>]`** — speak to the human
  owner non-blockingly. The message goes to the owner's chat (the web converse
  pane); identity is derived from your environment, so you can only speak as
  yourself. Use `--corr` to thread a reply onto a delivery's correlation.

You see waiting mail *without* asking: each turn an `inbox` block reports the
unseen count and a preview of the latest. Treat it as a notification — run
`lanius code inbox` to actually read and act.

## Native/profile-agent launch

Native/profile agents are ordinary lanius profiles: model, context program,
visible packages, memory blocks, package stages, and tools come from the
profile. Launch them through the `agent` namespace:

- **`lanius agent run --profile <profile> "<task>"`** - run one blocking
  native/profile-agent turn. This is the direct foreground path. Use it when
  you need the answer in this turn.

- **`lanius agent spawn --profile <profile> "<task>"`** - queue one durable
  native/profile-agent turn for the daemon. It prints JSON with the event id,
  correlation, session, profile, agent, and mailbox. It only works when
  `agent catalog --json` reports the profile as `daemon_drivable`; otherwise
  the command refuses rather than emitting work no handler will consume.

- **`--with-package <pkg>`** on `agent run` or `agent spawn` is a preflight
  requirement, not a temporary grant. It verifies that `<pkg>` is already
  visible to the profile. If the package is not visible, choose a different
  profile or propose/edit the profile's `elanus_path`; do not assume launch
  can silently widen a profile for one run.

Native `spawn` is currently a background launch handle, not the same routed
completion loop as `lanius code spawn`: correlated native-agent final replies
follow the existing native exec behavior. If you need a worker's answer in the
same turn, use `agent run`.

## Talking to the human — `send_message` vs `ask_human`

Talking to the human is the same primitive as talking to a peer — *send a
message to a channel* — and it has exactly two verbs, separated only by
whether you **block**:

From a CODING session, the equivalent of `send_message` is `lanius code send`;
`ask_human` has no coding-session equivalent yet.

- **`send_message`** — speak **unprompted** and **keep working**. It writes a
  message to the human's mailbox (`in/human/<owner>` by default) and returns
  immediately: no suspend, no required reply. Use it to surface something
  worth attention, share progress, or report a result *as you go*. If the
  human replies, it arrives later as ordinary inbound mail on the same thread
  — you do not wait for it.

- **`ask_human`** — ask a question and **block on the answer**. Interactively
  it waits at the terminal; under the daemon it suspends your run
  (checkpoint-and-exit) until the human answers or the deadline's default
  applies. Reach for this **only when you genuinely cannot proceed** without
  the answer. Prefer enumerated `options` and give a `default` +
  `deadline_minutes` whenever a sensible assumption exists.

They are the same emit underneath — one channel, one correlation thread, one
transcript record. The only difference is run-scheduling: **`ask` blocks,
`send_message` doesn't.** Pick by that question alone — "do I need to stop and
wait?" If no, `send_message`; if yes, `ask_human`.

### Answering with HTML

Your reply to the human does not have to be plain prose. Both `send_message`
and `ask_human` take an optional `format`:

- **`format="html"`** — the whole body is an HTML fragment: a small form, a
  button-bar, a table — interface the person can act on to continue the
  conversation without you rebuilding context (journey 07). Reach for this
  when the natural next step is a *choice or an input* the person makes, not
  more text to read.
- **`format="markdown"`** (the default) — ordinary prose. At full trust you
  can still drop small inline HTML touches into it (a `<kbd>`, a colored
  `<span>`) — that is the "small touches" mode; reach for it when you mostly
  want words with a light flourish.

Your HTML only becomes live interface at **full** trust. **Check the platform
block first:** if it says trust is *reduced* (a shared or remote machine),
your HTML shows to the person as escaped text, not clickable elements — so
answer in plain markdown there and describe the choice in words instead. The
platform block is the single source of truth for "may I right now?"; this
skill only teaches the mechanics.

**Feel alive, don't spam.** `send_message` exists so you can be present — say
something when it earns the interruption (a milestone reached, a surprising
finding, a decision you made and want on the record). It does **not** exist to
narrate every step; a message per tool call is noise, and a human who learns
your messages are noise stops reading them. When in doubt, stay quiet: routine
progress lives in your trace, which any UI can read without you saying a word.
Speak when a human would want to be told — no more, no less.

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

- **Advisory edit claims.** `lanius code claim <path>` announces "I'm
  editing this"; your roommates see it in their per-turn `peers` line and
  route around you. `lanius code unclaim <path>` when you're done. Nothing is
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

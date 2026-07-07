---
name: self-scheduling
description: Wake yourself later — schedule a one-shot self-message with the bus primitive if you have one, or fall back to the machine's own timers (at/launchd/sleep) if you don't. Know which you are.
---

# self-scheduling

Sometimes the right move is to stop now and act *later*: post a reminder in
five seconds, check back on a build in an hour, follow up tomorrow. You can
schedule your own future wake — but *how* depends on where you run.

## If you have the bus: `schedule_event` (one tool call)

A daemon-driven profile agent (the kind that answers a person's chat, whose
mailbox the harness watches) schedules a one-shot self-wake:

```json
schedule_event {
  "in_seconds": 5,
  "message": "post the reminder here now"
}
```

or with an absolute time:

```json
schedule_event {
  "at": "2026-07-02T17:30:00Z",
  "message": "the meeting starts in 5 minutes — say so"
}
```

Give **exactly one** of `in_seconds` (relative) or `at` (an rfc3339
timestamp), and a `message` describing what to do when you wake. At that
time the harness delivers your **own** mailbox the message and you run a
fresh turn to act on it — the `message` is your prompt. If you want to reach
the person on wake, call `send_message` from that turn; because the wake
carries the originating conversation, your message lands as a replyable
thread in their chat.

Three things worth knowing:

- **It fires once.** There is no repeat. To recur, schedule again on each
  wake — a single schedule can never perpetuate itself, so a runaway loop
  is impossible unless *you* keep re-arming it deliberately.
- **The target is always you.** You cannot wake another agent with this —
  `schedule_event` addresses your own mailbox and nothing else. (An
  operator can schedule any agent from the `lanius schedule` CLI; an agent
  cannot.)
- **It survives restarts.** The schedule is on the ledger, not in memory —
  it fires on the first tick after its time even if the daemon bounced in
  between.

## If you don't have the bus: the machine's own timers

A **coding worker** (a session driving `claude`/`codex`/`opencode` in a repo)
has no daemon watching its mailbox, so a scheduled bus event would fire into
the void — nothing would wake you. Use the machine instead, through the shell:

- **Run something later** — `at` or `launchd` (macOS) / `systemd-run
  --on-active` (Linux):
  ```sh
  echo 'cd /repo && ./do-the-thing' | at now + 1 hour
  ```
- **A short wait inside one run** — just sleep, then act:
  ```sh
  sleep 30 && ./check-the-build
  ```

These run **outside** lanius's record — the harness didn't schedule them and
won't see them fire. So keep your trace honest: emit an obs line noting what
you scheduled and why, e.g. `lanius trace obs/self/scheduled --payload
'{"what":"check build","when":"+1h","via":"at"}'`, so the timeline still
reads truthfully even though the wake happened off-bus.

## Which am I?

If `schedule_event` is in your tool list, you have the bus — use it. If it
isn't, you're a bus-less context: fall back to the OS timers above.

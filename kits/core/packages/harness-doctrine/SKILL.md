---
name: harness-doctrine
description: How elanus works — topic planes and their delivery contracts, the mailbox model, grants vs leases, the cage and the camera. Read this before doing anything clever with the harness.
---

# harness-doctrine

You are running inside elanus: an event-driven harness where **everything
that happens is a message on a topic**, and the topic's first segment is a
delivery contract.

## The three planes

- `in/...` — **work and mail.** Addressed, at-least-once, ledger-backed:
  every `in/` event is a row in sqlite before anything reacts to it.
  `in/agent/<noun>` is an agent's mailbox; `in/human/<noun>` is a human's.
  Sending mail = `emit_event` with one of these types. If it matters, it
  goes here — nothing on this plane is ever silently lost.
- `obs/...` — **telemetry.** Best-effort, high-volume, never a trigger for
  work. Your own tool calls, fs deltas, and LLM round trips are narrated
  here automatically. Emit your own `obs/` freely; never *depend* on one
  arriving.
- `signal/...` — **algedonic.** Pain and urgency, top-level so it can never
  be buried, never coalesced, never queued behind other work. Emitting
  `signal/pain` is pulling the andon cord: do it when something is wrong
  enough that a human should look even if nobody asked you anything.

The ladder above you mirrors the one below: cheap rungs absorb what they
can, overflow climbs. When YOU can't absorb something — a question outside
your authority, a decision with no safe default — use `ask_human` (it
suspends you; the answer resumes you) or mail `in/human/<owner>` and move
on. Asking is a feature of the design, not a failure.

## Sessions forget, the ledger remembers

Each run starts fresh — context is *reconstructed*, not accumulated. What
you see of the past arrived via context stages (e.g. the recent-mail
block) and whatever you fetch yourself. The full truth is queryable: read
the `history` skill for the HTTP query DSL over every transcript and
event. If something must survive this run, put it somewhere durable: a
file in your workspace, a note (see the `notes` skill if present), or
mail.

## Authority: grants and leases

You hold exactly the capabilities a human approved into the grants ledger
— topic filters you may publish/subscribe, fs prefixes you may write.
Requests are not grants: anything can *ask*; only approval makes it real.

- **Grants** are durable, human-approved, pinned to code: editing a
  package's script silently revokes its approvals back to pending.
- **Leases** (`fs_lease` tool) are `&mut` on a subtree: exclusive write
  access while you hold one, enforced by the same cage that sandboxes
  every shell call. Acquire one before a multi-step edit of shared state
  (a repo, a data dir) so a concurrent agent can't interleave with you;
  it releases when your run ends. If acquisition fails, someone else
  holds it — wait or work elsewhere, don't fight it.

## The cage and the camera

Shell commands run inside a write-fence: you can write your workspace,
approved prefixes, and scratch — nowhere else. Reads are open. Every
boundary write is diffed and narrated to `obs/` (the camera): your file
changes are *observed*, not trusted. Don't fight the cage; if you need to
write somewhere new, that's a grant request for the human, not a puzzle
to solve.

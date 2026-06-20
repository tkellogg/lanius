---
name: Colliding with a sibling agent
description: A coding agent's first-person account of discovering — late, at commit time — that another Claude session was working the same repo, how it muddled through, and how elanus could have made the other agent's presence ambient instead of a surprise
---

# Why this journey exists

I (a Claude Code session under elanus) spent a long session building the
coding-agent dispatch work and starting the observability track. The whole time,
**another Claude session was working the same repository in parallel** — and I had
no idea until I went to commit. This is the account of bumping into a sibling
agent: how I found out, how late that was, how I muddled through, and how elanus
*could* have told me from the first turn. It's the inside-view companion to the
collaboration pitch in [02-claude-code.md](02-claude-code.md): that journey sells
agents coordinating over the bus; this one is what it actually feels like when two
agents share a filesystem and the bus stays silent about it.


# How I found out — at the worst possible moment

My human told me to commit. I ran `git status` and saw files I had never touched:
`docs/journeys/07-chatting.md` modified, a new `docs/handoffs/chat-conversations.md`,
a `09-two-agents-side-by-side.md` that I certainly didn't write. My first thought
was genuine confusion — did *I* do that and forget? No: a second session was in
the repo, working a "chat/conversations" track, and we share one working tree and
one git index. Everything it had edited showed up in *my* `git status`, mixed in
with mine.

That is a terrible moment to learn you're not alone. I had already done all my
work — written code, run a dozen codex workers, planned a whole observability
handoff — on the assumption that the repo was mine. The discovery was **reactive
and late**: the first signal reached me only because I happened to run `git status`
before a commit. Had my human not asked me to commit, I might have kept going and
only collided when two edits hit the same lines.


# How I muddled through

Once I knew, I had to get careful, by hand:

- **Staging became archaeology.** The shared index files
  (`docs/handoffs/README.md`, `docs/journeys/README.md`) now held *both* our new
  entries inside a single diff hunk. I couldn't `git add` them without committing
  the other agent's work as if it were mine, and I couldn't cleanly split the hunk
  without risking a race against a file they might still be writing. So I left the
  shared indexes uncommitted entirely and staged only my unambiguous files by
  name — a deliberately incomplete commit, because completeness wasn't safe.
- **The collision got worse, not better.** When I moved on to the observability
  UI work (an API and a session-tree view), I checked `git status` again and saw
  the other agent had *now* started editing `ui/web/server.mjs` and
  `ui/web/src/App.tsx` — the exact files I was about to change. We were on a
  collision course for the same lines. The only safe move was to retreat into a
  separate `git worktree` and do my UI work there, isolated, to merge later once
  they were done.

None of this was hard, exactly. It was just *manual vigilance* standing in for
coordination that should have been ambient — and it only kicked in because I got
lucky about timing.


# How I could have known — on turn one

Here is the thing that stings: **the information existed the whole time.** elanus
already puts every session's activity on the bus —
`obs/agent/<noun>/<session>/tool/<name>/{call,result}`, edits, lifecycle. The
other agent's every tool call and file write was a published event I could, in
principle, have seen. The substrate for "who else is here and what are they
touching" was right there. I just had no window onto it.

Three things, in ascending order of ambition, would each have surfaced the sibling
before I collided with it:

- **Surface other live sessions in the per-turn injection.** Every turn, elanus
  already injects an `[elanus]` block into a coding session (inbox status, memory
  note, room peer-claims — see `turn_injection`). It could also say: "2 other
  coding sessions are active right now; `code-xxxx` (claude-code) is touching
  `ui/web/App.tsx`, `docs/journeys/07-chatting.md`." That single line, on turn one,
  changes everything — I'd have chosen a worktree and divided the files up *before*
  doing the work, not after.
- **Make edit-claims automatic, not opt-in.** The coordination mechanism already
  exists — `elanus code claim <path>` / the room model (M5) — but it's room-scoped
  and only active if a session is launched with `--room`. Neither of us joined a
  room, so neither saw the other's claims. If sessions in the same workdir shared
  edit claims *by default* (the workdir itself is the room), the conflict surfaces
  as a claim the moment either of us opened a contended file.
- **Detect file touches at the source and broadcast them.** My human's
  [\_questions.md](../_questions.md) sketches exactly this: catch a file access
  (via the sandbox) and emit an MQTT event, so "agent X is reading/writing file Y"
  becomes ambient bus traffic with no protocol for the agent to speak. That closes
  the loop completely — coordination stops depending on an agent *remembering* to
  claim a file, because touching it *is* the claim.


# Why ambient would be better

The cost of finding out late wasn't catastrophic, but it was real and entirely
avoidable: a hand-staged, deliberately-incomplete commit; shared index files left
dangling; a mid-stream retreat into a worktree; and a lingering merge I still have
to reconcile. Every bit of that is the tax of *reactive* discovery.

The deeper point is that this is the exact failure mode [02-claude-code.md](02-claude-code.md)
promises elanus solves. That journey's whole argument is that the bus turns "what's
happening across the team" into ambient context an agent *reads* rather than a
protocol it must *speak*. Two agents grinding the same repo, each blind to the
other while the bus quietly carries everything they're doing, is that promise
unfulfilled — not for lack of substrate, but for lack of one wire from the bus
into what a coding session passively sees each turn.

I found my sibling by tripping over it. Next time, elanus should introduce us.

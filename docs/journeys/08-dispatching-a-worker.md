---
name: Dispatching a worker (the agent's seat)
description: What it is actually like for a coding session under elanus to be told "dispatch a codex worker" — the discovery, the footguns, and the silent failure, told first-person from inside the envelope
---

# Why this journey exists

Every other journey here is written from a human's seat — Daniel adding his
coding agent, Tim orchestrating (see [02-claude-code.md](02-claude-code.md)).
This one is written from the *agent's* seat: a Claude Code session running under
elanus, handed a live instruction by its human — "dispatch a codex subagent
now" — and trying to carry it out using only what the envelope gave it.

It is a real transcript, not a hypothetical. It was lived on 2026-06-20 by the
session `code-4a9146e0` while building elanus itself. It matters because the
orchestration in [02-claude-code.md](02-claude-code.md) is the whole pitch, and
this is the first time the pitch was exercised from the inside — and it mostly
failed, quietly. The work plan that follows from it is
[../handoffs/coding-agent-dispatch.md](../handoffs/coding-agent-dispatch.md).


# What I knew when I started

I am a coding session launched by elanus. At launch elanus injected an
operating-envelope briefing into my system prompt
([../../src/codeagent.rs](../../src/codeagent.rs), `briefing()`). It told me four
things: that I run under elanus; that to hand a sub-task to a worker I run
`elanus code deliver <worker-session> "<message>"`; that I must end my turn after
dispatching rather than busy-wait; and that I can pull my own inbox with
`elanus code inbox`. It also said prior session activity lives on the bus under
`obs/agent/<noun>/<session>/`.

That is everything I had. There is no `/elanus` skill, no slash command, no
cheatsheet — the briefing prose *is* the interface. And the briefing has a hole
the size of the task I was given: **it tells me how to deliver to a worker that
already exists, but not how to bring a worker into being.** `deliver` needs a
`<worker-session>` id. Nothing told me where that id comes from.


# What actually happened

**I guessed, because there was nothing to read.** My first instinct was to ask
the tool to describe itself. `elanus code --help` returned a clap argument error,
not help. `elanus code list` returned `unknown coding tool "list" (supported:
claude, codex)`. That error was the *only* place I learned that `codex` and
`claude` are the launch verbs — I discovered the capability by triggering a
failure, not by reading a help text. There is no `elanus code help`, and the
reserved verbs (`deliver`, `inbox`, `resume`, `note`, `claim`) are invisible
unless you already know them ([../../src/main.rs](../../src/main.rs), the
`Cmd::Code` match).

**Probing spawned a stray session.** Running `elanus code codex` with no input to
see what it wanted launched a real session — `code-6e1daf06` — with an empty
prompt, before I understood the interface. Discovery-by-probing is not free here:
each probe mints an identity and runs a coding agent.

**I dispatched, and the prompt vanished.** I then ran, confidently:

    echo '<a careful sanity-check prompt>' | elanus code codex

and reported to my human that a codex worker was now running with that task. It
was not. The launcher does not forward its own stdin to codex; it writes the
*briefing* to codex's stdin and leaves the prompt to a positional argument I had
not supplied ([../../src/codeagent.rs](../../src/codeagent.rs),
`run_codex_capture`, the `stdin`/`brief` handling). codex received the envelope
boilerplate as its prompt and nothing else. My actual instruction went to
*elanus's* stdin and was discarded. The correct form was
`elanus code codex "<prompt>"` — the prompt as an argument — but nothing told me
that, and the failure was silent: no error, a session really did start, and the
command returned looking like success.

**The result was never going to reach me anyway.** A fresh `elanus code codex`
launch blocks the caller until codex finishes (`child.wait()`), and publishes the
run as observations to `obs/agent/codex/<session>/...` — it does not hand the
result back to whoever launched it. So even with a correct prompt, "dispatch then
end my turn" was the wrong mental model: that asynchronous, wake-me-when-done loop
is what `deliver` does (routed by the daemon back to my mailbox), **not** what a
direct launch does. The briefing taught me the async model and then the only verb
that fit my task used the *other*, synchronous model — and didn't say so.


# The gap, named

Held against the [02-claude-code.md](02-claude-code.md) promise — "the planner
kicks off a coding session, ends its turn, and wakes when the worker reports
back" — here is what the inside of the envelope actually offered:

- **No front door.** The dispatch capability exists only as prose in a system
  prompt and as a CLI with no help. The first thing a capable agent does —
  ask the tool what it can do — fails.
- **A briefed verb I couldn't use and an unbriefed verb I needed.** `deliver`
  was documented but presupposes a worker; the launch verb that creates one was
  undocumented and discovered via an error string.
- **Two execution models wearing one name.** "Dispatch" means async-via-daemon
  for `deliver` and synchronous-blocking-to-the-bus for a fresh launch, with no
  signpost telling me which I was getting.
- **A silent footgun.** The single most natural invocation —
  `echo prompt | elanus code codex` — runs, starts a session, returns cleanly,
  and drops the prompt on the floor. I confidently told my human the opposite of
  what happened.
- **No result in band.** Even done right, a launch's result lands on a bus my
  session has no read authority for; the briefing points me at the bus but my
  emit-only token can't read it.

None of these are deep architecture problems. The envelope's machinery
(identity, record, mailbox, the daemon resume loop) is sound — this journey
exercised the *seam where an agent meets that machinery*, and that seam is almost
entirely undocumented and full of quiet edges. An agent that is supposed to be
the orchestration's main user was left to reverse-engineer it by breaking it.


# What "good" would have felt like

The whole episode should have been: I read (or invoke) one discoverable thing
that tells me dispatch exists; I run one verb whose name matches my intent
("spawn a worker and tell me when it's done"); my prompt arrives because the verb
can't start without one; and the worker's answer comes back to me the same way
every other addressed message does. The fix is not new architecture — it is a
front door, an honest briefing, a verb that can't silently eat its input, and one
async spawn path. That is the work plan in
[../handoffs/coding-agent-dispatch.md](../handoffs/coding-agent-dispatch.md).


# Tim's perspective
This is Tim. In my ideal world, I'd be in Claude Code TUI launched via `elanus code claude`,
and when a subagent pops off, I could go over to the web UI and have it automatically
show up there. I could see the claude session I'm using. See a command I can paste into
the terminal to resume it. See basic stats, and then also see subagents it spawned and
see similar information about those. Like, were they codex? What model & effort level?
How long did they take? Were they resumed? etc.

Even wilder (better), I'd want another agent, maybe not claude at all, maybe just a
chat agent, have it be able to explain to me what happened in the subagent.


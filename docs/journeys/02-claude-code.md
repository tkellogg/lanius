---
name: Coding agents
description: Why Tim and Daniel run Codex and Claude Code under elanus, what the operating envelope buys them, and the orchestration that earns elanus its keep
---

# One thing, two brands

Codex and Claude Code are the same kind of thing to elanus: a coding agent we did
not write, that someone wants to run, and that becomes far more valuable the
moment elanus owns the ground it stands on. We should not build a "Codex
integration" and a separate "Claude Code integration." We should build one
operating envelope and two thin adapters, because to elanus both are just an
external actor brought up from the command line (docs/actors.md: "an external
actor, no different in standing from one the kernel started").

The envelope is the whole pitch. Run a coding agent bare and you get a smart
thing loose in your filesystem with its own opaque history. Run it inside elanus
and elanus owns where it can write (the cage), what gets recorded (the camera and
the ledger), what it costs, what context it sees each turn, and — the part that
turns out to matter most — the messages flowing into and out of it. The coding
agent stays exactly itself; elanus becomes the room it works in.


# Journey: Daniel adds his coding agent

Daniel arrives wanting to "add Claude Code," or Codex — he doesn't much care
which, he cares that it's cheaper and that his boss stops asking what it cost. He
is not here for a chat partner or a new hobby. The question in his head is narrow:
does running this through elanus make my existing coding workflow cheaper, safer,
or easier to operate, or is it just another thing to configure?

So the envelope has to sell itself in the first minute, in his terms. He wants to
point it at his repo and know it can't wander outside it. He wants a hard spend
ceiling, because the one thing he already trusts elanus for is not surprising him
on a bill. He wants to be able to look back at what the thing actually did — which
commands, which edits, which git operations — without scrolling a terminal he
already closed. If elanus gives him those three (a cage he can see, a cost he can
bound, a record he can read) he stays. If adding the coding agent feels like a
project — generate this config, understand that vocabulary — he closes the tab and
runs Codex bare, and we've lost him over ceremony, not capability.

Daniel will lean on Codex specifically because it's cheap, and he'll run a lot of
it. That makes the cost ledger and the cage the features he notices first. The
orchestration below is not his reason to show up — but it's the reason he stays
once he sees what Tim is doing with it.


# Journey: Tim orchestrates

Here is the thing I actually want, and it's exactly what I'm doing by hand right
now. I work with a big-picture model that's good at seeing the whole — what the
change means, whether it's the right shape, where it'll bite later — and I hand
the details to a cheaper model that's relentless about getting every line right.
Today that handoff is me: I have the planner write a milestone to a file, I
copy it into Codex, I wait, I read the result, I carry it back to the planner to
check, and then I prompt the next milestone. I am the message bus, and I am slow
and I forget things.

Put both coding agents on elanus and that loop closes on its own. The planner
kicks off a coding session for M1, the session does the work and announces it's
done, the planner reads the result, decides it's good (or sends it back), and
moves on to M2 — and I'm watching, free to interrupt, but no longer the wire. The
planner doesn't need a special "drive Codex" tool; it publishes work into the
coding session's mailbox and wakes when the session reports back, the same
addressed-message machinery every other actor already uses.

A step past that is where it gets interesting. Run more than one coding session at
once — one writing, one verifying — and let them coordinate over the bus. Not hard
locking; these are language models, so it's advisory, the way two people working
the same codebase call out "I'm in the auth module, give me a few." A session
announces "I'm editing src/foo.rs, ping me if you need to go near it," and the
others simply see that claim in their next prompt and route around it. The bus
already has the shape for this: a room is just a noun with a mailbox
(docs/topics.md), ledger-backed so a session spawned mid-stream can read what's
already been said.

And the way each session learns about the others is the same mechanism that makes
the whole thing feel alive: a hook on the coding agent that, every turn, asks
elanus what this session should know right now and folds the answer into the
prompt — open messages in its inbox, the current edit claims, a memory block the
planner left it. It's just a hook, so it can reach into the prompt the agent is
about to send. (There's a real wrinkle in *where* in the prompt that context can
land, and what it does to caching — that's in the handoff, not papered over here.)
The point is that "what's happening across the team" becomes ambient context the
coding agent reads, not a protocol it has to speak.

That is the moment elanus stops being a nicer way to run one coding agent and
starts being the thing that lets a cheap detail-worker and an expensive
big-picture planner actually collaborate — which is the pattern I keep reaching
for and keep having to be the glue for myself.


# What both of them need from elanus

Strip the two journeys down and the coding agents need the same five things, which
is why it's one envelope:

- A **home and a cage** — a workdir they run in and a boundary they can't write
  past, with elanus as the real sandbox so the coding agent's own "dangerous"
  bypass flags are safe precisely because elanus is the actual wall.
- A **record** — their lifecycle, commands, edits, and git operations as ordered
  observations on the bus, good enough to reconstruct what happened, tied to an
  elanus session.
- A **cost** they can't exceed and a human can read.
- A **mailbox** — messages can be delivered into a session and the session's
  results come back out, so a planner (or a person) can drive it without being the
  wire.
- **Context each turn** — a seam where elanus folds in memory, inbox state, and
  coordination claims, so collaboration is something the agent reads rather than a
  protocol it must implement.

Daniel needs the first three to stay. Tim needs all five for the orchestration to
exist. They are the same five features at different depths — the altitude lesson
from the rest of these journeys, one more time.

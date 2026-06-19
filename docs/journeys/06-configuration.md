---
name: Configuring Elanus
description: Why each character opens configuration, what they expect to find, and where the two kinds of config blur together
---

# Two kinds of configuration, one word

When someone says "let me go configure that," they mean one of two very different
things, and the product currently makes them walk through nearly the same doors
for both.

The first is the **instance** — the whole installation. What capabilities exist,
how a shared package behaves, who the owner is, where the database and the config
repo live. This is the stuff that is true for the machine, not for any one agent.
Today it lives in the setup view: the capability catalog, the installed
capabilities with their little "save setting" boxes, the trust-and-footprint
card, the agent-requests list.

The second is a **single agent** — one creature's identity. Its model, its run
budget, its home directory, what packages it's allowed to see, how fast it's
allowed to think. This is the configure tab.

The thing I keep tripping on is that these two overlap in a way that isn't
honest yet. A package "setting" you change from inside an agent's configure tab
is actually shared across every agent (it's a write to the config repo). A
context-stage value you change two inches away in the same tab is for that one
agent only (it's a `vars` entry on the profile). Same package, same-looking
control, opposite blast radius, and nothing on the screen tells you which is
which. Hold that thought — it shows up in three of the four journeys below.


# Journey: Lily tunes her pets

Lily doesn't "configure an instance." She has agents — a weather-watcher, a
launch-scout, a thing that drafts her standups — and she goes to config to care
for one of them. She opens configure with a specific, warm intention: *this one
is getting expensive, let me move it to a cheaper model*, or *give my scout a
bigger allowance, it keeps stopping mid-thought*, or *point the standup one at
the right folder*.

What she expects is the same shape open-strix gave her: a place that is clearly
*about this agent*, with the money in front of her. What she actually lands on —
the moment she finishes the new-agent wizard — is the configure tab with seven
sections stacked top to bottom, and that's the overwhelm moment from setup
arriving a second time, except now there's no agent helping her, just the wall.
She wanted to chat with the thing she just made; instead she's looking at
"prepend path" and "capture exclude" and a raw TOML editor.

She'll find the model field and the run-step cap, because those read in plain
language. But two things quietly fail her. One, the cost panel that would tell
her *this* agent's model and limits is back on the setup page and reflects the
default agent, not the one she's editing — so the number she's trying to control
isn't next to the controls. Two, the trap above: she opens a package's settings,
nudges a number to make her weather agent chattier, and is surprised later when
her launch-scout behaves differently too, because that setting was shared and
nobody said so. To Lily that's not a feature, it's the software being sneaky.

What would make her happy: land her in conversation with the new agent, not the
cockpit; put this agent's cost and budget at the top of its own configure tab;
and when a control is shared across all her agents, say so on the control.


# Journey: Daniel resents being here at all

Daniel does not want to be in configuration. He came to put his coding agent
under Elanus, and config is the tax he pays to get there. He opens it for exactly
two reasons: cap the spend so this thing can't surprise him on a bill, and aim it
at his repo. That's it. Everything past those two is, to him, ceremony — and
ceremony is precisely the moment he decides Elanus isn't worth it and goes back
to running the coding agent bare.

So the test for Daniel is brutal and simple: are the two things he came for the
first two things he sees? Right now they're in there — model, run-step cap,
workdir — but they're peers with context programs, throttle tables, package
trees, and a raw file editor, none of which he asked about and all of which read
as "this is going to be a project." He doesn't read the whole pane and conclude
it's powerful; he reads the first screenful, sees plumbing, and concludes it's
not for him.

What would keep him: an essentials-first configure tab — name, model, spend cap,
where it works — with everything else folded under one honest "advanced" fold he
never has to open. And the spend cap framed as what it is, a hard ceiling, so the
one thing he actually trusts Elanus for is the thing the page leads with.


# Journey: Ganesh audits, he doesn't tune

Ganesh opens configuration to answer questions, not to change values. He's
standing at the instance level, and he's running down his list: what's installed,
what's actually running, what can write to the filesystem, what opens a port,
what a human approved, what changed since that approval, where the data lives,
and — the one he most wants — how do I turn a thing off.

The trust-and-footprint card and the risk badges are genuinely aimed at him, and
the copyable summary is exactly his instinct: give me one block I can paste into a
ticket. But two of his questions hit walls. "What changed since approval" has no
answer on the screen — the badges know "approved" and "pending," but not
"drifted." And "how do I turn it off" has no answer at all: he can see a
capability is installed and on, he can read its risk, and then there's nowhere to
remove it or even stop it. For the person whose entire job is bounding risk,
being able to see a danger but not switch it off is the worst possible state.

Autonomy is the other thing he reads as a risk dial, and it's presented as a bare
dropdown — off, manual, assisted, autonomous — with no statement of what each one
lets an agent do without a human in the loop. Ganesh can't sign off on a word; he
needs the consequence spelled out next to it. "Autonomous: this agent's own
settings changes take effect without asking you" is the sentence he's looking
for, and it isn't there.


# Journey: Tim checks the machinery is honest

I go to config for a different reason than the rest of them. I'm not tuning and
I'm not auditing — I'm checking that the model underneath is real and that the
interface is telling the truth about it. I want to open the configure tab and see
the profile, the package path resolving the way I think it resolves, the context
chain in the order it'll actually run, and the raw file right there when I want to
drop to it. The density doesn't bother me; that's the cockpit and I asked for it.

What bothers me is when the surface lies about the system's shape, even by
omission. The sharpest case is the one from the top of this doc: a package's
"settings" written from inside an agent are shared across all agents, while the
context-stage values an inch away are per-agent, and the UI dresses them
identically. That's the system being unclear about its own scope, and scope is
not a detail — it's the whole point of having an instance level and an agent
level. If I have to stop and reason about which store a control writes to, I built
the control wrong.

The other thing I'm watching for is whether the other three light up. The
configure tab is where Lily gets overwhelmed, Daniel decides it's a project, and
Ganesh can't find the off switch. Those aren't separate bugs — they're the same
missing idea, which is *altitude*: the page treats a person's first, simplest
intention and the deepest builder knob as equals. The instance-versus-agent
split is one altitude problem; essentials-versus-advanced is another. Get
altitude right and the same pane can serve all four of us without any of us
having to think hard about it.

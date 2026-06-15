# The three layers

Decided 2026-06-13. This describes the direction we are moving toward. It is
not yet how all of the code and interface are organized; the migration state
lives in HANDOFF.md.

elanus is built in three layers. Keeping them honest is what lets the same
system be both a small, hackable core and a product that an ordinary person
can use without ever meeting that core.

## The kernel

The kernel is deliberately small. Its job is to move messages between the
participants in the system, to record everything that happens, and to
enforce the safety rules (the sandbox and the approvals ledger). Whenever a
feature can live outside the kernel, it does. We are more conservative about
growing the kernel than about anything else.

## The building blocks: packages and kits

On top of the kernel sit packages and kits. A package is a unit of behavior
or capability. A kit is a bundle of packages and configuration that sets the
system up for some purpose. These are the building blocks, and they are
genuinely powerful and composable — but they are meant for people who are
building with elanus. Using them well assumes you understand approvals,
mailboxes, the way topics are named, and so on. They are not the surface an
ordinary person should have to touch.

## The product: the interface

The interface is a complete, streamlined product. The goal is that someone
can get real value from elanus without knowing that the kernel exists,
without knowing what a package or a kit is, and without learning any of the
internal vocabulary. The interface has its own language, chosen to be clear
to a newcomer, and it quietly translates what the person wants into the
kernel's mechanisms behind the scenes.

## The rule that keeps the layers honest

The internal layers have precise words — "approval", "package", "topic",
"correlation", and so on. Those words are correct, and we should keep using
them in the kernel and in anything aimed at builders. They must not appear in
the interface.

A simple test decides it: if a word only makes sense once you understand how
elanus works on the inside, it does not belong in the product interface. A
person adding something should see "Add" or "Install", not a two-step
"stage, then approve". A person granting a capability should see something
like "Allow this agent to send you messages", not a reference to publishing
on a particular topic.

This is easy to get wrong, because the internal words are right there and
they are accurate. The discipline is to translate them at the boundary every
single time.

## A consequence worth stating plainly: one action to add something

The interface now carries real authority. A person acting through it is a
genuine, trusted human gesture, with the same standing as someone typing
commands in a terminal. Because of that, the old two-step rhythm — first
stage a change, then separately approve it — no longer makes sense inside the
interface. When a person adds something through the interface, it should be a
single action that takes effect right away, ideally after showing them
plainly what they are about to allow, the way an app store shows you the
permissions an app wants before you tap "Get".

There is exactly one situation where a review step still belongs: when one of
the agents, rather than a person, proposes a change. Then a human does need
to see it and decide. The interface should present that as a plain request in
ordinary language — for example, "the scout agent would like permission to
send you messages" — and not as a queue of pending technical approvals.

## How adding and proposing actually work

This section states the principle; docs/config.md works it out in full. The
short version: configuration is kept as files under version control, a person's
or an agent's change is a proposal held aside until it is accepted, and
acceptance is the single action above. How much an agent may have accepted on
its behalf without asking is a comfort setting the person controls. And a small
set of packages the product itself depends on — the transcript view is the
first — live in a protected "stdlib" that is always present and refuses to be
removed without a fight, so the product never depends on something a person has
to discover and turn on.

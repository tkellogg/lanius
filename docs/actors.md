# Actors

Decided 2026-06-13. This describes the direction we are moving toward. It is
not yet how the code is structured — today the system still talks about
"agents", "profiles", and "packages" as separate things. The migration plan
and the questions that are still open live in HANDOFF.md.

## Everything is an actor

The fundamental unit in elanus is the actor. A human is an actor. An agent —
something driven by a language model — is an actor. A script is an actor. An
actor is simply a thing that takes part in the system.

We chose the word "actor" on purpose. It is a plain noun with no verb form,
which keeps it from turning into jargon: you never "actor" anything, you just
have actors. It also frees us from arguing about whether a particular thing
is "really" an agent or "really" a tool. That distinction does not matter. If
it takes part, it is an actor.

## What an actor does

The definition is small and complete: you send messages to an actor, and the
actor produces messages. That is the entire contract. Everything else —
whether it thinks with a language model, whether it runs a shell script,
whether it is a person reading their mail — is an implementation detail of a
particular actor.

## Actors are launched; the launcher is not the actor

An actor is the running, addressable thing. The things that bring actors to
life are not themselves actors. A package can launch an actor — its
long-running background process, or a fresh run started for each message it
receives. The command line can launch an actor too: a program you start that
connects to the bus is simply an actor brought up from the command line
instead of by the kernel's own run machinery — an external actor, no
different in standing from one the kernel started. Either way, the actor is
the participant; the package or the command that started it is a launcher.

Every actor has its own inbox, even if it never reads from it. A pure
producer that only ever emits messages still has an address and an identity.
Having an inbox is what makes something a first-class participant you can
name, reach, and hold accountable, rather than an anonymous source of
traffic. This matters for identity (see docs/identity.md): because every
actor is addressable, every actor is also something the system can know the
sender of.

## What is not an actor

Everything that takes part is an actor, but the kernel's own machinery is
not. The dispatcher that schedules and supervises work, the safety hooks
that can veto a tool call, the file leases, the flight recorder that writes
down what happened, and the individual steps of the context pipeline are not
actors. They are the stage the actors perform on, not participants with
mailboxes.

The line is the definition itself: an actor is something you address and
send messages to. If you would never send it a message — if it is part of
how messages get moved, recorded, or guarded rather than something that
receives and answers them — it is kernel machinery, and it should stay that
way. Resisting the urge to turn every internal coordination surface into an
actor is part of what keeps the kernel small.

## Default actor implementations

If the only idea were "actor", the system would be hard to reason about: a
flat, featureless field of message handlers. We solve that by shipping a
small set of recognizable default implementations, so that most of the time
you are working with something familiar instead of assembling an actor from
nothing. The defaults we expect to provide:

- An **agent**: an actor that thinks using a language model.
- A **human**: an actor that is a person, reachable through one or more
  communication mechanisms — the web interface, the command line, and
  potentially others such as email or chat.
- A **reacting script**: an actor that runs a script in response to each
  message it receives.
- A **polling script**: an actor that runs on a schedule, or watches
  something, and produces messages when it notices a change.

These are starting points, not a closed list. The interface is free to call
the language-model ones "agents", because that is the word people expect,
while the kernel keeps the more honest word "actor".

## How much thinking: zero or one language model

An actor uses either no language model, or exactly one. There is no actor
that uses two.

If you find yourself wanting an actor that "sometimes" uses a language model,
the answer is not a half-wired brain — it is two actors working together. We
already do this. In the firehose example, a cheap script actor filters
incoming items with plain pattern matching, and only the survivors are handed
to a second actor that thinks with a model. "Sometimes think" is two actors
in a chain, not one actor with an optional mind. The same reasoning is why we
do not allow two models in a single actor: if you need two, you have two
actors.

This keeps a useful promise honest. When the kernel is the one managing the
model an actor uses, the kernel can see that use and account for it,
including, eventually, its cost. An actor is still free to call a model on
its own from inside a script, but the kernel cannot see or cost that, so it
is the script's private business rather than a managed part of the actor.

## Where an actor's model comes from: providers

When an actor does think with a model, that model comes from an inference
provider. We treat providers as first-class things in their own right,
instead of scattering a web address and an API key across each actor's
configuration.

A provider describes:

- where it lives and how to authenticate to it;
- which models it offers — discovered live by asking the provider wherever
  that is possible, rather than written down by hand;
- which network protocols it speaks. Some providers speak more than one. For
  example, a single provider might offer both an OpenAI-style interface and
  an Anthropic-style one. When there is a choice, elanus prefers the richest
  protocol the provider supports, because the richer protocols give us better
  tool use, reasoning, and caching. (Our model library already understands
  the Anthropic interface, the OpenAI chat interface, and the newer OpenAI
  "responses" interface, so this choice is real and available today.)

### Pricing is a separate, living thing

Knowing what a model costs is valuable but awkward, because most providers do
not report their prices through their interface. So pricing is handled as its
own module that improves over time and lives outside the core. It
periodically fetches pricing tables from wherever they can be found and fills
in what the providers themselves do not tell us. Where a provider does report
prices directly — some aggregators do — we use that. Over the longer term
this may be backed by a small public, regularly updated pricing dataset
published separately, so that every elanus installation benefits from the
same maintained information. We are explicitly not trying to get this perfect
up front; "price unknown" is a perfectly acceptable answer until the data
fills in.

## Reaching an actor: channels, and which way they flow

An actor — a person especially, but agents too — is reachable through more than
one mechanism: the elanus interface, the command line, email, a chat network.
Each such mechanism is a *channel* of that actor's identity; the full model of
identities, channels, and names lives in docs/identity.md. What belongs here is
how messages on those channels move, because ingress and egress are not
symmetric, and pretending they are would distort the design.

**Ingress is event-shaped.** Messages arrive unbidden, fan out to whoever is
listening, and the flight recorder wants to capture them. That is what the bus
is for, so external messages come *in* over the bus, through small ingress
bridges (the existing shape: a daemon that watches a source and publishes what
it sees). They are addressed by the channel they actually arrived on —
`in/dm/bluesky/<handle>`, not `in/dm/tim` — because the topic should record
what is true on the wire; deciding *who* that handle is belongs to the
phonebook, resolved later, not frozen into the topic now.

**Egress is command-shaped.** Sending a message out is a specific action with a
result — it succeeded, here is the id, or it failed — which is what HTTP and
service SDKs are built around and what fire-and-forget publish/subscribe is
not. So egress goes *direct*: the actor, or a small bridge that holds the
service's credential so no agent has to, calls the outside service over HTTP or
its SDK. It is not forced back through the bus as transport. What we keep is the
*record*: the send still emits an observation on the bus — "sent this to that
channel, here is the result" — so the flight recorder and the provenance trail
stay whole even though the delivery itself left the bus. The bus remains the
universal record plane even where it is not the transport plane.

This asymmetry is also why there is no `out/` plane (it was proposed and
deliberately dropped). Sending to another elanus actor is just writing to that
actor's inbox; sending to the outside world is a direct command that gets
observed. Neither one needs an outbox.

Two shipped exemplars make the egress shape concrete; both follow the same
principle — a direct send, then an `obs/channel/<kind>/sent` record, nothing
relayed through the bus as transport — but they are built differently, and the
difference matters for provenance. The `notify` package is a reacting exec
handler: it receives an ask and fires a desktop notification via the OS,
directly. The `webhook` package is a **daemon bridge**: a request arrives
addressed to its inbox (`in/package/webhook/send`), it POSTs directly to the
URL, and it emits its record. The daemon shape is the one to copy for a real
external-channel bridge, because a daemon carries its own identity (its package
token), so the broker stamps its record's sender as the bridge itself —
genuine provenance. An exec handler runs uncaged and tokenless and so
authenticates as the owner, mislabeling its sends (docs/security.md entry 16);
that is a containment gap to close, not a pattern to imitate. For a real
channel, replace the POST with the service's own API or SDK (whose credentials
the bridge holds, so no agent has to), keep the `obs/channel/<kind>/sent`
record, and run it as a daemon.

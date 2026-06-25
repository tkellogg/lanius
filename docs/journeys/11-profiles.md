---
name: Profiles
description: How users think about profiles
status: barely written
---

# Profiles
A profile is just a set of packages and kits all configured.

I could see this going down a few ways.

1. An open-strix type agent with a personality has a profile that's uniquely theirs, **is them**
2. An engineer might have a set of profiles for various coding jobs. Like maybe one per project. Where each has
   a set of memory blocks, maybe a computed memory block for top 5 issues assigned to the agent or human.
3. Agents cooperating within the elanus system might have a profile with configured memory blocks just for
   being aware of what else is going on within the system, what other agents are doing.

## Inter-agent communication
Every agent has an inbox and a way to send messages to other agents. But what good is that if they don't have
cooresponding hooks to make that valuable? I'm thinking a computed memory block that just says "(1) unread message 
from agent Y" and then they can call a tool to retrieve the actual message. 

Also, priority. Where do we inject these messages? I'm not sure what's possible or good. But the places I can 
think of are:

1. memory block (prepended to regular user messages)
2. injected memory block mid-cycle
3. appended to a tool call, like literally just concatenate some text
4. new human message (not possible with many harnesses)

I imagine the fight between 2-3 is really just the model's preferences, maybe the harness we're on. But 1 vs 2-3
is all priority. Do we need to intervene now or later? So I guess messages need to have priorities attached, with
the highest also injecting the full message not just a subject/summary.

So the thing that's handling inter-agent comms is a context program that's checking MQTT inboxes and injecting
it into the LLM conversation. Also, seems like for this particular case, you'd likely want to setup a shared channel
that a bunch of agents all agree on. Like for coding agents, I'm thinking a topic specific to a certain git repo.
Or maybe specific to the root worktree, idk, if it makes sense.


## Memory blocks
The default kind of memory block is useful for learned prompts. So if you want the agent to customize their own 
behavior, memory blocks are it. Unlike skills, blocks can't be ignored. So they're very good for behavior,
identity, and contextual info (e.g. inter-agent comms)

The key is to have a set-memory-block tool. I like putting all prompts into blocks. So even if there's a default
value, it evolves over time. So, e.g., the prompting telling the agent how to communicate with other agents,
that would be default prompting that goes into a memory block and the agent is encouraged to add to it to 
customize to it's own environment.

Ideally memory blocks would feel like they're built-in, so that other arbitrary profiles can include them easily.
Also, the context program pipeline should probably represent them in the data model, like just key-value pairs,
then the harness has block->text rendering built in. So a computed memory block is a pretty vanilla context program
that adds a block. Then a later context program simply sees memory blocks, with no distinction between computed and
not.

## Profiles
Yeah, so back to this, we need one really great way that elanus is better than stock claude code, or codex.
Technical awe may last a week or two, but ultimately it has to *actually* be better.

Inter-agent comms seems like an obvious-enough win. Also, bolting on agent patterns, like dynamic workflows or RLMs,
might be another thing that helps sometimes. Ultimately, I want to get to a point where we can have an agent start
estimating work before it embarks.

Another idea I've floated to several people — Fable is a stunning model, but it's quite expensive. The thing is,
you get most of the benefit from just using it to plan. I sort of said that here: https://timkellogg.me/blog/2026/03/29/mythos-ceo
So I think there's a lot of workflows we need to experiment with. But a lot of it doesn't really get off the ground
without proper analysis and observability into what's going on in the session.

## Estimating work
To estimate work, I'm imagining that an MQTT listener is watching traffic on certain agents. An agent, right after
it's got the plan figured out, it provides an estimate for the work. Then from that moment onwards it all counts 
against that estimate. So the process comes by later, does a retro on why it missed the mark, and then adjusts memory
blocks for a better future estimate.


## Additive
The part that makes all this tick is that it's all merely capabilities you can bolt onto an agent. Skills are easy
because they're stateless. Memory blocks get more interesting, especially when many agents can edit them. And then
estimating work is simply not something you can do only with a skill, too much state.

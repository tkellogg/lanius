---
name: Agentic Configuration
description: How someone sets up the helper agent, and how it helps
---

# The helper agent
I've had phenomenal success at work with this concept. Basically, an agent that has access to everything the UI
has access to. Maybe it makes mutations carefully, but reads are transparent. For complex UIs, it's a far superior
experience.a

## Activation
I say there's a side panel that pops out on the right, triggered by a "AI" button (the typical icon, maybe a 
magic wand, or maybe a chat bubble). 

Alternatively, the "setup" tab defaults to a chat view, full screen, and the current UI becomes a sub-tab that
you can click to navigate that way.

## Agent Charter
The agent's goal is to get you setup. Once the platform is setup, the agent's goal shifts to merely helping.
So there's a task list (a memory block), and a status of done/not done (another block).

As everything in Lanius, memory blocks are system prompts, so that's how we ship the charter. We also ship a KB
all about Lanius, and then also the sys prompt memory blocks refer to KB pages as needed. Probably ship the KB
separate from the memory blocks.

{{oh shit, we need package dependencies, we have not built that in yet, oof, that's a whole thing. Let's just avoid
versioning for now to simplify. It might not be necessary in the end}}

So now, with the block pointers to KB pages, the agent is now aware of
1. it's goal
2. it's progress toward it's goal
3. how to get more information and clarify sub-goals

## Grow a KB
As part of this, we also start a new KB dedicated to learning more about the user and what they want to do with
Lanius. As well as their specific Lanius setup. Not just what packages are setup, that's on disk already as config,
but why they're trying to setup packages that way.

# How to setup the agent
Well, this whole thing doesn't work without an LLM setup. So we need some basic UI in order to acquire LLM time 
from *somewhere*.

Obviously the API key is a good idea. But we should also attempt to be able to run this agent via headless
calls to `lanius code {claude|codex|opencode} ...` if that's what they have available. Is that possible? If so,
let's figure it out. Basically, I want this to be as seamless as possible. And I don't want to cause an "oh shit"
moment if they don't want to do API billing.

But API billing is also a thing. Suggest a few providers that are good options to setup accounts with. Fireworks,
openrouter, DeepSeek, Z.ai, etc. Places you can get something cheap.

Please do your best to flag underpowered models. Like if they select a 4B, it might not work well.

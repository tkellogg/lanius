---
name: How users chat with agents
description: A tour around general usage from a UI perspective. Assuming the agent is already setup.
---

# Never ending chat
Open-strix pioneered the idea of killing the cache on every message and then banking on having the agent
improve it's abilities to remember better. Weirdly, this ends up being cheaper in practice, because the 
average context is less than 50K tokens, whereas a 1M max context tends to sit around 400K-500K when maintaining
the cache. The way to kill the cache is basically to just take a sliding window over message history. The way
to remember better is basically a strong system prompt, then utilize memory blocks to guide the agent into
knowing what's important, what it'll need to know in the future.

One optimization that Tim has found is to use the UI to continue very specific conversations. Kind of like 
replying in a thread in Slack. Tim found that if the agent can respond in HTML (optionally, when it wants to),
it can provide UI elements and forms that continue the conversation without losing context. This still keeps
conversations short, but means the conversation doesn't have to be rebuilt.

Something Tim has been considering but hasn't built — what if elanus had some (optional) component that would
select which conversation to continue (or start fresh). That way you could have the UX of a never-ending conversation,
but with the near-optimal cache locality. If we do this, it should not be core. Fine if it feels core in the UI
though.

## Ambiance vs Chat
One reason for Tim's architecture is that with ambient agents that respond to events, you need a through-line
across everything the agent is doing, otherwise the agent doesn't *feel* like a single entity, it feels like
it's right hand doesn't know what it's left hand is doing. So the sliding window gives continuity, whereas
the heavy remembering processes help keep depth.


# Claude Code / Codex
A big use case for elanus is likely using Claude Code normally, but being able to subagent to Codex, or
subagent from Opus on Claude Code to GLM-5.2 on Claude Code. Daniel and Tim are likely the main target
users here. Lily likely won't find this arrangement as empowering.

When using Claude Code or Codex inside of Elanus, it's crucial that it feels just like the original, as much
as possible. Elanus is going to have to add tools and hooks to make it all work right, but that all should
remain as transparent as possible. Cursor will likely also be a target use also, if that's even possible.
When someone chooses to go this route, they're doing so because it makes sense to them. Because they like
it, we need to retain whatever it is they like about it.

Another big part of using coding agents is knowing how to get started. It likely won't be obvious, so we'll
have to give them a lot of help. Worse, many people prefer to not use the terminal for Claude Code, so they'll 
use a plugin in VS Code. Not sure how we'll address this.


# Ambient interaction
A big use may likely end up being just hooking it up to Jira or Github Issues, so that the agent automatically
starts handling tickets where they have interest or are being directly pinged. 

This likely needs to feel similar to Tim's "threaded conversation", Slack-style, that we talked about above. 
Where one thread at a time corresponds to one agent context. It's also important here to maintain context across
all threads such that one part of the agent knows what all other parts of the agent are currently doing.

When using agents ambiantly, the elanus UI probably won't be used by anyone except maybe whoever is administering
the agent. When administering, they'll likely only go in to change the agent configuration, or adjust guardrails.

I anticipate that Lily will be the biggest user of this case. She'll be hooking elanus up to all the systems
that marketing departments use to collaborate. Tim will also do something similar for engineering teams. Daniel
will likely only be a consumer (non-admin) of such a setup. Ganesh could also be interested, but only for the
purpose of enabling people like Lily & Tim to be admins yet also aligned to his own policy goals.


# Dashboards
Custom UI feels weirdly powerful. When Tim was working with open-strix, adding custom dashboards that the agent
can create on demand provided an absurd amount of visibility into what the agent is doing. Having elanus on MQTT
enables a lot more takes on this that weren't previously possible. The key, though, is to not prescribe what sorts
of UI should be created, and simply enable anything to be easily done (vibe coded, by the agent, on demand).

One utility is to have all the dashboards discoverable. It's nice to not have to rebuild work. But also, its
useful to be able to rebuild whenever a slightly better idea comes up.


# What chatting should feel like
Whatever Tim, Lily, or Daniel are doing, the thing they are doing is *having a
conversation with their agent*. That should be the most obvious object on the
screen, and it should behave the way every chat app has taught them to expect: you
say something, the agent answers in the same place, and you can come back tomorrow
and pick the thread back up. Lily in particular treats her agent like a companion —
if "talking to it" feels like filing tickets into a void she can't reply to, the
whole relationship breaks.

So the unit of the UI is a **conversation**: one ongoing thread with the agent,
the way you'd think of a Slack DM or a thread. Each thread is its own context
(one sliding window; see "Never ending chat"), but to the person it's just "the
conversation I'm having." It carries a human label — what it's about — not an
opaque id. Coming back later resumes it. Starting fresh is an explicit, deliberate
choice ("new conversation"), not something that silently happens every time you
reload the page. And when the agent acts on its own — kicked off by a GitHub
issue, a timer, an inbound event — that shows up as *another conversation you can
step into and reply to*, not a notification you can only watch. Daniel wants to
understand why his agent did something; the answer is to open that thread and ask
it, in the same box he'd use for anything else.

The other thing on the screen should know its place. When Tim drives Claude Code
or Codex under elanus, the worker runs those spawn are not conversations — they're
*work in progress he observes*. He wants to see them (which model, how long, did
it finish), but he is not chatting with `code-7f3a…`. Mixing a stack of those into
the same list as his real conversations is what makes the screen feel like noise.
Coding runs belong in their own surface, quiet by default, expanded when he's
actually watching the work — see
[../handoffs/coding-agent-observability.md](../handoffs/coding-agent-observability.md).

The deeper principle underneath both is the one in
[../layering.md](../layering.md): the product speaks the user's language, not the
kernel's. "Session" is an internal word; nobody chatting with their agent should
ever have to see it, or a raw id, to know what they're looking at. The work plan
for getting there is [../handoffs/chat-conversations.md](../handoffs/chat-conversations.md).

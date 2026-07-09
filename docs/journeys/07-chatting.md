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

# Dead air is the one unforgivable failure

Three separate times in the 2026-07-08 walkthrough, Tim typed a message and got
*nothing*: no typing indicator, no response, no error, no recourse. Once to the
main agent, once to the helper, once again to the main agent at the end. Each
time the same triple absence:

1. **No acknowledgment** — nothing confirmed the message even left the browser.
   A chat surface needs an immediate, local "sent" state and then a live
   "agent is thinking" indicator (typing dots, a spinner on the message, a
   status line — pick one). Silence between send and reply is where trust dies.
2. **No failure surfaced** — if the daemon is down, the provider rejected the
   call, or nothing is subscribed to the topic, *say so in the thread*, where
   the person is looking. A message that sails into the void is
   indistinguishable from a broken product. (Compare the failure-mail contract
   for runs: failed runs mail the human. The chat pane needs the same honesty.)
3. **No recourse** — even knowing it failed, there was nothing to *do*: no
   retry, no "check agent status" link, no pointer at the log line that
   explains it. Every dead-end error needs one next step attached.

Lily interprets dead air as her companion ignoring her. Daniel interprets it as
the product being broken and leaves. Tim interprets it as a debugging session he
didn't ask for. Nobody interprets it charitably.

A related expectation from the same walkthrough: the empty state should invite,
not just describe. "The agent hasn't sent any messages" is a shrug; the empty
Converse pane should be the *strongest* prompt to say hello, and saying hello
must visibly work (see 1–3 above).

## Talking to a coding session

When Tim saw his Claude Code worker appear (delightfully fast, to his surprise —
that moment should be protected), his very next instinct was: *can I send it a
message and see if it responds?* The current model treats coding sessions as
work-you-observe, not conversations (see above), and the DM plumbing filters
`code-*` sessions out. The walkthrough says the instinct to poke a live worker
is real anyway. That doesn't mean coding runs become peer conversations — but
there should be a sanctioned "say something to this worker" affordance
(lanius already has `deliver`; the UI should expose it on the worker's surface),
and the observe-vs-converse distinction should be legible rather than a
silent wall.

Learned the hard way (2026-07-09, first live use): the affordance must live
**where the instinct goes**, which is the worker's own panel — the thing you
land on when you click the worker in the agent list — not only on a separate
runs surface. Tim found the runs-detail compose only after asking; "shipped
the feature, missed the doorway." And a sent note must be *visible after
sending*, on that same panel, surviving a reload — a note that vanishes into
an inbox with no echo reads as a failed send even when delivery worked.

The deeper principle underneath both is the one in
[../layering.md](../layering.md): the product speaks the user's language, not the
kernel's. "Session" is an internal word; nobody chatting with their agent should
ever have to see it, or a raw id, to know what they're looking at. The work plan
for getting there is [../handoffs/chat-conversations.md](../handoffs/chat-conversations.md).

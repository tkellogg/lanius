---
name: Chat from anywhere
description: >-
  Tim's aspirational journey: talk to my running elanus agents from my phone
  while I'm away from the machine, on the same conversation the web UI shows —
  not a new app, not a new inbox, the same one.
last-updated: 2026-07-13
---

# Chat from anywhere

*First-person, aspirational. Not a plan — the thing a later agent reads to decide
whether the Telegram work actually did what I wanted.*

I'm out. Coffee shop, walking the dog, sitting in a meeting I can't leave. My
laptop is at home, lid down but the daemon is up — the agents are still running,
still working the queue, still occasionally needing me for a call they can't make
on their own. Right now that need just... waits. If an agent wants to ask me
whether to force-push, or wants to tell me the release built green, it emits onto
`in/human/tim` and I find out whenever I next open the web UI. Which is when I'm
back at the machine. The whole point of having agents that run without me is
undercut by my having to be *at the desk* to talk to them.

I want to pull out my phone and type. In Telegram — an app I already have, already
trust, already get notifications from. I want to say "yeah, ship it" and have the
agent that asked me *actually receive that as my answer* and keep going. I want it
to feel like texting a colleague who happens to be a program.

## What "it worked" feels like

**It's the same conversation.** This is the part I care about most. When I'm at my
desk I talk to my agent in the web CONVERSE pane. When I'm on my phone I talk to it
in Telegram. It is not two agents, not two threads, not two memories that later
have to be reconciled. It's *one* conversation that I happened to touch from two
places. If the agent asked me something in the browser this morning and I answer it
from Telegram this afternoon, the answer lands on the same question — same
correlation, same thread. When I get home and open the web UI, the Telegram
exchange is *right there* in the conversation list, labeled as having come from
Telegram, continuous with everything else. Nothing to merge. Nothing lost.

**It reaches me, and I reach back.** An agent decides it needs me — an `ask_human`,
a heads-up, a failure — and my phone buzzes. I read it on the lock screen. I reply
in the thread. The reply goes *back into the agent's turn* — it's not a note that
sits in a queue hoping someone reads it; it's the answer the agent was parked
waiting for, and the agent wakes up and continues. The round trip closes. Me →
phone → agent → work → agent → phone → me. Seamless enough that I stop thinking
about the plumbing.

**It knows it's me.** When a message arrives on Telegram, elanus knows that
particular chat is *me*, Tim, the owner — not because the message says so (anyone
can type "I'm Tim") but because I told the system once, deliberately, that this
Telegram chat is mine. So my message carries the weight of an owner's word: the
agent treats it as me, recall pulls in my history across channels, and it acts on
my say-so. And crucially: a *stranger* who finds the bot and messages it does
**not** get to be me. They don't get owner authority. Ideally they don't get an
agent's attention at all until I've vouched for them. The bot being reachable from
the whole internet must not mean the whole internet can drive my agents.

**The secret stays secret.** The bot's token — the thing that *is* the bot, that
anyone holding it can impersonate — is not sitting in a plaintext file on disk
next to the code. It's held the way my model-provider keys are held: encrypted,
handed to the bridge only in memory, never printed, never committed. If someone
reads my config repo they don't walk away with the ability to be my bot.

## What I do NOT want

- **I don't want a new inbox.** If "Telegram messages" become their own separate
  list I have to check, I've made my life worse, not better. It has to fold into
  the conversation I already have.
- **I don't want to babysit an endpoint.** My laptop is behind my home router.
  I'm not opening a port, not running a tunnel, not renting a server with a public
  HTTPS cert just so a chat app can reach me. If it needs any of that to work at
  all, it's too heavy for "I just want to text my agent."
- **I don't want to pick the platform every time.** When my agent replies to
  something I said *on Telegram*, it should go back *to Telegram* — obviously,
  automatically, because that's where the conversation is. I shouldn't have to
  teach it "this one goes to Telegram." (Whether an agent can *start* an unprompted
  conversation and choose which of my platforms to reach me on — that's a fancier
  thing, and I can live without it for now. Replying where I already am is the
  floor.)
- **I don't want a fragile toy.** If the network hiccups, if I send two messages
  fast, if the daemon restarts mid-conversation — it should just keep working. Not
  double-send, not drop my message, not crash-loop.

## The one-line test

I'm on a train. My agent asks me, via Telegram, whether to merge the branch. I
thumb back "merge it." The agent merges, tells me it's done — on Telegram. I get
home, open the laptop, and the whole exchange is sitting in my conversation list
like I'd been at the desk the entire time. That's the win.

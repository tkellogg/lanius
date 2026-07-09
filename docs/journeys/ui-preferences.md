---
name: UI Preferences & Perspectives
description: A careful treatment of how each user thinks about UI elements and navigates UI
---

# Options
When it comes time to UI elements, a text box is almost always the worst choice. E.g. In the model selector,
it seems like it should be an open set of choices, but it's not actually. Most inference providers have an API
for selecting models, and you should use it. Or for finding a files, that's a closed set, and there's UI controls
for it!

Some users, like Tim, may want a way to override the given options to do something strange. But even Tim will
almost always prefer the help provided by UI options.


# Agents
Should agents be used to configure elanus? Most users, except maybe Ganesh, will generally want to allow an agent
help them configure. Of the 3 remaining personals, Daniel will be hesitant but eventually warm up to it, whereas
Lily & Tim will dive right in. In fact, Lily & Tim will get annoyed if they **can't** allow an agent to configure
elanus for them.

The way Tim thinks about it, as long as there's a proper change log, that's all the safety he needs. In other words,
it's not about restricting change for Tim, it's about explaining when and why changes happened. They can be 
completely automagical, that's better even, as long as retroactive analysis can explain why it happened.

Daniel is different. He actually does want to maintain tight control how Elanus is configured and operates. He'll
eventually warm to the idea of letting an agent configure it for him, but until he's comfortable he'll want the
agent to carefully control and approve changes.

Ganesh's perspective is strictest. On one level, he's likely to experiment with autonomy, but ultimately he's
most interested in how he can establish policies across the board so all the people he's responsible for in the 
company can safely use AI. Like Tim, he wants strong audit abilities, but also is interested in making policy-based
decisions about how settings are evolved.


## Autonomy
I think this has probably been mostly addressed above. In general, all users except for maybe Ganesh want the
UI to be able to be fully operated by the agent. The users differ only in the guardrails they want to enact
on the agent.


# Navigation
Pages are places. When Tim changes pages, he expects the URL to change — that's
how he knows where he is, how he deep-links to the exact pane he's looking at,
and how the back button works. A SPA that swallows navigation into invisible
internal state reads as broken to every character, even Lily, because every site
they use daily behaves the other way. (First thing Tim checked in the 2026-07-08
walkthrough: "I'm expecting that when I change pages, the URL changes.")

The chrome should speak one language. If every other navigation affordance is a
word, settings should be a word too — not a tiny gear icon that Tim, the person
who built the thing, almost missed. Icon-only controls are for actions performed
fifty times a day, not for a page visited twice a month.

Internal vocabulary must not surface. "Instance" is a doc-and-kernel word (see
[06-configuration.md](06-configuration.md)); Tim hit it cold in the UI and his
reaction was "what's the word 'instance' doing here?" — and he *wrote* the docs
that use it. If the author trips on a word, Daniel bounces off it entirely.


# Log-like surfaces
The Activity pane currently reads like `tail -f` — one JSON object per line,
overflowing off the right edge. Tim's expectation: structure it like a UI, not a
log file. Each event is a row that can be *expanded* — one legible summary line
collapsed by default, click to unfold the JSON. The raw firehose is fine as an
escape hatch (Tim will want it), but it is not a resting state, and it is
definitely not a good **default page** for an agent. The default page should be
the conversation (see [07-chatting.md](07-chatting.md)); activity is where you
go when asking "what has it been doing," not the first thing you're shown.


# Colors
Tim doesn't care much about color schemes as long as they're functional. If there's enough contrast to see
text, he's good. Daniel similarly doesn't care much, although he thinks dark themes are cool because that's
what hackers use, allegedly. Also, Daniel is attracted to pre-boxed products, so if something looks like a
weekend hack-job he loses interest. "Professional" appeals to Daniel, less because he's using it professionally,
more just because he doesn't want to waste time on janky apps and the vibe emanating from the color scheme 
often tells him what he wants to know.

Lily is probably the hardest to serve here. She works around marketing people day after day, and it's very
difficult for her to overlook things like color, even with her background. Aesthetics are crucial for Lily.

One concrete rule from the 2026-07-08 walkthrough: **contrast has a hierarchy,
and text is at the top.** In dark mode the buttons currently pop harder than the
words — the eye goes to the chrome instead of the content. Buttons should sit a
step *below* body text in contrast, not above it. If a control outshines the
message the agent just sent, the palette is upside down.



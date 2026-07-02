---
name: "Timers & scripts"
description: "A bit of theory, from Tim's perspective on how messages should flow"
---

# Self-sending messages
While working with open-strix, one invaluable tool was allowing the agent to send messages to
itself. What it looks like in practice is something like:

1. I say, "remind me when this finishes"
2. Agent sends off a command and adds "; curl" to send a message to *itself* to analyze how the event turned out.

So yeah, in that case the message did indeed appear as coming from me, in open-strix anyway. Here, we can
do better. We attach a `sender` field, so when the agent wakes back up to handle the message, they're aware of
the full context, including how they got to that point. This tracing is key to having a true multi-agent system.

Because agents really are just actors responding to messages, they should also be able to respond to messages
from themselves. Allowing this enables a lot of patterns that transcend time in a way that agents aren't typically
able to process time. For more examples, just think about how actors in Akka & Erlang OTP can send messages to
themselves and what patterns those enables.


# Scripts are Ashby-pilled
Often, the human user has a desire, like "log into Bluesky and check my messages and reply to anything interesting".
They'll do that every 5 min, but if it's hitting the LLM every 5 minutes it accrues *MASSIVE* costs. And besides,
it's not really Ashby-pilled. Well, technically it is because the LLM's variety exceeds the variety of the
problem of checking bluesky messages. But it *vastly* exceeds. Scripts are a better match, variety to variety.


# Tell the agent
Across the board, not just when to use scripts, but across the board all features of Elanus needs to be explained
to the agent through memory blocks and skills.

* memory blocks are high availability. Consider them "prompt customization" or "learned system prompts". Use 
   them sparingly. But when you need them you need them.
* for skills, the agent's awareness of their existence is highly available, but the actual content is not. They're
   expando-prompts, so you can get extremely detailed as long as you structure it in a way that it unfolds 
   file-to-file.
* elanus CLI help messages — also an option. Good for when the agent already knows they need to use the elanus
   CLI but doesn't yet know how.
* knowledge bases — we haven't added this yet as a 1st class object, but a searchable KB can be better than skills,
   sometimes. Likely a good approach might be to have a knowledge base, and then use memory blocks on certain agents
   to bring certain concepts into high awareness by providing file + line references.

So every feature that an agent can use must be surfaced and explained to the agent.


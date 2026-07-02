---
name: Questions
description: This is stuff from Tim. All sessions are busy, things I want to chase down but can't do it now
---

Let's say your an agent, either coding agent or native elanus agent. Can you launch elanus agents? Can you launch
native elanus agents? Can you launch one with a certain package? Can you introspect available packages & profiles
to launch various kinds of agents that maybe havent' been launched before? (side question: Can you dynamically
configure?) I want all these things to be possible, maybe even easy.


In UI have the screen automatically follow the latest message by default, i.e. scroll to the bottom unless the
user intentionally scrolls away. Like a normal chat app.


Routing — in the web UI everything really should have it's own route, so that forward-back buttons work as you'd
expect. Right now it's all one giant app, lol.


model config error: "provider list unavailable — type a model id or set an API key". I should just have a link to
click to get me to where model providers are setup.


I'd really like to be able to set a model provider on a subagent. Like this could get crazy. Use Claude Code using 
the Claude.AI login, and have that launch a subagent that uses DeepSeek's ANTHROPIC_URL and API key via environment
variables. That should 100% be possible, it's just annoying af rn.


Claude Code session ID 6b197197-21ef-4f2f-a502-83405bdeb580 said that it saw a bunch of changes that another 
agent did. But I wasn't running another agent. I'm pretty convinced it was it's own subagent or some shit. We
should brainstorm how to detect file changes, and then what kinds of memory blocks and skills we would tack
on to the agent in order to make it more aware of what other agents are doing. Also, give it confidence to 
reach out to the other agent (if possible) to see what it was doing. Alternatively, if it was a coding agent,
we could launch off a regular elanus agent (or another coding agent) to read the old session file, figure out
what it was doing with particular files, and explain; it wouldn't be able to change course, but it could at least
explain the intent.


new failure mode: If the coding agent can't contact teh MQTT broker it dies. This is not a good failure mode.
Something softer??


Each coding agent's native MCP servers seem to not be able to load on launch. At least claude code & codex.


When I start a new codex or claude code session through elanus, it starts running one of my prompts from some
previous session, i think. Maybe it's related to QoS 1?? it's a strange behavior when in TUI mode.


I think when a subagent is invoked cross-coding harness, the parent harnesses have issues handling them properly.
I'm pretty sure if codex subshells to opencode, a dead opencode won't notify codex. I'm more confident in claude
code handling the death cycle better. Ideally, messages would arrive any time and the harness would just wake up
and handle them. I think with codex, there's a way for a subshell termination to cause the main agent to resume
processing (it's a weird UX interaction tbqh). We need to do this for all coding agents, figure out how they can
receive messages when dead and resume. Also, need to update the docs around what it takes to impl a new coding 
agent.


Realistically, what are the scaling bottlenecks? Like, if we had 100 agents all trying to modify the same memory
blocks, does that make memory blocks a scaling bottleneck? What would that look like?


Dolt might be worth considering. It's got Git-style history built-in, which might make it a much better option than
SQLite https://github.com/dolthub/dolt


Tests — do we have bullshit tests? Like those tests that only look at the text of the code rather than the 
functionality. Or tests with dumb asserts. Or tests that use too many mocks, etc.


I added the concept of a knowledge base in journey 14; we should also add an OOTB agent process type for 
consolidation. Like checking that links make sense, missing links, conflicting info, etc. And then use some cheap
tier of LLM.


Have an LLM knowledge base. Basically, go use benchmarks & online anecdotes to seed ideas for how certain LLMs
should be used. Different ones have different strengths. But it needs to be a mutable KB because the user probably
has a lot of preferences, and those preferences drive most of what they think.

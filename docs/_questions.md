---
name: Questions
description: This is stuff from Tim. All sessions are busy, things I want to chase down but can't do it now
---

How does it work when we have a list of 5 kits and 10 skills and we wire it into claude code? codex? ...


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


UI nitpicks:
- Advanced bar should highlight on mouseover, some indication that the whole thing is one button, not separate
    buttons for each. (configure pane)
- context steps: I think this needs to be a more visual view, like blocks. Being a visual view, prefer drag-n-drop
    over up/down arrows. Have it be a very visual walk through of what things are. Then with the "new" part, have
    it be a single button, just "New" or "+" that opens up a modal wizard. The wizard takes an existing configured 
    LLM and layers on it's own set of packages for modifying the context. Probably a profile that's system 
    configured and hidden by default. Dramatically scoped down permissions to just doing that one thing. Also, if
    existing packages have context programs, offer those as options to enable, as an alternative to the agent.




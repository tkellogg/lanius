---
name: Questions
description: This is stuff from Tim. All sessions are busy, things I want to chase down but can't do it now
---

Detecting files read — we said we couldn't detect when files are read in order to send MQTT events. But what if
we configured the sandbox to disallow all files, fail, then immedietely change the settings to allow, then try again,
so it's transparent to the agent, but we catch the first access so we can send an MQTT event (and still reference
when it's used). If you just do it on first access it feels like it might not be too slow. Not sure if you can
actually do this though, i'm not sure we're actually in the path to catching these OS errors.


Rust + web packaging — I think the current `cargo install --path .` ends up using web files from ~/code/elanus, 
which won't work for anyone who hasn't cloned. Really, we need this to work with `cargo install elanus` (i'll
probably change the name btw). IIRC the UI can probably be served by only a Rust backend, no node.js which would
be ideal, to keep dependencies light.


If I run `elanus code claude --resume`, it seems to pass on arbitrary args (wonderful!) but does it register the 
session properly? Since we're resuming an old one... really curious if this works properly.


I think the agents are bumping into each other. I have to keep explaining to them who else is there. But the whole
purpose for Elanus is so they just know and self-navigate and negotiate. I'm thinking that claude/codex agents need
to have memory blocks injected by default that let them know what else is going on. Like which agent, what the 
agent is trying to do, etc. I'm imagining this just happens by default. I'm not sure why any agent needs to have
this turned off, just feels like a basic functionality of elanus that most agents all get injected.


How does it work when we have a list of 5 kits and 10 skills and we wire it into claude code? codex? ...

---
name: lanius-voice
description: >-
  How lanius talks — the plain-language rules for ALL user-facing copy in the
  product: UI strings, labels, buttons, empty states, errors, help text, and the
  helper/assistant's on-screen words. Use this whenever you write or edit text a
  person reads on screen (anything in ui/web/, or copy in a handler/skill that
  surfaces to a user). The one job: say less, plainly — name things as they are,
  cut words, and keep the invented vocabulary out. Full audit + before→after in
  docs/handoffs/web-ui-copy.md.
---

# lanius voice — say less, plainly

lanius has a habit of talking like its own source code, in a private vocabulary
the user never agreed to learn ("instruments", "read camera", "comms plane",
"pinned to the larder", "correlation"). Every one of those makes the reader stop
and decode. The fix is always the same: **uninvent and simplify.**

## The rules
1. **Name it what it is.** If the user does X, call it X. "Sandbox," not "cage."
   "Messages," not "comms plane." "A saved model key," not "a credential the
   dispatcher points at."
2. **Say it once, in fewer words.** Most descriptions can lose half their words
   with nothing lost.
3. **No internal words on screen.** Never show "correlation", "principal", "web
   relay", "failure-mail", "manifest", "TOML", "resident/exec block", raw topic
   paths (`obs/agent/...`), raw CLI commands, or — worst — raw function names
   (`get_status`, `list_agents`). Those live in the code, not the UI. (In a
   *system prompt* the model may need the tool names; just never render them to
   the person.)
4. **One vocabulary.** No dual "cockpit/plain" registers. Pick the plain word and
   ship only that.
5. **The butcher-bird lives in the brand, not the words.** The logo, the icon,
   the README, the splash carry the shrike. App labels stay literal — no "larder",
   no "impaled", no new terms to learn.
6. **A control says what it does; a result says what happened.** "Send."
   "Approve." "Saved — applies next run." No decoration, no glyph-speak.
7. **Errors help.** What went wrong, then how to fix it.
8. **No codenames.** Internal persona names (Ganesh, Lily) are for our docs,
   never the UI.

## The model to copy
The existing **empty states and error toasts are already right** — short, plain,
consistent: `no agents yet — create one below`, `saved — applies on the next run`,
`save failed`. Make everything else read like these.

## A few before → after (see web-ui-copy.md for the full set)
- *"set up a useful agent, see whether the local stack is healthy, then open the
  cockpit when you need the real machinery."* → **"Set up an agent, check
  everything's running, and open one when you need the details."**
- *"the cross-agent comms plane — agent-to-agent mail (priority, state, failures)"*
  → **"Messages agents send each other, and the rooms they share."**
- `read camera` · `cage (writes)` · `broker` · `active principal` →
  **"Activity is readable" · "Sandbox — file writes" · "Message bus" · "Owner"**
- *"failure-mail on this correlation — the worker run failed."* → **"This run
  failed."**

## Quick test before you ship a string
- Would a smart non-engineer know what it means without a gloss? If not, rename.
- Can you cut a third of the words? Do it.
- Does it invent a term or a metaphor? Delete the invention.

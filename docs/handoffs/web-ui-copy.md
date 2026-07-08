---
status: done (delivered with redesign M1)
author: Claude Opus 4.8 (planner) — copy / voice
last-updated: 2026-07-07
---

# Web UI copy — say less, plainly

A companion to [web-ui-redesign.md](web-ui-redesign.md). The visuals are settled;
this fixes how the app *reads*. Today it uses too many words and a private
vocabulary — clever-sounding phrases the user never agreed to learn ("instruments,"
"read camera," "comms plane," "pinned to the larder"). The job is to **uninvent
and simplify**: name things plainly, cut words, keep the butcher-bird in the brand
and out of the buttons.

Grounded in a full string inventory of `ui/web/` (2026-07-07). The before→after
below is of real shipped strings, worst-first.

---

## The rules

1. **Name it what it is.** If the user does X, call it X. "Sandbox," not "cage."
   "Messages," not "comms plane." "A saved model key," not "a credential the
   dispatcher points at."
2. **Say it once, in fewer words.** Most descriptions can lose half their words
   with nothing lost. The table proves it.
3. **No internal nouns on screen.** "correlation," "principal," "web relay,"
   "failure-mail," "manifest," "TOML," "resident/exec block," and — never — raw
   function names (`get_status`, `list_agents`). Those live in the code, not the UI.
4. **One vocabulary.** Delete the cockpit/plain toggle. Ship the plain words only.
   Two dialects for the same thing is the problem, not a feature.
5. **The butcher-bird lives in the brand, not the words.** The logo, the icon, the
   README, the splash carry the shrike. The app's labels stay literal. No "larder,"
   no "impaled," no new terms to learn.
6. **A control says what it does; a result says what happened.** "Send." "Approve."
   "Saved — applies next run." No decoration, no glyph-speak.
7. **Errors help.** What went wrong, then how to fix it. The current error toasts
   already do this — match them everywhere.
8. **No codenames.** Persona names (Ganesh, Lily) are for our docs, never the UI.

---

## The one-vocabulary decision (delete the toggle)

`App.tsx:31-34` ships a `cockpit`/`warm` `LABELS` map. Collapse to one plain set,
remove `#vocabulary-toggle` and `localStorage['lanius.cockpit']`:

| concept | cockpit (delete) | warm | **ship** |
|---|---|---|---|
| the live view | instruments | explore | **Activity** |
| per-agent live | telemetry | activity | **Activity** |
| past chats | sessions | history | **History** |
| coding jobs | runs | runs | **Runs** |
| send button | transmit | Send | **Send** |

Then the nav reads, plainly: **Activity · Setup · Runs · Messages · Providers**,
then **Agents**, then **Workers**. ("signals"→Activity, "comms"→Messages.)

---

## Before → after (real strings)

### Health / setup card — the metaphor labels (`App.tsx:1747-1783, 1884-1887`)
| before | after |
|---|---|
| `read camera (advisory)` / `(authoritative)` | **Activity is readable** (advisory / confirmed) |
| `cage (writes)` / `cage (reads)` / `cage (network)` | **Sandbox — file writes / file reads / network** |
| `broker` | **Message bus** |
| `owner credential` | **Model key** |
| `root` | **Data folder** |
| `active principal` | **Owner** |
| `web relay` | **This web app** |
| `A cheap security summary for Ganesh: what is local, where data lives, and which capabilities create risk.` | **A quick security summary: what runs locally, where your data is, and what could be risky.** |

### View descriptions (`App.tsx:869-874`)
| before | after |
|---|---|
| `a live view of everything happening — orange means something needs your attention` | **What's happening now. Red means something needs you.** |
| `coding runs and the workers they spawned — tool, model, effort, duration, and a resume command` | **Coding runs and the workers they started.** |
| `the cross-agent comms plane — agent-to-agent mail (priority, state, failures) and the coordination rooms` | **Messages agents send each other, and the rooms they share.** |
| `named, encrypted model-provider credentials — add, test reachability, and select one per agent` | **The model keys your agents use. Add one, test it, pick one per agent.** |

### Welcome (`App.tsx:1705, 1802, 1618, 1441`)
| before | after |
|---|---|
| `set up a useful agent, see whether the local stack is healthy, then open the cockpit when you need the real machinery.` | **Set up an agent, check everything's running, and open one when you need the details.** |
| `Use the cockpit when you need transcript, telemetry, or advanced config.` | **Open an agent for its history, activity, and settings.** |
| `setup first, cockpit when needed` | *(delete)* |
| `agent explorer // live` | **your agents** |

### The helper panel — stop reciting function names (`App.tsx:1605, 2281-2282, AgentAssistant.tsx:57`)
| before | after |
|---|---|
| `Help me get set up... Use get_status/list_agents/list_packages/list_providers/read_conversation to look things up, and navigate to take me to what you're describing.` | **Ask me to help you get set up, or about anything here — your agents, models, and settings. I can look things up and take you where you need to go.** (the tool list stays in the system prompt, off-screen) |
| `Context author` | **Settings helper** |
| `Help add one useful context step... Start by calling list_context_blocks...` | *(system-prompt only; the panel title reads)* **Add a prompt step** |
| `profile` (the picker label) | **Agent** |

### Messages / comms (`CommsView.tsx:69,85,91,194,205`)
| before | after |
|---|---|
| `failure-mail on this correlation — the worker run failed.` | **This run failed.** |
| `correlation:` | **Thread:** |
| `this urgent copy was injected mid-task (between the agent's tool calls); it is still unread until the agent pulls its inbox` | **Delivered while the agent was working — unread until it checks its inbox.** |
| `No agent-to-agent mail yet. When one coding agent delivers work to another (\`lanius code deliver\`), it shows here...` | **No messages yet. When one agent hands work to another, it shows up here.** |
| `No coordination rooms with members. Sessions sharing a workdir (or launched with \`--room\`)...` | **No shared rooms yet. Agents working in the same folder share a room here.** |

### Providers (`ProvidersView.tsx:115,118,141,171`)
| before | after |
|---|---|
| `A provider is a named, encrypted credential the dispatcher or a coding tool can point at. The key is stored encrypted at rest; only its redaction is shown here.` | **A saved model key your agents can use. It's encrypted; only a masked version shows here.** |
| `native login — nothing to probe (the tool uses its own login)` | **Uses the tool's own login — nothing to test.** |
| `stored encrypted; sent once over loopback, never placed on the command line` | **Stored encrypted. Never shown in full or put on the command line.** |

### Badges (`App.tsx:227-243, 190-201`)
| before | after |
|---|---|
| `low surface` | **Low risk** |
| `broad publish` | **Posts widely** |
| `prompt context` | **Adds to prompts** |
| `agent-tunable` | **Agent can change this** |
| `actor` / `stage` / `hook` / `mcp` (capability types) | **Service / Prompt step / Event handler / Tool** |

### Empty states with a CLI command in them (`CodeSessions.tsx:687`)
| before | after |
|---|---|
| `No coding sessions yet. (Run \`lanius code project\` to refresh, or start a worker.)` | **No coding runs yet. Start a worker to see them here.** *(put "refresh" on a button, not in the sentence)* |

---

## What NOT to touch
- **Empty states + error toasts** (`no agents yet — create one below`, `save failed`,
  `saved — applies on the next run`) are already the model. Leave them; make
  everything else match.
- The cost labels (`hard cap` / `soft limit` / `estimate`) are already plain.

## The one-line diff of intent
> Before: the app talks like its own source code, in two dialects.
> After: it says what you can do, once, in words you already know — and the bird
> is on the logo, not in the sentences.

## Scope / migration
- These are string changes — no data-contract or e2e-**id** changes (the selectors
  are `id`s, not text). Safe to do inside the redesign's M2/M3.
- A few e2e assertions check visible **text** (e.g. a label). Grep `ui.spec.mjs`
  for the changed phrases and update those assertions in the same commit.
- Delete `#vocabulary-toggle`, the `LABELS` map, and `localStorage['lanius.cockpit']`.

## Read these first
- The string inventory this is built on (in this handoff's before→after).
- `ui/web/src/App.tsx` (`LABELS:31`, view copy `:869-874`, welcome `:1705`,
  helper `:1605`, badges `:190-243`), `ProvidersView.tsx`, `CommsView.tsx`,
  `components/AgentAssistant.tsx`.
- `web-ui-redesign.md` (this is its Voice & copy companion).

## Log
- 2026-07-07 (Opus, planner): wrote from a full string inventory. Findings:
  the cockpit/plain dual vocabulary is the top fix (delete it); metaphors
  ("cage," "read camera," "comms plane") and internal nouns ("correlation,"
  "principal," "failure-mail," raw function names) leak straight to screen; one
  codename ("Ganesh") shipped in user copy and must be removed. Empty states and
  error toasts are already plain — the model for the rest. Rule of thumb: name it
  what it is, say it once, keep the bird in the brand.

---
status: observed
observed: 2026-07-11
surface: http://127.0.0.1:7180/agents/claude-code/activity
---

# A worker is split across four unexplained surfaces

This is an investigation, not a proposed redesign. It records what the worker
UI felt like in a live instance, what each surface actually reads, and why the
whole currently fails to explain what Claude Code or Codex is doing.

The short version: the UI presents Chat, History, Activity, and Runs as nearby
views of one thing. They are actually four projections with different sources,
retention, identities, and failure modes. The UI does not teach those
boundaries, so a person has to reverse-engineer Lanius before they can read it.

## The report that prompted this

When Tim opens a worker, it lands on Activity. Activity looks like a log file
and is hard to read. He often wants to talk to the worker instead. History seems
similar to Activity but is never available. Runs appear and disappear without
explaining what they are, and the many subagents inside Claude Code and Codex do
not become legible children. The interface shows machinery without explaining
the work.

That report is accurate. It also matches an earlier journey ruling: Activity is
useful as a raw escape hatch but should not be an agent's resting page
([UI preferences](../journeys/ui-preferences.md#log-like-surfaces)).

## What the live UI showed

The walkthrough used the existing server on port 7180 and made no durable UI
changes. The page was backed by the live root at `~/.lanius/root`. The source
tree was simultaneously being changed by active workers, so code references
below describe the working tree observed on 2026-07-11, not necessarily a clean
commit.

### Opening a worker deliberately selects Activity

This is not browser memory or a routing accident. The navigation divides agent
nouns into ordinary agents and workers. Clicking an ordinary agent calls
`selectAgent(name)`, whose new-agent default is the internal `converse` tab.
Clicking a worker explicitly calls `selectAgent(name, 'telemetry')`
([Nav.tsx](../../ui/web/src/views/Nav.tsx), lines 13–18 and 49–76;
[App.tsx](../../ui/web/src/App.tsx), lines 323–330).

The resulting URL is `/agents/claude-code/activity`. The live page showed 1,002
events and filled the stage with rows like:

```text
obs/agent/claude-code/code-9b42979f/tool/Read/call {"cc_session": ...}
obs/agent/claude-code/code-9b42979f/tool/Read/result {"failed": false, ...}
```

Rows can expand to raw JSON, which is an improvement over an entirely inert
log. The collapsed form is still a topic path followed by a clipped serialized
payload. There is no turn grouping, task title, parent/child structure, outcome,
or summary of why the event matters
([RailView.tsx](../../ui/web/src/views/RailView.tsx), lines 16–64).

### The Chat tab currently becomes a trace doorway

The visible tab is still named **Converse**. In this document it is called Chat
because that is the product concept and the requested language.

On the live `claude-code` noun, Chat said:

> claude-code hasn’t sent any messages on the comms plane — its work shows up as
> a trace.

It then offered fourteen generically named run choices, a box to send a note to
one worker, and a link to Runs. This is a deliberate fallback: if the comms
projection contains no conversation but worker observation traffic exists, the
component replaces chat with a trace explanation
([ConverseView.tsx](../../ui/web/src/views/ConverseView.tsx), lines 47–84).

Work in progress in the same tree is adding worker DM conversations. That may
make Chat richer, but it does not by itself settle what the worker's home should
be. A note sent to a running job, a durable human-facing conversation, and an
observed trace are still three different interactions.

### History was visible but unreachable

History showed:

```text
transcripts unavailable — live view only.
the background service isn’t running — start it with `lanius daemon`.
```

The actual runtime state was subtler. `/api/status` reported History as
`available: true`, `grant: allowed`, with an HTTP endpoint. A direct
`/api/history?kind=agents` request returned HTTP 503 because that endpoint was
unreachable. `available` currently means the endpoint file exists, not that the
actor answers. The proxy rereads that file and returns 503 when the connection
fails ([web.rs](../../src/web.rs), lines 497–534 and 1179–1223).

So the displayed diagnosis was not reliable. More importantly, History is not
an Activity archive. It asks the history package for message transcripts and
conversation sessions
([App.tsx](../../ui/web/src/App.tsx), lines 1259–1291;
[SessionsView.tsx](../../ui/web/src/views/SessionsView.tsx), lines 1–51). A
person comparing the two adjacent labels has no way to discover that distinction.

### Settings is not a meaningful worker home either

`claude-code` is a traffic-derived noun, not a configured profile in the live
instance. Settings rendered a full configuration form but ended with:

```text
no settings file for claude-code — this agent only exists as traffic
```

This makes Settings a poor fallback default for the current worker noun. It
looks actionable before admitting that there is no corresponding object to
save.

### Runs contained the useful structure, buried in opaque rows

The Runs API returned 154 logical coding threads during the walkthrough. The
page rendered a long tree sorted with running and idle work first. A typical
row exposed:

- an opaque `code-*` id;
- `claude` or `codex`;
- model and effort, often `? / ?`;
- running, idle, or done;
- duration, resume/relaunch counts, and tokens.

Some rows had disclosure arrows and indented children. There was no task name,
prompt summary, human label, owner/planner, or explanation of why a row was
still `running` or `idle`. Symbols for resumes, relaunches, and token direction
had no visible legend. The page therefore contains more useful structure than
Activity while remaining difficult to identify with the work a person actually
asked for.

Focusing `/runs/code-d94cac68` did not replace or filter the ledger. The entire
list stayed above the selected detail, putting the result of a deep link far
below the fold. The detail itself was substantially richer: timeline, note
composer, and a `spawned workers` section. But its children were still opaque
ids with statistics rather than named tasks or dispatch reasons.

The worker count in the left navigation is a different, live-derived number.
It showed six while the durable Runs projection showed 154. Neither label
explains the difference. Chat offered fourteen generic Claude run choices at
the same moment. These three counts cannot be reconciled from the screen, which
makes Runs appear to come and go even when the underlying projections are
behaving as written.

## The four data planes

| Surface | What it actually reads | Retention | Unit shown |
| --- | --- | --- | --- |
| Chat / Converse | comms projection over addressed human/agent messages | durable ledger projection | a conversation thread |
| History | history-package queries over stored messages | durable while the package is healthy | sessions and transcripts |
| Activity | recent SSE frames from bus traffic | live ring only | a raw event |
| Runs | coding-session sqlite projection plus live coding observations | durable projection merged with live updates | a Lanius coding thread and its Lanius children |

Activity is especially easy to misread as history. The web relay holds only a
1,000-frame in-memory ring and replays it to a new SSE client
([web.rs](../../src/web.rs), lines 61–76 and 300–312). The browser filters that
buffer by the selected agent topic and renders at most the latest 600 matching
rows ([App.tsx](../../ui/web/src/App.tsx), lines 1293–1301). It is literally a
live tail, not a record of what the agent has done.

Runs is narrower than its name suggests. Its projection accepts only coding
observations beneath the `codex` and `claude-code` nouns with a `code-*` thread
id ([code_projection.rs](../../src/code_projection.rs), lines 135–157). It does
not represent every activation of every Lanius agent.

## What counts as a child run

The existing handoff says Runs ships a “nested subagent tree”
([coding-agent-observability.md](../handoffs/coding-agent-observability.md)). In
the implementation, that phrase has a narrower meaning: a child is another
Lanius coding launch whose launcher inherited the parent session id. The parent
edge is captured from `LANIUS_CODE_SESSION` or the async reply route when
`lanius code` starts the child ([codeagent.rs](../../src/codeagent.rs), lines
4050–4061).

That catches workers launched through Lanius. It does not automatically make a
harness-native subagent into a separately identified Lanius coding thread.

- Claude Code `Agent` tool calls appear in the parent's activity/timeline. The
  generated hook set does not currently subscribe to Claude's separate
  `SubagentStart` and `SubagentStop` events, so those native children do not
  acquire an inspectable identity and lifecycle in Runs.
- Codex native collaboration agents have the same conceptual gap unless their
  launch crosses the Lanius coding-launch seam. Two native observer subagents
  were launched during this investigation; no new child rows appeared beneath
  the current Lanius run while they worked.
- A Lanius-launched nested Claude or Codex worker does get a parent edge and can
  appear recursively in the Runs tree.

This is why both statements can be true: Runs has a real child tree, and the
subagents a person knows are active can still be absent. “Subagent” names a
harness concept; the UI tree currently models a Lanius launch relationship.

## The launch already contains most of the missing explanation

The opaque tree is not primarily a collection problem. For a worker launched
through `lanius code`, Lanius knows considerably more than Runs shows.

### Known directly

- **Who launched it:** a nested or detached launch records the parent Lanius
  coding-session id. `lanius code spawn` derives the spawner from
  `LANIUS_CODE_SESSION`; the ordinary nested launch also inherits that identity
  ([codeagent.rs](../../src/codeagent.rs), lines 1041–1105 and 4050–4061).
- **What it was asked to do:** the launcher has the initial prompt as an
  argument. It publishes that text as the retained session `intent` event before
  the worker starts ([codeagent.rs](../../src/codeagent.rs), lines 3773–3826).
  The current working tree also stores this as the session's durable baseline
  intent, with the first interactive user prompt filling it when a TUI launch
  had no initial task ([codesession.rs](../../src/codesession.rs), lines
  471–538).
- **Where and how it started:** `session/start` includes tool, workdir, complete
  launch args, parent, model, effort, provider, and capability posture
  ([codeagent.rs](../../src/codeagent.rs), lines 3845–3875). The durable session
  record also knows its coordination room.
- **Which model did the work:** model name and reasoning effort are separate
  launch facts. When supplied, both should travel with the worker everywhere it
  appears—list row, tree child, detail, and activity summary. Reasoning effort
  must not disappear into a generic model label; `gpt-5.6 / high` and
  `gpt-5.6 / xhigh` describe materially different runs. When either fact was not
  captured, the UI should say which one is unknown instead of rendering the
  context-free `? / ?` seen in the live Runs ledger.

### Model and effort are part of the worker's identity

The model is not merely an implementation detail attached to an otherwise
interchangeable worker. Along with its instructions, tools, memory, and
authority, it is a significant part of who that worker was for this piece of
work. It shapes what the worker is likely to notice, how it reasons, how much
context it can use effectively, its characteristic failure modes, its speed,
and its cost. Two children given the same prompt but run on different models are
not meaningfully the same delegation.

Reasoning effort belongs to that identity too. It is not a decorative tuning
value. A medium-effort implementation pass and an xhigh-effort adversarial
review may use the same named model while being assigned very different jobs
and expected to behave differently. Hiding effort erases part of the planner's
decision. Showing only `codex` or `claude` erases even more: those are harnesses,
not the minds that performed the work.

This matters most in the tree. “Claude launched Codex” describes plumbing.
“Claude Opus planned the change, then launched GPT-5.6/high to implement it and
GPT-5.6/xhigh to verify it” explains the orchestration. It lets a person judge
whether the delegation made sense, understand why one child took longer or cost
more, and decide how much confidence to place in each result. Without model and
effort, the tree conceals the most consequential choice the parent made about
its children.

The UI should therefore treat model and effort as primary identity text, close
to purpose and status, on every collapsed worker row. Detail can add provider,
context window, token use, or pricing, but the person should not have to open a
trace to learn which model did the work. If the requested model and the model
actually resolved by the provider differ, preserve both and foreground the
resolved model. If resolution is genuinely unknown, say `model unknown` or
`effort not supplied`; an unexplained `? / ?` makes missing capture look like
meaningless data.

Live examples made the loss obvious. Child `code-9b42979f` had parent
`code-79985d39` and an intent beginning “You are implementing milestone M2…”;
child `code-de0c323a` had the same parent and an intent beginning “You are
implementing milestone M0…”. Runs rendered those as ids, tools, and statistics.
The useful sentences were already in each detail timeline but were not promoted
to the list, tree row, detail heading, or parent-child relationship.

### Reconstructable but not first-class

The parent timeline usually contains the shell-tool call that invoked
`lanius code`, often including the complete command and prompt. Combined with
the child's parent edge and launch timestamp, this gives a strong account of
which parent shell action caused the launch. There is not currently a durable
foreign key from the child to that exact tool-call event. The UI would have to
correlate nearby evidence rather than follow an explicit `launched_by_event`
edge.

### Not currently captured

For a root TUI started directly by a person, Lanius does not persist the human's
terminal or shell identity as a product concept. It knows the coding tool,
workdir/room, process lifecycle, and launch arguments. The distinction matters:
“this Codex child was launched by Claude session X” is known; “this root was
launched from Tim's iTerm tab Y running zsh” is not.

The largest immediate loss happens in the projection. `session/start` contains
`args` and the trace contains `intent`, while `code_session_stats` keeps only
tool, workdir, model, effort, parent, lifecycle, and token fields
([code_projection.rs](../../src/code_projection.rs), lines 31–56 and 263–289).
The Runs list/detail API is therefore handed an identity-stripped record even
though the source observation already carries purpose.

## Where the interface loses the person

### One apparent object is several incompatible objects

The header says `CLAUDE-CODE`, as though Chat, History, Activity, and Settings
are properties of one configured agent. In the live system the noun is partly a
tool family, partly an observation namespace, partly a bucket for worker DMs,
and not a configurable profile at all. Runs then moves the most concrete worker
identities to a separate top-level page.

### The default optimizes for available telemetry, not likely intent

Activity is the easiest projection to populate because bus frames arrive
without needing a healthy reconstruction package. That makes it a dependable
debug surface, not a good home. The journey evidence favors conversation for
ordinary agent interaction across Tim, Lily, and Daniel
([07-chatting.md](../journeys/07-chatting.md#what-chatting-should-feel-like)).
For ambient agents, an administrator may arrive specifically to change settings.
There is no journey evidence that another persona wants a raw event tail as the
default worker experience.

Tim and Daniel are the personas most likely to drive Claude Code or Codex. The
worker-specific journey records Tim's immediate desire to message a worker and
the need to distinguish observing from conversing, rather than silently
preventing one ([07-chatting.md](../journeys/07-chatting.md#talking-to-a-coding-session)).

### State exists without an account of the work

Activity answers “which events arrived?” Runs answers “which coding threads
were projected?” Neither answers the first human questions:

- What is it trying to do?
- Is this my current worker or an old one?
- What did it delegate, and why?
- Which child is still working?
- What changed, succeeded, failed, or needs me?
- Can I talk to the responsible agent from here?

The system has pieces of these answers—intent events, assistant summaries,
parent edges, lifecycle state, DM threads—but no surface assembles them into an
explanation.

## Language: Chat, not Converse

“Converse” is visible in the agent tab and the welcome action, while the empty
state and surrounding product language already say “chat,” “conversation,” and
“messages.” The requested product word is **Chat**.

This is not only one label. The working tree contained 157 case-insensitive
uses of `converse` across the web source, tests, and docs. Many are internal
identifiers (`AgentTab`, `view-converse`, route helpers) and do not have to block
a copy correction. But an across-the-board cleanup should inventory:

- visible labels and accessibility text;
- helper/navigation tool schemas that expose `converse` as an enum;
- tests and selectors that encode the product word;
- comments and design docs that use Converse as if it were a distinct concept;
- internal identifiers, deciding deliberately whether compatibility or one
  vocabulary matters more. The browser URL is already the bare
  `/agents/:agent`, so changing the visible word need not create a route migration.

## Directions suggested by the evidence, not decisions

These are constraints for a later journey or handoff, not a wireframe:

- A worker needs a legible home that answers “what is happening?” before it
  exposes the event stream.
- Chat should be the ordinary default when a chat relationship exists. A
  traffic-only worker needs an honest worker-specific home, not a fake Settings
  form or raw Activity by default.
- A useful activity view needs semantic grouping around work, turns, tools,
  outcomes, and delegation. Raw topics and JSON remain valuable as disclosure.
- Runs needs human identity: task/intent, parent purpose, recency, and a clear
  account of `running`, `idle`, and `done` before more statistics. Launch intent
  and parent are already available and should be treated as primary fields, not
  buried timeline payloads.
- Every worker presentation should name its model and reasoning effort when
  supplied. These belong beside purpose and status on the collapsed row, not
  behind the detail view.
- A child should link to the parent action that launched it. Parent session id
  is already exact; recording the specific spawning tool-call event would turn
  a strong reconstruction into a direct causal edge.
- Native harness children and Lanius-launched children must either share a
  model or be explicitly named as different things. Quietly showing only one
  class will keep violating the person's direct observation.
- History needs an honest availability check and a name/description that makes
  clear it is transcript reconstruction, not archived Activity.
- The UI should make observing, sending a note to a live job, and having a
  durable chat distinguishable but adjacent. The person's instinct to talk to
  the worker is evidence, not misuse.

## Open questions worth observing next

1. When a worker has both a DM thread and a live trace, what should its home
   foreground without hiding the other?
2. Is the durable object people care about the coding thread, the harness TUI
   session, the task, or the agent/tool noun? Current navigation mixes all four.
3. Which native Claude and Codex lifecycle events are available reliably enough
   to give subagents their own identity, parent, intent, and state?
4. What does `idle` mean to a person, and when should an idle or stranded run
   stop occupying the top of Runs?
5. Should “History” survive as a separate pane once Activity and Chat each have
   durable, legible timelines?
6. For ambient-agent administrators, should a profile remember a preferred
   landing surface, or can the UI infer one from the kind of object selected?

## Evidence gathered

- Headless browser walkthrough of Activity, Chat/Converse, History, Settings,
  and Runs on the live port 7180 surface.
- Direct reads of `/api/status`, `/api/history`, `/api/code/sessions`, and
  coding-session details.
- Independent native-agent reads: one live UX observer and one code/data-plane
  tracer.
- Source tracing through `Nav.tsx`, `App.tsx`, `ConverseView.tsx`,
  `SessionsView.tsx`, `RailView.tsx`, `CodeSessions.tsx`, `web.rs`,
  `codeagent.rs`, and `code_projection.rs`.

# The context pipeline

Decided 2026-06-12 (HANDOFF plan, phase 2). One mechanism replaces four
ad-hoc seams — profile blocks, `[[provider]]`, context/history policy,
payload→prompt mapping: an ordered chain of *context stages*, each a program
`Context -> Context` over one typed JSON document. **Programs decide
content; the kernel guarantees the wire.**

## [DECIDED 2026-06-18] The agent owns a context program

An agent is not "a prompt with tools." An agent is the language-model actor
implementation described in docs/actors.md: a durable actor identity with an
inbox, a model loop, capabilities, state, and policy. The thing that makes one
agent behave differently from another is its **context program** — the program
that turns an incoming event, the session transcript, actor state, memory,
skills, tools, and policy into the provider request.

The context program is broader than a system prompt:

- it chooses and orders system blocks;
- it maps the incoming event into the opening user message;
- it windows, rewrites, or annotates transcript messages within the wire rules
  below;
- it invokes memory/recall/index context stages;
- it exposes tools and skill inventories;
- it may consult resident actors or exec scripts, but the output is still one
  validated context document.

Prompt text is therefore an output artifact, not the primitive. A "prompt"
field in an agent config is only one input to this program, and usually not the
most important one.

A context program is not an identity. A package may ship a context program or
context components, but an agent runs a bound instance of that program under the
agent's authority. If two agents use the same context program, they default to
separate bound instances: same package/code provenance, different agent
authority, state, topics, caches, and build logs.

## [DECIDED 2026-06-18] Agent, session, run

Use three separate words:

- **Agent** — the durable actor identity and policy. It persists across all
  conversations and runs.
- **Session / conversation** — a durable thread of messages for that agent.
  Continuing a chat means appending to the same session; starting a new chat
  means a new session. The transcript is the session's truth.
- **Run / activation** — one wake-up of the agent in response to an event. A run
  may perform several provider requests and tool calls before it exits,
  suspends, or fails.

The existing `model.max_turns` setting is really a run budget. Rename it toward
`max_steps` / `max_llm_calls` when the agent config surface is cleaned up.

## [DECIDED 2026-06-18] Parameters belong to context stages

The current profile `[vars]` map is only a thin parameter channel for context
stages and templates. It is not a satisfying top-level agent concept, and it
should not drive the product model. Long term, context stages should declare
typed parameters beside the context stage that consumes them: the
recent-history context stage owns its window size, a recall context stage owns
its retrieval policy, an event-prompt context stage owns event-to-message
mapping knobs, and so on.

Typed `[[stage.config]]` declarations now give the product UI named controls,
but the runtime wire stays simple: the kernel resolves stage defaults and
package config into `meta.vars`, then lets profile `[vars]` override those
values for one agent. `[vars]` remains the advanced escape hatch:

- `meta.vars` gives context stages access to string parameters;
- `{{name}}` template substitution can use the same values;
- the UI should not present arbitrary key/value pairs as if they define an
  agent. It should prefer the context-stage tile controls and show raw key/value
  rows only as "advanced context parameters."

## [DECIDED 2026-06-18] Packages ship context components, not identity

A package can provide reusable context-building pieces:

- static memory blocks;
- computed block producers;
- context stages/transforms;
- registers or block stores;
- placement preferences and configuration schemas;
- tools, APIs, or MCP endpoints that mutate agent-scoped blocks/registers.

The package supplies code and schemas. The agent supplies identity and
authority. Installing or enabling the same package for two agents must not
merge their memory, subscriptions, caches, or writable state by default.

Resident context components therefore default to agent-bound processes or
agent-bound sessions. A shared resident daemon is a later optimization only if
it proves strict caller identity, agent-scoped handles, isolated state, and
auditable provenance.

## [DECIDED 2026-06-18] Blocks, registers, and the context build log

Context assembly should be explainable as a durable build log, not only as a
final prompt string. Each provider request starts from an empty context and is
materialized through ordered transformations:

```text
empty context
  -> seed event/session metadata
  -> add static memory blocks
  -> read registers/block stores
  -> run computed block producers
  -> run context transforms
  -> apply placement/budget rules
  -> format transcript and provider request
```

Core concepts:

- **Block**: a named, typed, hashable piece of context with content, placement
  preference, priority, owner, package provenance, and scope.
- **Register**: a small mutable value set by an API, tool, MCP server, or actor
  and read by a computed block/context stage.
- **Computed block**: a function from agent/run state to one or more blocks,
  for example time of day, a file tail, or a synthesized memory summary.
- **Placement policy**: the agent/context policy that decides whether blocks
  land in system text, user text, before/after transcript, or are dropped under
  budget.
- **Build log**: durable records, stored beside the conversation transcript,
  that explain which component added, removed, rewrote, or formatted each part
  of the final provider request.

The harness owns block semantics: validation, hashing, placement, budgeting,
and build-log writes. Package executables can produce blocks or transforms, but
the meaning of a block must be understood by the harness and shared libraries.

Implementation lean: create a Rust library crate for the block/context data
model and algorithms, with optional binaries for package actors. Do not make
the block model only an executable service. Future JS/Python libraries should
target the same schema/protocol.

Built substrate (2026-06-18):

- `src/context_blocks.rs` defines `ContextBlock`, `Register`,
  `Placement`, `Scope`, build-log action/record types, content hashing, and
  name validation.
- `src/lib.rs` exports the block primitives as a library module for future
  package binaries.
- SQLite tables `context_blocks` and `context_build_log` store block-shaped
  state and durable assembly summaries/hashes. The current provider request
  path still uses `context::Doc`; moving actual assembly onto these tables is a
  later step.

## [BUILT 2026-06-18] Subagent policy and lineage substrate

A subagent is an ordinary elanus agent spawned by a parent run, not a separate
actor class. Profiles now declare the policy future spawn code must enforce:

```toml
[subagents]
allow_profiles = ["scout"]
inherit_budget = true
max_depth = 1
grant_policy = "narrow"
# context_program = "default"
```

Defaults allow no generic subagent spawns. `allow_profiles` is the child-profile
allowlist, `inherit_budget` records that child work is charged to the parent run
budget, `max_depth` caps recursive spawning, `grant_policy = "narrow"` means the
child cannot inherit broader grants than the parent, and `context_program`
optionally restricts child context policy.

The SQLite `subagent_sessions` table records parent session, child session,
parent event, parent/child agent names, child profile, budget inheritance,
context-program restriction, grant policy, and lifecycle timestamps. No generic
launcher is built yet; this is the enforceable storage/policy substrate it will
have to use.

## [DECIDED 2026-06-18] Context stage resolution is dynamic, not copied

The context program is explicit, but it is not a giant copied pipeline inside
each agent's config. It is derived at run time from obvious rules:

1. The harness owns non-optional built-ins: seed the document, replay the
   transcript, enforce final validation, and build the provider wire request.
   An agent cannot disable these.
2. Candidate context stages come from packages visible to the agent through the
   agent's effective `elanus_path` and package visibility rules.
3. A candidate runs only when its package is approved, its manifest/code hash is
   current, and its context-stage grant is approved.
4. The agent's context policy may disable, enable, configure, or order context
   stages only within the authority it has. Protected/system context stages are
   not removable by the agent. Other actors may modify the pipeline only through
   the same configuration/proposal path and permission checks as any other
   authority-bearing change.
5. The resolved chain is sorted deterministically by declared phase/order and
   then by `(package, context-stage name)`. A context stage should not need to
   know which other context stages exist.
6. The full resolved chain is emitted as observation metadata for debugging, but
   the full context document is not broadcast.

This is inversion of control with a small kernel: packages declare independent
context stages, authorized configuration can influence which context stages are
in the chain, and the harness remains the part that resolves and executes the
chain.

## [DECIDED 2026-06-18] Calling a resident context stage

Context stages are privileged readers and writers of the provider request, so
resident context stages are spawned and registered by the harness. They do not
pick arbitrary public topics and wait for ambient broadcasts.

For a resident context stage:

1. The supervisor starts the package actor inside the appropriate cage with a
   bound execution identity: the authority is the agent's identity, while the
   package/code identity remains provenance for grants, review, and logging.
2. The harness allocates an exclusive request topic for the context stage and
   records it in kernel state. Only that context-stage actor may subscribe to
   the request topic; only the harness context runner may publish to it.
3. At run time the context runner resolves the ordered context-stage chain. For
   each resident context stage, it publishes `{doc, context_stage, budget}` to
   that context stage's exclusive request topic with a per-call response topic
   and correlation.
4. The response topic is exclusive to that call: only the context runner may
   subscribe to it, and only the called context-stage actor may publish to it.
   The context-stage actor publishes exactly one response with the same
   correlation: either `{doc}` or `{error}`.
5. The context runner waits for the response, validates the returned document,
   emits a summary observation, and then calls the next context stage.

The response topic gives the completion signal; blocking subscriptions are not
needed for context stages. Blocking subscriptions remain useful for interception
points where the broker itself is coordinating a publish, but the context
program already has an explicit caller and an explicit ordered chain.

## [DECIDED] The document (v1)

```json
{ "v": 1,
  "system":   [ {"name": "00-system.md", "text": "..."} ],
  "messages": [ {"role": "user", "text": "..."},
                {"role": "assistant", "text": "...", "tool_calls": [...]},
                {"role": "tool", "tool_call_id": "...", "name": "...", "content": "..."} ],
  "event":    { "topic": "in/agent/main", "payload": {}, "correlation_id": null },
  "meta":     { "profile": "default", "agent": "main", "session": "s1",
                "turn": 1, "model": "...", "vars": {} } }
```

- `system` is an ordered list of named blocks; the final system prompt is
  their texts joined with blank lines. Names exist so context stages can address
  blocks ("drop the skills inventory", "insert before 00-system.md").
- `messages` uses the *transcript row shape* (the normalized form stored in
  sqlite), not the provider wire shape — context stages transform dialogue, the
  kernel owns the conversion to the provider protocol afterward.
- `event` is the dispatching event, null fields for CLI-direct runs.
- `meta.vars` is the profile's legacy `[vars]` map - an advanced string
  parameter channel for context stages and templates until context-stage-owned
  typed parameters replace it.

## [DECIDED] Seed and chain, per LLM call

The kernel seeds the document and runs the chain before **every** LLM call
(history grows within a run; context stages must see it):

- **Seed system** (computed once per run, today's semantics preserved):
  profile blocks (sorted by filename), provider outputs (`[[provider]]`,
  by declared order), skills inventory — each as one named block. These are
  the in-kernel built-ins; with no context stages declared the assembled request is
  byte-identical to the pre-pipeline kernel (the golden parity gate).
- **Seed messages**: the session transcript (after crash repair), replayed
  faithfully. The opening user message is composed from the event payload
  (`payload.prompt`) and recorded to the transcript exactly as before —
  the transcript stays the truth of the dialogue.
- **Chain**: every visible (profile `skills.include/exclude`), approved
  package context stage, in deterministic total order `(order, package name,
  context-stage name)` — lexicographic tiebreaks, no dependency declarations. The order
  is greppable from manifests; `elanus stages` prints the effective chain, and
  `elanus context render --profile <agent> --session <id> --event <event-id-or-json>`
  prints the transformed context document with context-stage summaries.
  Chain order is *execution* sequence only: where a context stage's output
  lands is the context stage's choice (it edits the arrays).

## [BUILT 2026-06-18] Agent context-program config

Profiles now have a first-class context-program policy section:

```toml
[context]
program = "default"
max_total_ms = 30000

[[context.stage]]
package = "window"
name = "window"
enabled = false
order = 25
timeout_ms = 9000
```

`program = "default"` is the only supported recipe today: kernel built-ins seed
the document, then visible package context stages are resolved from approved
package manifests. `max_total_ms` is policy metadata for the future total
assembly budget; the current runner still enforces per-stage budgets.

`[[context.stage]]` entries are per-agent overrides for visible package context
stages. `enabled = false` removes the matching context stage from the resolved
chain. `order = N` overrides the manifest order before the final deterministic
sort by `(order, package, context-stage name)`. `timeout_ms = N` overrides that
context stage's runtime budget; exec stages enforce it directly. These entries
do not grant authority, bypass package visibility, or copy the whole pipeline
into the agent.

## [DECIDED] Context stage contract

Declared in the package manifest:

```toml
[[stage]]
name  = "recent-history"   # one topic level, like everything named
run   = "scripts/stage"
order = 30                 # default 50
mode  = "exec"             # "exec" | "resident"

[[stage.config]]
key = "window_rows"
type = "number"            # "string" | "number" | "boolean" | "array" | "enum"
default = 80
label = "Window rows"
help = "Maximum transcript rows kept by this context stage."
agent_tunable = true
```

- `mode = "exec"`: spawned per call; document JSON on stdin, transformed
  document JSON on stdout; 10s budget.
- `mode = "resident"` (BUILT 2026-06-12): the package's daemon actor is
  consulted over the bus — for context stages with state (db handles, caches).
  NOT MCP: MCP is a border protocol for third-party tool servers.
  As built today, response topic and correlation ride IN THE BODY, not MQTT
  §4.10 properties — the serving daemon can be a plain
  `elanus bus sub | transform | elanus bus pub` pipeline the CLI supports
  with no property plumbing.
    request   obs/harness/stagereq/<package>/<stage>
              {"doc": ..., "response_topic": ..., "correlation": ...}
    response  <response_topic>
              {"correlation": ..., "doc": ...} | {"correlation": ..., "error": ...}
  The forward design tightens that as-built wire: the harness allocates
  exclusive request/response topics for each resident context stage call, and
  ACLs ensure only the called context-stage actor and the context runner can
  participate. The full document is never recorded (broker carve-out: a document
  is megabytes; the per-context-stage obs delta is the record). The consult
  FAILS CLOSED — connect failure, timeout (15s), daemon absent, error response:
  the run fails, context-stage-attributed (opposite of resident hooks, which
  allow when the radio dies — a context stage composes meaning).
  The manifest must request context-stage capability; concrete subscribe/publish
  grants are minted by the harness when it allocates exclusive topics, so review
  stays explicit without making the topic public.
  packages/recent-history is the exemplar (warm read-only sqlite + cache).
- Context-stage scripts are covered by the manifest `code_hash`; each
  `[[stage]]` also registers a grant request (`kind = "stage"`), so a context
  stage runs only approved, and an edit re-enters review like any process or
  hook. The manifest key remains `[[stage]]` until a compatibility migration
  renames it; the design term is context stage.
- Context-stage parameters are declared as typed `[[stage.config]]` entries.
  Product UI should present these named, documented knobs instead of arbitrary
  `[vars]` maps. Runtime resolution is defaults first, package config second,
  and profile `[vars]` last so an agent-specific tile edit wins.

**Fail closed.** A context stage that errors, times out, emits invalid JSON, or
breaks a wire invariant fails the exec run with a context-stage-attributed
error. No silent skip: context stages can rewrite the opening message —
skipping one corrupts meaning. (Providers fail open; that's why providers stay
the append-only sugar and context stages are the real seam.)

## [DECIDED 2026-07-02] The `[[tool]]` seam — a package supplies an agent tool

A package may supply a **model tool**, not just a context stage. It is the same
move `[[stage]]`/`[[harness]]` made — a declaration is a grant request, dispatch
is the exec-mode contract — applied to the agent's tool array. This is a
**first-party** mechanism: MCP stays the border protocol for third-party tool
servers (`docs/mcp.md`), and a stock stdlib capability must not sit on the wrong
side of that border, so it rides `[[tool]]`, not `<server>__<tool>`.

```toml
[[tool]]
name        = "search_knowledge"   # ONE topic level, BARE — no <pkg>__ prefix
description = "Search the knowledge base for a query."
run         = "scripts/search"     # package-relative; exec-mode dispatch
timeout_ms  = 10000                # default 10s

# The input JSON schema, inline as a TOML table…
[tool.schema]
type = "object"
required = ["query"]
[tool.schema.properties.query]
type = "string"
# …or out of line: schema_file = "search.schema.json" (its bytes join code_hash).
```

- **Declaration is the request.** Each `[[tool]]` registers a grant of `kind =
  "tool"` (registered beside `[[stage]]`/`[[mcp]]` in `src/packages.rs`). The
  tool enters **no** agent's array until the human approves it into the ledger.
- **Availability = approved + visible.** The kernel folds approved packages' tool
  defs into the agent's array for profiles that can **see** the package — the
  same visibility gate `provides_builtin_tools` rides (`src/pkgtool.rs`).
- **Dispatch is exec-mode**, the `[[stage]]` contract: on a call the kernel
  spawns the `run` script with the call args as **JSON on stdin**; the script's
  **stdout JSON becomes the tool result**, under the `timeout_ms` budget (killed
  on the deadline). A nonzero exit or an overrun degrades that **one call** into
  a legible error result the model sees — it never wrecks the run.
- **`run` (and any `schema_file`) join `code_hash`**, so an edit re-enters review
  like any process, hook, or stage.
- **Bare name, one live holder.** The tool is `search_knowledge`, period. A
  **second** package declaring the **same** name swaps the engine behind the tool
  invisibly to agents; the approve gate refuses two live holders (and any
  kernel-builtin shadow) **loudly, naming the incumbent** (`src/packages.rs`
  `decide()`) — disable/revoke one, approve the other. Exec-mode only for now:
  a resident dispatch mode (the bus-consult shape stages have) is deferred until
  a tool needs held state.

## [DECIDED] Wire validation — what context stages may do to `messages`

After the chain, the kernel validates before building the provider request:

1. messages non-empty, first role is `user`;
2. every `tool` row answers a tool_call_id from the most recent assistant
   tool_calls, each exactly once;
3. every assistant tool_call is answered before the next user/assistant
   row (the tool_result-adjacency rule — DeepSeek 400s are the de facto
   conformance test).

Structure-safe operations, by permission:
(a) `system` blocks: unrestricted add/edit/remove/reorder;
(b) rewrite the opening user message's *content* (the transcript records
    the original; the rewrite is visible in obs deltas);
(c) drop complete *leading* turns (windowing) — a dropped assistant row
    takes its tool rows with it;
(d) rewrite content of historical rows in place, structure preserved
    (e.g. truncating a huge tool result).
Never fabricate mid-dialogue turns.

## [DECIDED] Timescale doctrine

Within-run transcript is kernel truth, replayed with adjacency intact.
Cross-run continuity is *reconstruction*: nothing accumulates across runs
by default; a context stage deliberately injects what earned its way back in
(timkellogg.me/blog/2026/04/14/forgetting). Hence the stock cross-run
context stage is `recent-history` (resident, reads the history db, injects a
system block) — not a compaction fallback.

Prompt caching is an observability and cost signal, not a design constraint.
Long-lived agents may intentionally rebuild context every call because the
right memory behavior is selection pressure, not append-only continuity. Context
stages choose their own memory strategy: stable memory blocks, sliding windows,
associative recall, prediction repair, and "do not promote this" are all valid
if they improve the agent.

The harness should measure cacheability without governing by it:

- provider cache hit/miss and cached-token counts, when available;
- stable prefix size across adjacent provider requests;
- per-context-stage token/byte churn;
- similarity between the previous final context and this final context that was
  not cacheable because it moved, rewrote, or reselected content.

That last metric is useful precisely because it shows memory/context systems
that are semantically stable but not prefix-cache-shaped.

## [DECIDED] Observability

After each context stage: one non-blocking QoS 0 record on
`obs/agent/<agent>/<session>/context/<context-stage>` carrying the
transformation summary: context-stage id, input/output document hashes, block
names, message counts, token/byte sizes, cacheability metrics, and a redacted
patch summary if the caller is allowed to see one. The full document never rides
the recorded bus; resident consults are point-to-point and unrecorded (hook
precedent).

Replay is an observation consumer, not a harness monopoly. The harness should
emit enough transformation events for authorized subscribers to reconstruct,
compare, index, or display what happened. Exact replay is only possible for
subscribers that also have access to the same underlying source data or captured
outputs.

If context inspection needs programmable redaction, reuse the same
document-transform idea on observation/debug documents instead of adding a
separate broker interception mechanism. Keep the first version conservative:
summaries only, full context only through an explicit privileged read path.

## [OPEN] (with leans)

- Explicit per-profile chain pinning/reordering (beyond visibility
  gating). Lean: don't build until a kit needs it — order ints have been
  enough for hooks.
- Composing the opening user message *in* a context stage when the seed is empty
  (today: rewrite-after-seed covers the known cases; the funnel composes
  payloads upstream). Lean: add when a kit actually needs structured
  event→message mapping, as a built-in named `event-prompt` that context stages
  may precede.

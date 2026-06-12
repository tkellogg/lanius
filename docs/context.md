# The context pipeline

Decided 2026-06-12 (HANDOFF plan, phase 2). One mechanism replaces four
ad-hoc seams — profile blocks, `[[provider]]`, context/history policy,
payload→prompt mapping: an ordered chain of *stages*, each a program
`Context -> Context` over one typed JSON document. **Programs decide
content; the kernel guarantees the wire.**

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
  their texts joined with blank lines. Names exist so stages can address
  blocks ("drop the skills inventory", "insert before 00-system.md").
- `messages` uses the *transcript row shape* (the normalized form stored in
  sqlite), not the provider wire shape — stages transform dialogue, the
  kernel owns the conversion to the provider protocol afterward.
- `event` is the dispatching event, null fields for CLI-direct runs.
- `meta.vars` is the profile's `[vars]` — the config channel for stages: a
  stage that wants a knob documents a var (e.g. `window_rows` for the stock
  window stage); the human sets it per profile.

## [DECIDED] Seed and chain, per LLM call

The kernel seeds the document and runs the chain before **every** LLM call
(history grows within a run; stages must see it):

- **Seed system** (computed once per run, today's semantics preserved):
  profile blocks (sorted by filename), provider outputs (`[[provider]]`,
  by declared order), skills inventory — each as one named block. These are
  the in-kernel built-ins; with no stages declared the assembled request is
  byte-identical to the pre-pipeline kernel (the golden parity gate).
- **Seed messages**: the session transcript (after crash repair), replayed
  faithfully. The opening user message is composed from the event payload
  (`payload.prompt`) and recorded to the transcript exactly as before —
  the transcript stays the truth of the dialogue.
- **Chain**: every visible (profile `skills.include/exclude`), approved
  package stage, in deterministic total order `(order, package name, stage
  name)` — lexicographic tiebreaks, no dependency declarations. The order
  is greppable from manifests; `elanus stages` prints the effective chain.
  Chain order is *execution* sequence only: where a stage's output lands is
  the stage's choice (it edits the arrays).

## [DECIDED] Stage contract

Declared in the package manifest:

```toml
[[stage]]
name  = "recent-history"   # one topic level, like everything named
run   = "scripts/stage"
order = 30                 # default 50
mode  = "exec"             # "exec" | "resident"
```

- `mode = "exec"`: spawned per call; document JSON on stdin, transformed
  document JSON on stdout; 10s budget.
- `mode = "resident"` (BUILT 2026-06-12): the package's daemon actor is
  consulted over the bus — for stages with state (db handles, caches).
  NOT MCP: MCP is a border protocol for third-party tool servers.
  As-built wire (deviation from the hook seam, deliberate): response topic
  and correlation ride IN THE BODY, not MQTT §4.10 properties — the broker
  is not the coordinator here, and the serving daemon is a plain
  `elanus bus sub | transform | elanus bus pub` pipeline the CLI supports
  with no property plumbing.
    request   obs/harness/stagereq/<package>/<stage>
              {"doc": ..., "response_topic": ..., "correlation": ...}
    response  <response_topic>
              {"correlation": ..., "doc": ...} | {"correlation": ..., "error": ...}
  Both prefixes are fan-out-only and never recorded (broker carve-out: a
  document is megabytes; the per-stage obs delta is the record). The
  consult FAILS CLOSED — connect failure, timeout (15s), daemon absent,
  error response: the run fails, stage-attributed (opposite of resident
  hooks, which allow when the radio dies — a stage composes meaning).
  The manifest must request subscribe on its stagereq topic and publish on
  obs/harness/stageresp/# — explicit grants, legible in review.
  packages/recent-history is the exemplar (warm read-only sqlite + cache).
- Stage scripts are covered by the manifest `code_hash`; each `[[stage]]`
  also registers a grant request (`kind = "stage"`), so a stage runs only
  approved, and an edit re-enters review like any process or hook.

**Fail closed.** A stage that errors, times out, emits invalid JSON, or
breaks a wire invariant fails the exec run with a stage-attributed error.
No silent skip: stages can rewrite the opening message — skipping one
corrupts meaning. (Providers fail open; that's why providers stay the
append-only sugar and stages are the real seam.)

## [DECIDED] Wire validation — what stages may do to `messages`

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
by default; a stage deliberately injects what earned its way back in
(timkellogg.me/blog/2026/04/14/forgetting). Hence the stock cross-run
stage is `recent-history` (resident, reads the history db, injects a
system block) — not a compaction fallback.

Cache note: stages that churn `system` every call will defeat provider
prompt caching when it lands (genai 0.7 knobs). Stock stages stay
cache-shaped — volatile material late or in messages.

## [DECIDED] Observability

After each stage: one QoS 0 record on
`obs/agent/<agent>/<session>/context/<stage>` carrying block names,
message count, byte sizes (before → after), truncated — the camera
doctrine applied to context assembly. The full document never rides the
bus recorded; resident consults are point-to-point and unrecorded (hook
precedent).

## [OPEN] (with leans)

- Explicit per-profile chain pinning/reordering (beyond visibility
  gating). Lean: don't build until a kit needs it — order ints have been
  enough for hooks.
- Composing the opening user message *in* a stage when the seed is empty
  (today: rewrite-after-seed covers the known cases; the funnel composes
  payloads upstream). Lean: add when a kit actually needs structured
  event→message mapping, as a built-in named `event-prompt` that stages
  may precede.

---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-07-03
---

# Memory blocks: two placement levels (system vs user)

Memory blocks today only render in the **system prompt**. This handoff gives
them a second home: the **user turn**. The two levels mean an agent can choose
*where* a block lands based on how often it changes.

Tim's framing:

> Memory blocks need 2 levels. Those that go in the **system prompt**
> (infrequently modified) and those that go in the **user prompt** (heavily
> modified, uses more duplicate tokens — avoid unless you need this).

The point is a cost trade the agent gets to make:

- **`placement=system`** — the block sits in the cached system prefix. Cheap to
  re-send turn after turn (the provider caches it), but every edit invalidates
  the whole cached prefix. Good for stable stuff (identity, long-lived notes).
- **`placement=user`** — the block rides inside the user turn. It is **not** in
  the cached prefix, so editing it costs nothing there; but it is re-sent in
  full on every turn (the "duplicate tokens" Tim names). Good only for context
  that changes almost every turn.

So the default is **system**; **user** is the escape hatch you reach for when a
block is genuinely hot. The guidance has to say that plainly, because an agent
picking `user` by reflex just burns tokens.

Tim adds one more reason this matters: *"the semantics of user vs system mean
that context programs can stack together more neatly."* A context program (a
package stage) can now target a distinct **user region** of the document,
separate from the **system region** — two labelled shelves instead of one. That
is why (below) the user blocks live as their own structured region of the
document through the whole stage chain, and only get flattened into message text
at the very end — mirroring exactly how `system` blocks already work.

**Scope:** `placement=user` only. The other three placements the table already
stores (`before_messages`, `after_messages`, `scratch`) stay unimplemented and
are listed as residuals. Do not build them here.

## Read these first

- `docs/handoffs/memory-blocks.md` — the parent. M1–M4 shipped the durable block
  substrate, the write CLI, the computed-block-is-a-stage claim, and the
  coding-agent projection. This handoff is the `placement=user` follow-on it
  deferred (its decision **2**: "M1 honors `system` placement only … grow the
  other placements later in the same model").
- `src/context.rs` — the document and the pipeline. `Doc { v, system:
  Vec<Block>, messages, event, meta }` (line 46); `Block { name, text }` (line
  24); `Doc::system_text()` (line 55) is the built-in system renderer;
  `assemble_detailed()` (line 210) seeds the doc and runs the stage chain;
  `validate()` (line 485) enforces the wire invariants after every stage.
- `src/render.rs` — `render_parts()` (line 30) builds the **system-only** seed.
  It calls `context_store::load_system_blocks` and `seed_defaults`.
- `src/context_store.rs` — `load_system_blocks()` (line 106) hard-codes
  `WHERE placement = 'system'` (line 126); `seed_defaults()` (line 533) forces
  `Placement::System` (line 544). These two are why user rows never render.
- `src/exec.rs` — the live run. The incoming prompt is persisted to the
  transcript at line 525 (`store_msg`, raw prompt); `render_parts` is called
  once per run at line 540; each turn re-reads the transcript
  (`transcript_rows`, line 1524) and calls `assemble` (line 632);
  `build_request` (line 1534) sends `doc.system_text()` via `with_system` (line
  1594) and maps `doc.messages` to chat messages (line 1546+).
- `src/blockcli.rs` — the write surface. `--placement` already parses and stores
  **all five** placements (lines 36, 78); a `user` row persists today. Nothing
  reads it back.
- `src/context_blocks.rs` — `Placement` enum (line 14) already has `User`.

## Wonky bits / decisions to confirm

These are the calls that shape the work. Each is decided with evidence; push
back if you see a hole.

### (a) Fold user blocks INTO the trailing user message — never a separate message

**Decided: fold.** Render the user-placement blocks to text and splice them into
the **most-recent `role:user` message** in `doc.messages`, so the wire still
carries exactly one user message there.

Why not a separate appended user message: `build_request` emits one
`ChatMessage::user` per `role:user` row (exec.rs:1550). On the common
first-turn-of-a-run case the last message *is* the user prompt, so a second
user message sits adjacent to it — **two consecutive user roles**, which the
Anthropic wire rejects (roles must alternate). `validate()` would **not** catch
this (it only checks tool_result adjacency, context.rs:495 — the `user` arm
never forbids consecutive users), so it would slip through to a provider 400.
Folding keeps a single user message and is adjacency-safe on every turn.

Fold shape: blocks first as a labelled memory preamble, then the user's actual
text (`"{blocks}\n\n{original_text}"`), matching how system context precedes the
ask. The exact header/order is low-stakes and tunable; the invariant is *one
user message, block text clearly not the human's own words.*

### (b) Seed the user region PRE-chain; flatten to text POST-chain

**Decided: pre-chain seed, post-chain fold** — the two are different moments and
don't conflict.

- **Seed pre-chain.** User blocks enter the document *before* the stage chain
  runs, exactly like system blocks do. Stages then see and can rewrite them.
  This is what makes Tim's "context programs stack neatly" literal: a stage can
  push/edit a user block the same way it pushes a system block.
- **Fold post-chain.** The flatten-to-message-text happens once, at the very end
  of `assemble_detailed`, after the last stage — mirroring the memory-blocks
  doctrine that "block→text rendering is built into assembly, run once at the
  end" and that a downstream stage sees structured blocks, not rendered text.

Concretely: add a `user: Vec<Block>` region to `Doc` (see M1), seed it from the
user seed pre-chain, let the chain transform it, then fold it into the trailing
user message and clear it. `build_request` stays unchanged (it reads
`doc.messages`), because by the time it runs the user blocks are already message
text.

*Alternative considered and rejected:* fold straight into the message text at
seed time (no `Doc.user` field, simplest). Rejected because a stage could then
only reach a user block by string-parsing the message body — it kills the
"distinct user region a context program can target" that Tim explicitly wants.
The structured region costs one serde-default field and a few signature changes;
worth it.

### (c) The double-injection hazard — why folding is safe across turns

The worry: a block folded into turn N's user message replays verbatim in turn
N+1's transcript *and* gets injected again, compounding every turn.

**It cannot, by construction, as long as the fold stays in-memory.** The chain
of facts:

1. The transcript table only ever stores the **raw** prompt — `store_msg` at
   exec.rs:525 writes `{role:user, text:p}` with no block text, once, before
   assembly.
2. `transcript_rows` (exec.rs:1524) re-reads that raw table every turn.
3. The user blocks come from the **seed** (`render_parts`/the new user loader),
   which is recomputed and **never persisted to the transcript**.
4. The fold mutates only the **in-memory** assembled doc, at the end of
   `assemble_detailed`.

So turn N+1 re-reads the raw prompt and re-folds fresh from the seed — the
folded text from turn N was never written back, so nothing compounds. **The hard
rule for the implementer: the fold must live inside assembly and touch only the
in-memory `Doc`; nothing may `store_msg` the folded text.** A regression test
pins this (M1 acceptance).

### (d) Shipping user-placement *defaults* from block-file frontmatter — DEFER

**Decided: not in the core; gated as M3, droppable.** For the first cut, a
`user` block is created only at runtime via `elanus block set … --placement
user` (the write surface already exists, blockcli.rs). A profile *shipping* a
`user`-placement default block is a real want, but it drags in real complexity:
`render_parts`' static-`blocks/*.md` fallback path (render.rs:99–120) assumes
every file is a **system** part, and the durable-name dedup (render.rs:104) is
checked against the system durable set only. To ship a user default correctly
that path must become placement-aware (parse `placement` from the file's JSON
frontmatter, route the file into the user seed, dedup against the *user* durable
set, and teach `seed_defaults` to honor placement instead of forcing System at
line 544). That is a separable concern from "give user blocks a render home," so
it is isolated in M3 and can be dropped without touching M1/M2.

### (e) Coding-agent parity — DEFER, with justification

**Decided: defer, document as residual.** Two reasons:

1. **Placement is semantically moot in the coding path.** A coding agent has no
   `Doc`/system split; `turn_injection` (codeagent.rs:3200) renders *all* of its
   blocks into one **next-turn user-side** injection string already. So every
   block a coding agent sees is effectively "user placement" today. Widening
   `load_session_blocks` (context_store.rs:171, `placement='system'` at line
   185) to also load `user` rows would only make user-placement blocks *visible*
   to coding sessions — it would not create a new "level," because there is only
   one level there. That is a small, optional visibility fix, not parity.
2. **`src/codeagent.rs` has active sibling edit claims right now** (two other
   sessions). Touching it invites a shared-tree collision for near-zero
   semantic gain. Keep this handoff entirely off `codeagent.rs`.

Residual pointer left for later: if user-placement blocks *should* reach coding
sessions, widen `load_session_blocks` to `placement IN ('system','user')` — a
one-line change once the sibling edits settle.

## Milestones

### M1 — A render home for user-placement blocks (the core)

Give `placement=user` durable blocks a real render path: a structured user
region on the document, seeded pre-chain and folded into the trailing user
message post-chain.

Work:
1. **`src/context.rs`** — add `#[serde(default)] pub user: Vec<Block>` to `Doc`.
   Serde-default keeps it backward-compatible: existing stage scripts round-trip
   the whole JSON dict (they preserve the key), and the Rust type tolerates its
   absence on input.
2. **`src/context_store.rs`** — add `load_user_blocks(conn, prof, session)`
   returning `placement='user'` rows, priority-ordered, under the same
   visibility predicate as `load_system_blocks`. Refactor the shared,
   dedup-on-read SQL into one private helper parameterized by placement, with
   `load_system_blocks`/`load_user_blocks` as thin wrappers (the two bodies are
   already near-identical — keep the dedup logic in one place).
3. **`src/render.rs`** — add `render_user_parts(root, conn, profile, session) ->
   Vec<(String, String)>` returning the priority-ordered user durable blocks.
   User blocks come from durable rows only in this cut (no static-file,
   provider, or skills-inventory contribution — those are system concepts).
   Leave `render_parts` (the system seed) byte-for-byte unchanged.
4. **`src/context.rs`** — thread a `user_seed: &[(String, String)]` param through
   `assemble` and `assemble_detailed`. Seed `doc.user` from it **before** the
   stage loop. After the loop (before returning), **fold**: find the last
   `role:user` message in `doc.messages`, splice the joined user-block text
   (`\n\n`-joined, blocks-then-original) into it, then set `doc.user = vec![]`
   so the final doc has exactly one home for the text. Run a final `validate()`
   (folding only edits an existing user message's text, so it is
   adjacency-preserving by construction — the validate is a cheap guard).
5. **`src/exec.rs`** — compute the user seed once per run next to `render_parts`
   (line 540) and pass it to both `assemble` (line 632) and the `assemble_
   detailed` in `render_context` (line 104). Nothing about `store_msg` or
   `transcript_rows` changes — the raw prompt stays raw (decision c).
6. Update the other callers of `assemble`/`assemble_detailed` to pass an empty
   user seed: the tests at context.rs:639, :789, :863 and exec.rs:2578.

**Acceptance:**
- `elanus block set scratch "hot notes" --placement user` then `elanus context
  render <profile> <session>`: "hot notes" appears folded into the **trailing
  user message** of `document.messages`, and does **not** appear in
  `document`'s system text. A `placement=system` block still renders in system
  text as before.
- The wire never carries two consecutive user messages: after folding there is
  exactly one user message where the prompt was.
- **Double-injection guard:** assembling twice from the *same* raw
  `transcript_rows` and the *same* user seed yields identical folded output (no
  compounding); the folded block text is never written to the `messages` table.
  A unit test pins both (assemble is pure over its inputs; the transcript it is
  handed is unchanged).
- The golden-parity test (context.rs:604) is **unchanged** — an empty user seed
  produces byte-identical output. `render.rs` block tests (429/464) unchanged.
- New tests mirror the existing ones: a `load_user_blocks` visibility/order test
  in `context_store.rs` (pattern of :700/:797), and a fold test in `context.rs`
  (pattern of :569/:810). `cargo test` green.

### M2 — Guidance: make "prefer system" the loud default

The mechanism is neutral; the guidance must steer. An agent that reaches for
`--placement user` by reflex just pays duplicate tokens every turn.

Work:
- **`docs/context.md`** — document the two levels, the caching/duplicate-token
  trade, and the rule: *default to `system`; choose `user` only for a block that
  changes almost every turn.*
- **`src/blockcli.rs`** — say the same, tersely, where an agent actually meets
  the choice: in the `--placement` help text, and as a one-line stderr note when
  a write uses `--placement user` (e.g. "note: `user` placement re-sends this
  block every turn; prefer `system` unless it changes each turn"). Keep it to a
  sentence; do not gate or refuse the write (homogeneous authority — this is
  advice, not a fence).

**Acceptance:** `docs/context.md` explains both levels and the prefer-system
rule; `elanus block set --help` states the trade-off; a `--placement user` write
prints the one-line steer to stderr (asserted by a light test on the emitted
string). No behavior change to the write itself.

### M3 (optional, gated — droppable) — Ship user-placement defaults from frontmatter

Let a profile *ship* a `user`-placement default block, so a profile can preload
the user shelf, not just runtime `set`s. Only build this if M1+M2 land clean and
there is appetite; it is separable and its own risk.

Work: make `render_parts`' static-`blocks/*.md` path placement-aware — parse an
optional `"placement"` key from the file's JSON frontmatter (`parse_block_front`
already returns the meta object, render.rs:193), route a `user` file into the
user seed instead of the system parts, make the durable-name dedup check the
*user* durable set for user files, and teach `seed_defaults` (context_store.rs:
533) to honor `block.placement` from the default tuple instead of forcing
`System` at line 544 (thread placement through the `defaults: Vec<(String,
String, i32, Value)>` shape — likely add a placement element).

**Acceptance:** a profile `blocks/NN-name.md` with frontmatter
`{"placement":"user"}` seeds a `user`-placement row on first render and folds
into the user turn; a plain (no-placement) block file still renders as system; a
later `elanus block set` on the same name evolves it (stored-wins) and survives a
re-render. System-only profiles are byte-identical to today.

## Residuals (honest list)

- **`before_messages` / `after_messages` / `scratch`** — still unimplemented.
  The `context_blocks` table stores them and the CLI accepts them, but they have
  no `Doc` home. Out of scope here by Tim's instruction; a future handoff can add
  regions the same way M1 adds the user region.
- **Coding-agent parity** — deferred (decision e). One-line follow-on when the
  `codeagent.rs` sibling edits settle: widen `load_session_blocks` to
  `placement IN ('system','user')` so user blocks are at least visible to coding
  sessions.
- **Frontmatter-shipped user defaults** — M3, droppable (decision d). If dropped,
  user blocks are runtime-created only.
- **Per-stage build-log attribution for user-block mutations** — the build log
  (`log_block_mutations`, context.rs:365) diffs `doc.system` only. Extending it
  to diff `doc.user` so a stage rewriting a user block is attributed is a natural
  later add; not required for the render home.
- **MCP `block` tool wrapper** — still deferred (parent handoff decision 1),
  unchanged by this work.

## Log

- 2026-07-03 — Planned by Opus/high in the handoff workflow. Grounded against
  `context.rs`, `render.rs`, `context_store.rs`, `exec.rs`, `blockcli.rs`,
  `context_blocks.rs` (all read, not remembered). Key findings that shaped the
  plan: (1) `build_request` maps one `ChatMessage::user` per `role:user` row and
  `validate()` does not forbid consecutive users, so a separate user message is a
  latent wire-400 — hence fold, not append (decision a). (2) The transcript
  stores only the raw prompt (`store_msg`) and is re-read raw each turn, so an
  in-memory fold cannot compound (decision c). (3) `render_parts`' static-file
  path assumes system, which is why shipping user *defaults* is isolated in the
  droppable M3 (decision d). (4) The coding-agent path injects everything into
  the user turn already, and `codeagent.rs` is under active sibling edits, so
  parity is deferred (decision e).

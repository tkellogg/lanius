---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-23
---

# Memory blocks

Make **memory blocks** a first-class, built-in part of the context pipeline:
named, durable, agent-editable chunks of prompt that any profile gets for free,
with a default value that *evolves* over time. This is the keystone the
profiles journey leans on — inter-agent comms and work-estimation are both just
packages that write blocks (see [agent-comms-package.md](agent-comms-package.md)
and [work-estimation.md](work-estimation.md)).

The guiding design (Tim, in the planning chat):

- Blocks are **plain key-value data in the document** — `name → content`. No
  special "computed block" type.
- **block→text rendering is built into assembly**, run once at the end. Stages
  only ever push/edit entries.
- A **computed block is therefore just a vanilla stage** that writes an entry. A
  downstream stage sees the merged map with **no distinction** between computed,
  static, or persisted blocks — they all entered at different points in the
  deterministic stage order.
- The *mechanism* lives in the **kernel** so it "feels built-in"; packages and
  profiles only supply *content*.

## What already exists (read before assuming)

- The context pipeline already models blocks as kv. `src/context.rs`:
  `Doc { v, system: Vec<Block>, messages, event, meta }` (line 46) where
  `Block { name, text }` (line 24), and `Doc::system_text()` (line 55) is the
  built-in renderer (joins blocks with `\n\n`). Stages are `Doc -> Doc`
  transforms — `chain()` (line 82), `assemble_detailed()` (line 210),
  `run_stage()` (line 352), wire `validate()` (line 425). The native system seed
  comes from `src/render.rs::render_parts()` (line 29): profile `blocks/*.md`
  files + provider outputs + skills inventory.
- The **durable substrate exists but is unwired.** `src/context_blocks.rs`
  defines `ContextBlock { name, content, placement, priority, owner, scope,
  package, meta }`, `Register`, `BuildLogRecord` — its own header says *"the
  current prompt path still assembles the legacy `context::Doc`; this module is
  the substrate for moving context assembly toward named, hashable blocks."*
  `src/db.rs` has the backing tables: `context_blocks` (line 288, `UNIQUE(scope,
  owner, session_id, run_id, name)`) and `context_build_log` (line 311). **No
  production code writes or reads these tables** (the only INSERTs are in db
  tests).
- The **only live "memory" today** is `code_notes` (`src/db.rs` line 406): one
  plain string per coding session, set via `elanus code note`, read each turn by
  `turn_injection()` and surfaced in the per-turn injection. Its own schema
  comment says *"the full context_blocks substrate integration is deferred."*
  This is the thing memory blocks generalize and eventually replace.

So the work is mostly **bridging the unwired substrate into the live Doc path**,
plus a write surface and the coding-agent projection — not a greenfield build.

## Decisions to confirm (the wonky bits)

1. **Write surface = CLI verb first, MCP tool as the ergonomic wrapper.**
   `elanus block set/get/list/append/rm <name> …` is the universal surface —
   every harness can shell out to it exactly like `elanus code note` does today.
   An elanus MCP tool (elanus already runs an MCP server, `src/mcp.rs`) is the
   "feels-like-a-real-tool" upgrade, since all three harnesses speak MCP. Build
   the CLI in M2; the MCP wrapper can follow. *Confirm before building the MCP
   half.*
2. **M1 honors `system` placement only.** `Doc` carries `system` + `messages`;
   the richer placements (`before_messages`/`after_messages`/`user`/`scratch`
   from `context_blocks.placement`) have no Doc home yet. Start with `system`
   (the common case, matches today), order by `priority`, and grow the other
   placements later in the same model. The table column already stores all five.
3. **"Default that evolves" = seed-once, stored-wins.** A package/profile ships a
   default (a `blocks/<name>.md` or a manifest declaration). On first use the
   default seeds a `context_blocks` row; every read thereafter returns the stored
   (possibly agent-edited) row. The default is a *fallback*, never an overwrite.
4. **Multi-writer is owner-scoped, not locked.** `context_blocks` is keyed by
   `(scope, owner, session_id, run_id, name)`. Keep writes last-writer-wins per
   key and lean on identity (`owner`) + scope for isolation rather than building
   locking. A peer writing "your" block writes a *different* `owner` row; the
   renderer merges by placement+priority. (Aligns with the homogeneous-authority
   stance — no trust boundary between an owner's own agents.)
5. **Migrate `code_notes`, don't break it.** The per-turn memory note becomes one
   well-known block (`note`, session scope). Keep `elanus code note` working as a
   thin alias so the coding-agent injection path and its tests don't regress;
   migrate the read in `turn_injection()` to the block store.

## Milestones

### M1 — Durable blocks seed the native Doc (the renderer is built-in)
Wire `context_blocks` rows into assembly. Add a kernel seed step (in
`render.rs`/`context.rs`, before the stage chain) that loads the blocks visible
to `(scope, owner, session)` for the profile, ordered by `priority`, and appends
them to `Doc.system` as `Block { name, text }`. Rendering stays
`Doc::system_text()` — no per-stage rendering. No new agent capability yet.

**Acceptance:** insert a `context_blocks` row (scope=agent, owner=`<agent>`,
name=`identity`, placement=system); `elanus context render <profile> <session>`
(`render.rs::render`, line 16) shows its content in the system text, positioned
by `priority` relative to the profile's static `blocks/*.md`. `cargo test`
green; wire `validate()` still passes.

### M2 — The write surface + default-that-evolves
Add `elanus block set/get/list/append/rm <name> [--scope …] [--placement …]
[--priority …]`, owner = the caller's broker-verified identity, upserting
`context_blocks`. Add the seed-once default path: a profile `blocks/<name>.md` or
a package manifest default seeds a row on first render if none exists; a `set`
thereafter wins. Re-point `elanus code note` at the `note` block (alias, M-decision 5).

**Acceptance:** `elanus block set identity "I am Lily."` persists; next
`context render` shows it. A shipped default block appears on first render, and a
subsequent `set` overrides it (and survives a re-render — it *evolved*).
`elanus code note <session> "x"` still works and shows up as before.

### M3 — Computed block = a vanilla stage (prove the uniformity)
Demonstrate the core claim: a package `[[stage]]` that writes a block is
indistinguishable downstream from a static/persisted one. Ship one tiny example
stage (e.g. a `clock`/`status` block) that adds an entry to `doc.system`, and a
trivial downstream stage that reads `doc.system` and proves it sees the computed
entry with no special-casing. Emit a `context_build_log` row per block mutation
(the table at `db.rs:311` is ready) so "which component added/edited which block"
is reconstructable.

**Acceptance:** with the example package approved, `context render` shows the
computed block; a downstream stage observes it via `doc.system` exactly like any
other block; `context_build_log` has an `add` row attributed to the stage's
package. The two stages share no block-specific code path.

### M4 — Coding-agent projection + placement→injection-vector
Project blocks into the coding-agent injection seam so blocks reach Claude
Code / Codex / opencode too — the "both, one substrate" goal. A block's
placement/priority picks an **injection vector** via a per-harness capability
matrix (the harness-modes per-(harness,capability) pattern), degrading down a
ladder when a harness can't do the requested vector:

- **next-turn** (normal): extend `turn_injection()` (`codeagent.rs`, the
  `UserPromptSubmit`/`SessionStart` `additionalContext` path at line 5571) to
  render visible blocks, not just inbox+note. Works on all three today.
- **mid-cycle** (high priority): emit `additionalContext` on **`PreToolUse`/
  `PostToolUse`** for Claude Code (the hooks are *already registered* — see the
  read-camera/auto-claim arms at `codeagent.rs:5536`/`5550` — they just don't
  emit context yet); `POST /session/{id}/prompt_async` for opencode; **Codex
  degrades to next-turn** (no live hook bridge).
- **algedonic** (drop-everything): opencode `POST /session/{id}/abort` + inject
  is a true interrupt; Claude Code lands at the next tool boundary; Codex → next
  resume. Maps onto the existing `signal/` plane.

This milestone is the coding-agent half and the biggest; M1–M3 are a shippable
native keystone on their own. **The mechanism is de-risked** — see the spike in
the Log.

**Acceptance:** a normal-priority block lands next-turn in a live Claude Code
session; a high-priority block lands **mid-turn** (between tool calls) in Claude
Code and opencode; a high-priority block in a Codex session degrades to
next-turn with a logged, legible "downgraded vector" rather than an error or
silent drop.

## Read these first
- The why: [../journeys/11-profiles.md](../journeys/11-profiles.md) ("Memory
  blocks" and "Additive").
- The pipeline: [../context.md](../context.md) (esp. increment 6 / the
  `context_blocks` substrate) and `src/context.rs`.
- The unwired substrate: `src/context_blocks.rs`, `src/db.rs` lines 285–328.
- The thing we generalize: `code_notes` (`src/db.rs:406`), `set_note`/`get_note`
  (`src/codesession.rs:470`/`493`), `turn_injection()` and the hook bridge
  (`src/codeagent.rs:5463`).

## Log
- **2026-06-23 — cross-harness injection spike (run live, this session).** The
  load-bearing question for M4 — "can elanus inject context *mid-turn*?" —
  answered empirically per harness:
  - **Claude Code: YES.** A `PreToolUse` and a `PostToolUse` hook each returning
    `hookSpecificOutput.additionalContext` both reached the model mid-turn (it
    echoed both sentinels back, labeled by hook). Vector confirmed; the hooks are
    already registered, just not emitting context.
  - **opencode: YES (richest).** Live HTTP control plane: `POST
    /session/{id}/prompt_async` ("send a new message… starting the session if
    needed"), `POST /session/{id}/abort` (true interrupt), `POST
    /tui/append-prompt` (drives the attached TUI input — elanus's opencode cell).
  - **Codex: not on the supported path.** Hooks are a confirmed dead end;
    `codex exec resume` is turn-boundary only; the experimental
    `app-server`/`remote-control` control socket is a possible-but-unproven
    future vector elanus does not drive. → Codex degrades to next-turn.
  This is why M4's vector ladder has a graceful-degradation rung for Codex.
- **2026-06-23 — planning.** Collapsed the journey's "inter-agent comms" into
  "just blocks" per Tim: priority is a property of a *block* (placement→vector),
  not a separate comms pipeline. Comms and estimation become packages on this
  substrate, not kernel work.

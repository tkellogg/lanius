---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: kb-search — the `[[tool]]` seam, the FTS daemon, the search tool

Decomposed from [knowledge-base.md](knowledge-base.md) build step **B2** (ruling
D3). Depends on [kb-core.md](kb-core.md) (the `[kb]` marker + `kb/` convention
must exist to index). Ships two things:

1. **M0 — the minimal `[[tool]]` manifest seam** (kernel): a package declares a
   tool definition (name, description, JSON schema, run script); granted +
   visible packages' tools enter the agent's tool array; dispatch is exec-mode
   (args JSON on stdin → result JSON on stdout, the `[[stage]]` contract). This
   is the same move `[[stage]]`/`[[harness]]` made, ruled by Fable 2026-07-02
   over my original MCP routing (see wonky bit 1).
2. **The `kb-search` package**: a **read-only daemon** indexing the union of
   every enabled package's `kb/` into a **package-local sqlite FTS5** index (its
   own state dir, **zero kernel schema**), supplying **`search_knowledge`**
   through the new seam as the primary agent surface, with a skill and `elanus
   kb search` CLI alongside. Re-index on package enable/disable.

The read-before-write ordering (D3) means this lands right after kb-core: it
proves the KB's value with zero write risk. The out-of-scope multi-vector engine
(D3) is the **swap-proof future**: an alternative package declaring the **same**
`[[tool]] name = "search_knowledge"` with an embedding engine — the swap must be
invisible to agents, and this handoff's acceptance proves it with a toy second
engine.

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **THE SEAM — a package cannot supply a tool *definition* today; M0 builds the
   `[[tool]]` manifest seam. [FABLE RULING 2026-07-02 — overrules my MCP call.]**
   This is the gap the design (D3) anticipated. Verified against the tree:
   `provides_builtin_tools` (`src/manifest.rs:56`) only **gates availability** of
   the 7 tools hardcoded in `exec::tool_defs` (`src/exec.rs:1529-1666`: `shell`,
   `emit_event`, `schedule_event`, `fs_lease`, `send_message`, `ask_human`,
   `launch_agent`; gating in `withheld_builtin_tools`,
   `src/packages.rs:195-219`). A package **cannot** introduce a new tool name +
   schema + dispatch.

   My original call routed `search_knowledge` through MCP
   (`kb__search_knowledge`). **Fable overruled it on doctrine:** `docs/mcp.md:1-11`
   is explicit that MCP is a **border protocol for third-party tool servers**
   and "**first-party mechanisms never speak it**" — a stock stdlib capability
   must not sit on the wrong side of that border. The ruled design is the
   **`[[tool]]` seam**:

   - **Declaration:** `[[tool]]` in `elanus.toml` carries `name`, `description`,
     the input **JSON schema** (inline TOML mapped via `manifest::toml_to_json`,
     or a sibling file named by path), and `run` (package-relative script).
     Mirror `StageDecl` (`src/manifest.rs:203-219`): name validated as one topic
     level (and not colliding with a kernel builtin), `timeout_ms` with a
     default, fail semantics declared. The `run` script joins the `code_hash`
     fold (`src/manifest.rs:426-449`) so an edit re-enters review.
   - **Authority: grant-gated like everything else.** The declaration IS the
     request — a new grant kind `"tool"`, registered exactly where
     `[[stage]]`/`[[mcp]]` append theirs (`src/packages.rs:302-309`). The tool
     enters no agent's array until the human approves it into the grants ledger.
     (A risk badge on the approval surface probably follows later — noted, not
     built here.)
   - **Availability:** the kernel folds **approved** packages' tool defs into
     the agent's tool array alongside `exec::tool_defs`, for profiles that can
     **see** the package — the same visibility gate `provides_builtin_tools`
     rides today.
   - **Dispatch: exec-mode, the `[[stage]]` contract.** On a tool call, the
     kernel spawns the package script with the call args as **JSON on stdin**;
     **stdout JSON becomes the tool result** — mirror `run_exec_stage`
     (`src/context.rs:429-476`) precisely: piped stdin/stdout with a writer
     thread (no pipe-buffer deadlock), the declared `timeout_ms` budget, kill on
     deadline, nonzero exit = a tool **error result** the model sees. A failing
     tool degrades that one call loudly; it does not corrupt the run.
   - **Bare name, no prefix:** the tool is `search_knowledge`, period.
   - **Collision rule (my lean, stated as ruled scope): REFUSE at approve time,
     loudly.** When approving a package's `[[tool]]` grant would create a second
     enabled+approved holder of the same tool name (or shadow a kernel builtin),
     the approval **fails with a message naming the current holder**; the human
     disables/revokes one first. Deterministic and auditable — and exactly the
     swap ergonomic: disable the old indexer, enable + approve the new one,
     agents never see two engines racing. *Confirm the approve-time refusal (vs
     first-hit-wins shadowing, which is silent).*

   This makes Tim's swappability requirement literal: a third-party indexer
   package declares the **same** `[[tool]] name = "search_knowledge"` and the
   swap is invisible to agents.

2. **Indexer topology: daemon indexes, tool script reads.** `kb-search` declares
   **both** `[process] mode = "daemon" run = "scripts/index"` (builds/refreshes
   the FTS5 index into the package's state dir) **and** `[[tool]] name =
   "search_knowledge" run = "scripts/search"` — a short-lived exec-mode script
   that opens the FTS sqlite **read-only** per call and answers on stdout. The
   `history` package is the read-only-query-daemon precedent
   (`kits/stdlib/packages/history/`, HTTP + `mode=ro` sqlite,
   `scripts/main:177-207` LIKE search) — but its surface is **HTTP + CLI, not a
   model tool**, which is exactly the gap the `[[tool]]` seam closes.

3. **Re-index trigger: poll vs watch.** The daemon must pick up a newly
   enabled/disabled kb-carrying package. Two options: watch the bus for
   `obs/config/changed` (emitted by the config-proposal accept path,
   `src/exec.rs:307-326`), or poll `packages::discover` (`src/packages.rs:67`)
   every N seconds and diff the enabled kb set. **My call: poll (simplest,
   robust to missed events), upgrade to watch later** if latency matters. D3
   says "pick simple."

4. **Search authority (design wonky bit 4): world-readable within the
   instance.** A KB is curated shared knowledge; the whole point is availability
   (homogeneous authority). The tool does **not** gate by correspondent the way
   `recall` does (recall's content is conversations). Sensitive material stays in
   `notes/`/blocks, not a KB. **My call: follow the design lean — world-readable
   within the instance.** *Confirm — this is the one place D2's "no trust
   boundary" bites if a KB ever holds a secret.*

5. **Index what, keyed how.** One FTS5 table over the union, columns roughly
   `(package, path, line_start, line_end, chunk)` — chunk per heading/section so
   a hit returns a file + line range an agent can open. `search_knowledge`
   returns ranked `{package, path, lines, snippet}`. Nothing kernel-side; the db
   is `<pkg-state-dir>/kb-index.sqlite`.

## Milestones

### M0 — the `[[tool]]` manifest seam (kernel)
Build the seam per wonky bit 1: `ToolDecl` in `src/manifest.rs` (mirror
`StageDecl` — name/description/schema/run/timeout_ms, validation, `code_hash`
fold), grant kind `"tool"` registered beside stage/mcp
(`src/packages.rs:302-309`), approve-time collision refusal, kernel folding of
approved+visible packages' tool defs into the agent tool array, and exec-mode
dispatch mirroring `run_exec_stage` (`src/context.rs:429-476`). Document the
seam where `[[stage]]` is documented.

**Acceptance:** a toy package declaring `[[tool]] name = "echo_args"` is
invisible to agents until approved; once approved + visible, the tool appears in
the agent's array with its declared schema and a call round-trips args→stdin,
stdout→result; a script that exits nonzero or overruns `timeout_ms` yields a
legible tool error, not a wrecked run; editing the script detaches the grant
(hash test, mirroring `editing_run_script_detaches_grants`,
`src/manifest.rs:595`); approving a second package with the same tool name is
**refused loudly**, naming the holder. `cargo test` green.

### M1 — the `search_knowledge` tool against a hand-built index
Stand up the `kb-search` package with `[[tool]] name = "search_knowledge" run =
"scripts/search"` (`query`, optional `limit`), backed by an FTS5 sqlite built by
hand for this milestone (a fixed fixture index over kb-core's
`kb-llm-strengths`). The script opens the index read-only.

**Acceptance:** an agent given **only** the `search_knowledge` tool (no other
context) answers "who verifies?" from a cold start, returning the file + line
from `kb/role-verifier.md`. The tool script opens the index read-only; a
kill-and-restart of the agent session leaves the index intact.

### M2 — the indexing daemon over the union of enabled kb/
Add `[process] mode = "daemon" run = "scripts/index"` that discovers every
enabled package carrying `[kb]` (`packages::discover`), reads each `kb/` file,
chunks it, and (re)builds the FTS5 index in the package state dir. Read-only over
the corpus (it only writes its own index). Idempotent rebuild from files.

**Acceptance:** the daemon builds the index from the corpus on the disk with no
kernel schema touched; killing and restarting the daemon reproduces an
equivalent index (kill-and-restart safe); the daemon never writes a `kb/` file.

### M3 — re-index on package-set change + CLI + skill + THE SWAP PROOF
The daemon re-indexes when the enabled kb set changes (poll — wonky bit 3). Add
`elanus kb search <query>` (a `KbCmd::Search` on the `Cmd::Kb` added in kb-core)
that queries the same index and prints ranked file+line hits, and a `kb-search`
skill teaching the tool + CLI (journey-14 availability tiers). Then **prove the
swap**: a toy second indexer package (e.g. plain-grep engine) declaring the
**same** `[[tool]] name = "search_knowledge"`.

**Acceptance:** enabling **another** kb-carrying package (e.g. a scratch
`discord` package shipping `kb/discord-api-notes.md`) makes its content findable
by `search_knowledge` on the next index pass; `elanus kb search` returns the
same hits as the tool; the skill's frontmatter makes the tool's existence
high-availability while its detail stays expando; **the swap proof:** with
`kb-search` disabled and the toy engine enabled + approved, an agent's tool
array carries `search_knowledge` unchanged (same name, same schema shape) and a
query still answers — the engine swap is invisible to the agent; with **both**
enabled, approving the second is refused loudly (M0's collision rule, asserted
end-to-end).

## Explicitly out of scope
- **The multi-vector engine.** Named as the swap-proof future: an *alternative*
  package you install instead of `kb-search`, declaring the same `[[tool]] name
  = "search_knowledge"` with an embedding-based engine. Building it needs
  embedding setup and is its own handoff. This handoff's job is to make that
  swap mechanically real (M0 + M3's toy proof), not to build the second engine.
- **The resident-stage auto-surface rung** (expensive, always-on) stays deferred
  (D3): start pull-only.
- **A resident dispatch mode for `[[tool]]`** (the bus-consult shape stages have,
  `src/context.rs:415-421`): exec-mode only in this increment; add resident when
  a tool actually needs held state.
- **A risk badge on tool-grant approval** (noted in wonky bit 1): later.

## Read these first
- The ruling: `docs/mcp.md:1-11` (MCP is a border protocol; **first-party
  mechanisms never speak it**) — why the seam is `[[tool]]`, not MCP.
- The settled design: [knowledge-base.md](knowledge-base.md) D3 (package daemon,
  tool-first, default FTS, indexer swappable), D7 (the union feels like one KB),
  wonky bit 4 (search authority), build step B2.
- **The seam's precedents, verified:** `src/manifest.rs:203-219` (`StageDecl` —
  the decl shape to mirror), `src/packages.rs:302-309` (a decl IS the grant
  request — stage/mcp registration), `src/context.rs:429-476` (`run_exec_stage`
  — the exec dispatch contract: stdin/stdout JSON, writer thread, timeout, fail
  semantics), `src/manifest.rs:426-449` (`code_hash` fold) + the detach test
  `:595`.
- **What exists today (the gap):** `src/manifest.rs:47-57`
  (`provides_builtin_tools` GATES only), `src/exec.rs:1529-1666` (`tool_defs` —
  the 7 kernel tools), `src/packages.rs:195-219` (`withheld_builtin_tools`).
- The dependency: [kb-core.md](kb-core.md) (the `[kb]` marker + `kb/`
  convention).
- The read-only-query-daemon precedent: `kits/stdlib/packages/history/`
  (`elanus.toml` `mode=daemon`/`http=true`, `SKILL.md:16-73` the DSL,
  `scripts/main:177-207` LIKE search over `mode=ro` sqlite).
- The consumer of the same seam: [kb-discovery.md](kb-discovery.md) (its
  privileged tool rides `[[tool]]` too).

## Log
- 2026-07-02 — Decomposed from knowledge-base.md B2 by Opus (planner) under
  Fable. Grounded against the sprint-4 worktree: **confirmed packages cannot
  supply tool definitions** — `provides_builtin_tools` only gates the 7
  hardcoded `exec::tool_defs`. No FTS5 exists anywhere in the tree today, so the
  index is built from scratch. `history` is the read-only-query-daemon pattern
  but its surface is HTTP+CLI, not a tool. Original judgment call: route
  `search_knowledge` through MCP (`kb__search_knowledge`).
- 2026-07-02 (later) — **Fable's ruling folded in: MCP routing OVERRULED.**
  `docs/mcp.md`'s doctrine (border protocol; first-party mechanisms never speak
  it) puts a stock stdlib capability on the wrong side of the border. Replaced
  with the `[[tool]]` manifest seam as a new **M0**: decl mirrors `StageDecl`,
  authority is a grant kind `"tool"` registered like stage/mcp, dispatch mirrors
  `run_exec_stage`'s exec contract, name is bare `search_knowledge`. Collision
  rule decided: refuse at approve time, loudly (lean confirmed by Fable's
  prompt). M3 now carries the end-to-end swap proof (a toy second engine with
  the same tool name). Remaining judgment calls: approve-time refusal wording
  (1), daemon-indexes/tool-reads topology (2), poll to re-index (3),
  world-readable (4).

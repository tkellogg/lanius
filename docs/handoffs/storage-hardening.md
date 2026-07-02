---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: storage hardening — the duplicate-block race and the tick-loop scan

Two real bugs from the scaling/storage research probe
([../notes-scaling-and-storage.md](../notes-scaling-and-storage.md), sections
1–2), both measured, both small and local. (1) The memory-block upsert
(`src/context_store.rs:370`) is a non-transactional read-then-branch, and the
table's `UNIQUE(scope, owner, session_id, run_id, name)` constraint
(`src/db.rs:319`) is **inert for every scope except `run`** because SQLite
treats NULL as distinct in a UNIQUE index — concurrent writers to the same
agent/global/session-scope key silently produce **duplicate rows** (5–10
measured at N=10, 30 at N=100), and the read path
(`load_system_blocks` `src/context_store.rs:106`, `load_session_blocks` `:158`)
injects **every duplicate into the prompt** (plain `SELECT … ORDER BY priority,
name, id`, no dedup). (2) `expire_deadlines` (`src/dispatcher.rs:701`) full-
scans the `events` table on **every 1s tick** — its `WHERE` leads with
`e.type = ?1` and no equality on `state`, so neither `idx_events_pending
(state, type, priority)` nor the partial announce index applies (`src/db.rs:
173-175`); measured 46µs at 1k rows → **9.3ms at 200k**, linear in ledger
*age*, unconditionally, forever.

## Wonky bits / decisions to confirm

1. **Fix (1) with the sentinel migration, not just a transaction.** The
   research doc weighs both (its section 2 fix list); I pick the sentinel:
   backfill `session_id`/`run_id` NULL → `''` and bind `''` instead of NULL in
   `scope_binding`/all block code. Rationale: (a) the existing table-level
   UNIQUE constraint then simply **starts firing** — no table rebuild, no new
   index, the schema is already right once the values are non-NULL; (b) it
   lets `upsert_block` become one native `INSERT … ON CONFLICT(scope, owner,
   session_id, run_id, name) DO UPDATE …` — atomic in the engine, no TOCTOU
   window at all; (c) a transaction-only fix leaves the constraint neutered, so
   any *future* writer that bypasses `upsert_block` re-opens the hole — the DB
   should be the source of truth for uniqueness, not app discipline. The
   `BEGIN IMMEDIATE` transaction still comes along for free: the upsert + its
   `context_build_log` row (`write_build_log` call at `:434`) should commit
   together anyway (today a crash between them logs nothing for a landed
   write). *Fable: confirm sentinel-over-transaction-only; the doc itself
   leans (2) "probably the better long-term fix".*

2. **Migration order matters: dedupe FIRST, then backfill.** Existing
   databases already contain duplicate rows (any instance that ever had
   concurrent block writers). Backfilling NULL → `''` on a table with
   duplicates **violates the newly-live UNIQUE constraint mid-migration**. So
   the migration (in `db.rs`'s open/migrate path, beside the existing ALTER
   patterns): (a) for each `(scope, owner, ifnull(session_id,''),
   ifnull(run_id,''), name)` group keep the row with the greatest
   `updated_at`, tie-broken by greatest `id` (the last writer — honoring the
   documented last-writer-wins contract, `context_store.rs:368`), delete the
   rest; (b) then `UPDATE context_blocks SET session_id='' WHERE session_id IS
   NULL` (and same for `run_id`). Log the deleted-duplicate count to stderr so
   an operator sees the heal happen. *Fable: confirm keep-latest as the dedupe
   winner (it's what an uncontended last-writer-wins would have produced).*

3. **Dedup-on-read stays as a belt-and-suspenders guard.** Even after the
   migration, the read path should collapse duplicates by logical key (keep
   the max-`id` row per `(scope, owner, session_id, run_id, name)`) — one
   `GROUP BY`/window clause or a small Rust-side fold in `load_system_blocks`
   /`load_session_blocks`. Why keep it if the constraint now fires: a DB
   restored from a pre-migration backup, or an attach of an old `elanus.db`,
   must never render the same block 2–30 times into a prompt again — the
   prompt is the blast radius here, and the guard is cheap. *Fable: confirm
   keeping the read guard permanently vs treating it as migration-window-only.*

4. **Fix (2) is one index; keep the query shape.** Add `CREATE INDEX IF NOT
   EXISTS idx_events_type_deadline ON events(type, deadline)` beside the
   existing indexes (`src/db.rs:173-175`). The query's leading `e.type = ?1
   AND e.deadline IS NOT NULL AND … deadline < now` rides it directly; the
   `NOT EXISTS` subquery already rides `idx_events_correlation`. The doc's
   alternative (restructure the NOT EXISTS) is more surgery for the same
   result — rejected. `CREATE INDEX IF NOT EXISTS` in the schema block is
   also the migration (it builds on existing DBs at next open). *Fable:
   confirm index-only.*

5. **Scope discipline: this handoff is these two fixes, nothing else.** The
   research doc's other findings (single-writer insert throughput, broker
   fan-out, Dolt) are explicitly non-urgent or no-change — do not "improve"
   them here. The doc's section 3(c) feeds
   [knowledge-base.md](knowledge-base.md), not this handoff.

## Milestones

### M1 — Sentinel migration + native atomic upsert
- Migration in the `db.rs` open path (wonky bit 2): dedupe keep-latest, then
  backfill `''`; idempotent (re-running is a no-op).
- `scope_binding` (`src/context_store.rs:378` call site) and every block
  read/write binding NULL for unbound `session_id`/`run_id` switches to `''`;
  the `session_id IS NULL OR session_id = ?` visibility predicates
  (`context_store.rs:116`, `:168`, and `get_block`/`exists`'s `IS`-comparisons
  referenced by the comment at `:382-387`) become their `''`-aware
  equivalents.
- `upsert_block` (`:370-448`) becomes `INSERT … ON CONFLICT(scope, owner,
  session_id, run_id, name) DO UPDATE SET …` inside one `BEGIN IMMEDIATE`
  transaction with its `write_build_log` row (`:434`); the stale comment at
  `:382-387` (which documents the NULL workaround) is replaced by one
  explaining the sentinel. The before-sha for the build log comes from the
  same transaction (read inside it, or use the UPDATE's returning/changes to
  distinguish Add vs Rewrite).

**Acceptance:** the existing `upsert_get_roundtrip_and_last_writer_wins` test
still passes; a **concurrency regression test** — N=10 threads (each with its
own `db::open` connection) upsert the same agent-scope key concurrently, then
assert exactly **one** row exists for that key and its content is one of the
writers' payloads (and zero errors surfaced) — this test fails against the old
code (5–10 rows) and passes against the new; a migration test seeds a DB with
hand-inserted NULL-keyed duplicates, opens it, and asserts one row per logical
key with the latest content + all `session_id`/`run_id` non-NULL. `cargo test`
green.

### M2 — Dedup-on-read guard
`load_system_blocks` (`:106`) and `load_session_blocks` (`:158`) collapse to
one row per logical key (max `id` wins) before returning; block ordering
(`priority, name, id`) unchanged for the survivors.

**Acceptance:** a unit test hand-inserts two rows with the same logical key
(bypassing the constraint via direct SQL with distinct sentinel-era values or
a constraint-dropped fixture) and asserts each load function returns that
block exactly once, with the max-`id` content. `cargo test` green.

### M3 — The `(type, deadline)` index, measured
Add `idx_events_type_deadline` (`src/db.rs`, beside `:173-175`). Verify with
`EXPLAIN QUERY PLAN` that `expire_deadlines`'s SELECT
(`src/dispatcher.rs:704-710`) uses the index (no `SCAN e`), and reproduce the
research doc's before/after measurement: seed ~200k events, time the query
before (expect ~ms) and after (expect low µs) — the doc's own "how to measure
it later" recipe (section 4). Record the numbers in this handoff's Log.

**Acceptance:** an `EXPLAIN QUERY PLAN` assertion (a test that prepares the
exact query and asserts the plan mentions the index, guarding against future
query drift re-introducing the scan); the before/after timing logged; existing
deadline-expiry tests green. `cargo test` green.

## Read these first
- The evidence: [../notes-scaling-and-storage.md](../notes-scaling-and-storage.md)
  sections 1 (the scan, with the benchmark table) and 2 (the duplicate-row
  reproduction + the two candidate fixes).
- The write path: `src/context_store.rs` — `upsert_block` `:370` (the NULL
  workaround comment `:382-387`, UPDATE `:389`, INSERT `:410`, build-log
  `:434`), `remove_block` `:451`, the last-writer-wins contract `:367-369`.
- The read path being guarded: `src/context_store.rs` — `load_system_blocks`
  `:106` (predicate `:116`), `load_session_blocks` `:158` (predicate `:168`).
- The schema: `src/db.rs:304-322` (`context_blocks` + the UNIQUE at `:319`),
  `:139-175` (`events` + the three existing indexes `:173-175`).
- The scanning query: `src/dispatcher.rs:701-737` (`expire_deadlines`; the
  SELECT `:704-710`), called from `tick()` at `:256`.
- Who consumes blocks (the blast radius): [memory-blocks.md](memory-blocks.md)
  (blocks render into prompts and the coding-agent injection),
  `context_store.rs` `NOTE_BLOCK` `:148` (the per-session note is
  session-scope — one of the affected scopes).

## Log
- 2026-07-02 — Created from the storage research probe's findings
  (`docs/notes-scaling-and-storage.md`, landed 7ee9aa5) at Fable's direction,
  alongside the `_questions.md` sprint-3 pull. Grounded against the worktree:
  the NULL-neutered UNIQUE is real (`db.rs:321` + the workaround comment
  `context_store.rs:382-387`), the upsert is read-then-branch with no
  transaction, both load functions return duplicates undeduped, and
  `expire_deadlines` leads with `type` + `deadline` which no existing index
  covers. Judgment calls for Fable: sentinel `''` migration + native ON
  CONFLICT over transaction-only (1); dedupe-then-backfill order, keep-latest
  winner (2); permanent dedup-on-read guard (3); index-only fix for the scan,
  with an EXPLAIN-plan regression test (4).

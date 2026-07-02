---
status: research
author: probe synthesis (3x adversarial dynamic workflow probes)
last-updated: 2026-07-02
---

# Notes: scaling and storage

Three standalone probes investigated whether elanus's storage layer becomes a
bottleneck as agent count grows (10 → 100), and whether Dolt (a versioned,
git-like SQL database) should replace any of elanus's three storage stores.
All probes worked read-only against `elanus-sprint` (reading `src/db.rs`,
`src/context_store.rs`, `src/dispatcher.rs`, `src/config_repo.rs`,
`src/events.rs`, `src/broker.rs`, `spike/ntex/REPORT.md`) and did all
measurement in standalone scratch crates outside the repo. No repo mutations,
no git mutations, no processes left running.

If you're about to design the knowledge-base store (see
`docs/handoffs/knowledge-base.md`, in flight), **read section 3 below before
choosing storage** — it's a direct precedent for that decision.

---

## 1. The scaling picture: what breaks first as agents go 10 → 100

Ordered by which one an instance actually hits first, with evidence and the
cheap mitigation for each.

### 1st: `expire_deadlines`'s missing index — grows with ledger *age*, not agent count

`dispatcher.rs::tick()` runs every 1000ms (default `interval_ms`, one thread,
fully serial: `tick_crons` → `tick_schedules` → `expire_deadlines` →
`announce_ledger_events` → `reap` → `settle_code_deliveries` →
`drive_code_deliveries` → `resume_suspended` → `dispatch_pending` →
`tick_actors` → `release_dead_leases`, one connection, one after another).

Benchmarked against the real schema (`src/db.rs:139-174`) and real queries
(`src/dispatcher.rs`) at 1k and 200k row scale:

| Query | Index used | Cost @ 1k rows | Cost @ 200k rows |
|---|---|---|---|
| `announce_ledger_events` (`WHERE announced=0 ... LIMIT 500`) | partial index `idx_events_unannounced` | 40µs | 56µs — flat, backlog-bound |
| `dispatch_pending` (`WHERE state='pending' ...`) | `idx_events_pending(state,type,priority)` | 20µs | 37µs — flat, pending-bound |
| `expire_deadlines` (`WHERE e.type=?1 AND ... deadline<now AND NOT EXISTS(...)`) | **none — full `SCAN e`** | 46µs | **9.3ms — linear in total table size** |

`events` has indexes on `(state,type,priority)`, `(correlation_id)`, and a
partial one on `announced=0`, but nothing on `type` alone or on `deadline`.
`expire_deadlines`'s `WHERE` leads with `e.type=?1` and never constrains
`state` by equality, so SQLite can't use `idx_events_pending`'s leftmost
`state` column and falls back to a full scan **every tick, unconditionally**,
whether or not any deadlined asks exist.

This is the one query in the tick loop that scales with **total historic
ledger size**, not concurrent agent count — it degrades as the instance ages,
independent of how many agents are live at once. At 200k rows it's still only
9ms inside the 1s tick budget, but it's O(N) and unbounded, and 100 busy
agents accumulate events fast. This is the first thing that will visibly eat
tick time, then deadline-expiry latency for waiting-on-human asks, as an
instance's ledger grows into the millions.

**Mitigation:** add an index on `(type, deadline)`, or restructure the
correlated `NOT EXISTS` to use the existing `idx_events_correlation` index.

### 2nd: SQLite single-writer serialization on ledger inserts — real, but far above realistic emit rates

`events::emit` (`src/events.rs:52-121`) is a single parameterized
`INSERT ... ON CONFLICT DO NOTHING`. Measured with real pragmas
(`src/db.rs:6-11`: WAL, `synchronous=NORMAL`, `busy_timeout=5s`):

| Concurrent writer connections | Aggregate throughput |
|---|---|
| 1 | ~25,000 events/sec |
| 10 | ~16,000 events/sec |
| 100 | ~3,600 events/sec (slowest thread ~2.8s for 100 rows) |

Contention visibly degrades aggregate throughput as concurrency rises — but
even the floor (3.6k/s at 100 concurrent writers) is orders of magnitude
above any realistic per-tool-call emit rate from 100 agents. Real, but
second-order — not the first ceiling hit.

### 3rd: memory-block write contention — see section 2 (throughput is a non-issue; there's a separate correctness bug)

### Not a near-term ceiling: the broker

`spike/ntex/REPORT.md`: `client.publish()` latency ~63µs debug / ~11µs
release; full cross-thread echo round-trip <10ms. `fan_out`
(`src/broker.rs:1274`) is a linear scan over sessions with a matching
subscription per publish — O(subscriber count) per message — with
deferred-PUBACK completion via `FuturesUnordered`/`join_all` over
per-subscriber sinks. No subscriber-count throughput curves exist in the
cited docs (not re-measured here per instructions — cited from existing
spikes), but nothing suggests the broker is the near-term ceiling at 10→100
agents; its per-op cost is µs-scale.

### Bottom line for section 1

Ranked by how soon an instance actually feels it: **(1) `expire_deadlines`
full scan — grows with ledger age, first visible symptom as history
accumulates; (2) sqlite single-writer lock on ledger inserts — real
degradation curve but a floor (3.6k/s @ N=100) far above realistic agent
emit rates; (3) memory-block write throughput — same single-writer lock,
same conclusion, see section 2; broker fan-out — no evidence it's close.**
None of these are urgent at today's scale; #1 is the one to fix before an
instance runs for months with many agents.

---

## 2. Memory-block contention — Tim's direct question

**Question:** "If 100 agents all modify the same memory blocks, does that
make blocks a scaling bottleneck?"

**Throughput: no.** A standalone probe exactly mirroring `db::open()`'s
pragmas and `context_store.rs::upsert_block()`'s logic (read-check-then-branch
UPDATE/INSERT + `context_build_log` insert, no surrounding transaction,
matching the real code verbatim) measured:

| N (writers) | Mode | Writes/sec sustained | SQLITE_BUSY errors |
|---|---|---|---|
| 1 | any key | ~14,300–17,200/s | 0 |
| 10 | same key | ~3,800–4,600/s | 0 |
| 100 | same key (1000 writes) | ~823/s | 0 |
| 100 | distinct keys (2000 writes) | ~1,026/s | 0 |

Throughput drops ~15–17× from N=1 to N=100 and converges to ~800–1,200
writes/sec **regardless of whether writers hit the same key or different
keys** — this is SQLite's single-writer-transaction lock (WAL only allows one
committing writer at a time), not row-level contention. `busy_timeout(5s)`
fully absorbs the queueing: zero `SQLITE_BUSY`/`SQLITE_LOCKED` errors across
every run — writers block-and-wait rather than failing. ~1,000 writes/sec is
not a bottleneck for the real workload (agents write memory blocks
occasionally, not in a tight loop; queueing of a few ms is negligible next to
LLM turn latency).

**Correctness: yes, a real bug, and it's worse than last-writer-wins.** The
`context_blocks` table's `UNIQUE(scope, owner, session_id, run_id, name)`
constraint is silently neutered by NULL columns for every scope except `run`:

- SQLite treats NULL as *distinct from NULL* in a UNIQUE index.
- `scope='global'/'agent'` → both `session_id` and `run_id` are NULL.
- `scope='session'` → `run_id` is still NULL.
- Only `scope='run'` has both columns populated — confirmed with raw
  `sqlite3`: two inserts with identical `(run, x, s1, r1, memo)` correctly
  threw `UNIQUE constraint failed`; the identical test with `session_id=NULL`
  or `run_id=NULL` inserted **both** rows with no error, exit 0.

`context_store.rs` (comment at line 382-387) already half-acknowledges this
and works around it in `upsert_block` with a manual read-then-branch — but
that read-then-branch is **not wrapped in a transaction**, so under real
concurrency it's a textbook TOCTOU race: multiple writers each see "no
existing row," each take the INSERT branch, and the (useless) UNIQUE index
doesn't stop them.

Measured, reproducibly:
- N=10 writers hammering the *same* agent-scope key → **5–10 duplicate rows**
  (expected 1) across 3 runs.
- N=100 writers hammering the same key → **30 duplicate rows** (expected 1).
- All writes reported `Ok` — no error surfaces to the caller. Silent failure.

This is worse than "last-writer-wins losing data" (which the existing unit
test `upsert_get_roundtrip_and_last_writer_wins` verifies for the
*uncontended* case, and is fine as a design choice). Under real contention on
scope `global`/`agent`/`session` — the vast majority of real block usage,
including identity blocks and the per-session `note` block — you don't get
clean last-writer-wins, you get **duplicate rows for the same logical
block**. And `load_system_blocks`/`load_session_blocks`
(`src/context_store.rs:106-194`) issue a plain
`SELECT ... ORDER BY priority, name, id` with no `DISTINCT`/`GROUP BY`/
`LIMIT 1` per name, so every duplicate row is returned and injected into the
prompt — an agent could see the same block name rendered 2–30 times with
different (possibly stale) content.

**Verdict:** no scaling bottleneck on throughput (SQLite serializes writers
but sustains ~1,000 writes/sec with zero errors — plenty for occasional block
writes); **but concurrency exposes a real correctness bug**: writes to the
same `(scope, owner, session_id, run_id, name)` key under `global`/`agent`/
`session` scope can silently produce duplicate rows instead of a clean
overwrite, because the UNIQUE constraint is inert whenever `session_id`/
`run_id` is NULL and the app-level upsert isn't transactional. Only
`scope=run` blocks (both columns populated) get real DB-enforced uniqueness
today.

**Fix (either one closes it):**
1. Wrap `upsert_block`'s read+write in an explicit `BEGIN IMMEDIATE` /
   `COMMIT`, or
2. Replace the NULLable columns with a sentinel value (e.g. `''` instead of
   `NULL`) so the UNIQUE index actually fires and `ON CONFLICT` upserts work
   natively.

(2) is probably the better long-term fix — it makes the DB itself the source
of truth for uniqueness instead of app-level TOCTOU-prone logic, and it's a
small migration (backfill NULL → `''` in `session_id`/`run_id`).

---

## 3. Dolt vs elanus's storage stores — per-store recommendation

Dolt is a MySQL-compatible, git-like versioned SQL database (Go, ~2019+).
Findings below are the load-bearing facts, not the full research trail.

### Ecosystem facts that matter for elanus specifically

- **DoltHub's own guidance says don't embed it.** Their "[When NOT to Use
  Dolt](https://www.dolthub.com/blog/2025-12-30-why-not-dolt/)" (Dec 2025)
  explicitly lists "embedded systems: only works in Go; SQLite is better for
  iOS/mobile" and calls Dolt an OLTP database, not a fit for lightweight
  embedding.
- **No native Rust embedding exists.** The Go `database/sql` driver gives
  single-writer, file-based embedding, but only *inside Go processes*. Normal
  usage is server mode: `dolt sql-server` on a TCP port speaking the MySQL
  wire protocol; Rust would talk to it as a MySQL client (`mysql` crate /
  Diesel) — i.e. running a separate daemon process for every elanus instance.
  DoltHub shipped **DoltLite** in April 2026 (C-based, SQLite-shaped,
  "compiles to... Rust crates") but by their own description it's "newly
  shipped... community-driven... inviting issues" — not production-hardened,
  and no stable `dolt-lite` Rust crate is published on crates.io as of this
  check.
- **Throughput:** Dolt is ~10% faster than MySQL on single writes but only
  ~40% of MySQL's TPC-C throughput under concurrency (40 tx/s vs ~100 tx/s,
  per DoltHub's own benchmark). SQLite embedded (no network hop) beats a
  client/server MySQL-protocol DB on single-writer local workloads
  specifically because it avoids IPC/network overhead — which is exactly
  elanus's shape (one local daemon, one writer, WAL already handling
  concurrent readers).
- **History has a real, acknowledged storage cost.** Dolt keeps entire
  history by design; storage grows with every write, indexes are versioned
  per-commit, and schema changes ("adding a column forks the table") lose
  structural sharing. DoltHub has open work (dictionary compression,
  cold-storage culling) specifically because history bloat is a known
  operational cost.

### (a) Ledger — Dolt would not beat SQLite/WAL

The ledger is append-only by *event insertion*, not by *row mutation
history* — nothing is ever UPDATEd/DELETEd in a way that needs diffing (rows
carry `state` transitions and `finished_at`, but the row itself, not a
git-style history of the row, is what's queried). Git-style commit history
answers "what did this row look like at each past version" — a question
elanus never asks of the ledger. It asks "what happened, in what causal
order," which the existing `cause_id` parent-pointer chain plus `created_at`
already answers, at SQLite's lower write latency and with no server process.
Dolt's per-write versioning overhead here is pure cost with no matching need.

**Recommendation: keep SQLite. Do not use Dolt for the ledger.**

### (b) Config — moot, already solved with real git, and solved better than Dolt could give it

`src/config_repo.rs` is a hardened, kernel-owned git repo (`<root>/config`,
`live` branch, proposal branches, `git diff --raw` path-discipline). Config
is small text files (packages/agents TOML) where human/agent legibility
(`git diff`, `git log -p`) and the proposal-branch review model are the
actual requirement — plain git plumbing, already hardened against the
untrusted-content attack surface (identity-model increment 3/4 work,
`docs/security.md` entry 19: mode-aware path-discipline, symlink rejection,
case-insensitive protected-name checks, size/file caps). Swapping in Dolt
here would trade a simple, auditable, zero-dependency git repo for a
SQL-flavored git substitute with weaker Rust tooling and the same
untrusted-content risks to re-solve from scratch.

**Recommendation: no change. Real git already wins this comparison.**

### (c) Blocks/KB — the closest conceptual fit, still not a clear win today

This is genuinely "versioned, agent-editable structured content" — Dolt's
actual sweet spot (diffable rows, potential branch/merge). But:

1. The current design deliberately avoids needing merge — writes are
   owner-scoped, so each identity only ever touches its own rows, and there
   is never a conflict to resolve.
2. `context_build_log` already gives a full audit trail (who/what/when/
   before-sha/after-sha per mutation) without needing git-style branching —
   the audit need Dolt would nominally satisfy is already satisfied by a
   much cheaper hand-rolled log table.
3. The correctness bug in section 2 (NULL-neutered UNIQUE constraint,
   non-transactional upsert) is an elanus bug in the current schema/logic,
   not evidence that the underlying data model needs Dolt's versioning —
   it needs a transaction or a sentinel-value fix, either of which is a
   small, local change.
4. Dolt's Rust story (no native embedding, DoltLite unproven, server-mode
   daemon otherwise) is a real operational cost elanus would take on for a
   feature (branch/merge on blocks) nothing today asks for.

**Recommendation: no change today.** If a future requirement genuinely needs
branch/merge semantics on structured agent-editable content — e.g. a
knowledge base where two agents might independently propose conflicting
edits to the *same* shared (non-owner-scoped) entry and a human needs to
review/merge the diff — that is the point at which Dolt (or DoltLite, once
it matures) becomes worth re-evaluating, not before. **This is exactly the
decision point the in-flight `docs/handoffs/knowledge-base.md` should read
before picking a storage engine**: if the KB design keeps writes
owner-scoped like blocks do today, SQLite + an audit-log table is the
established, cheaper pattern; if the KB design *requires* shared-entry
conflict/merge, that's a real point in Dolt's favor, but arrives with the
Rust-embedding cost above (server-mode `dolt sql-server` daemon, or a bet on
unproven DoltLite) that should be sized explicitly before choosing it.

---

## 4. What we did NOT measure, and how to measure it later

- **Broker subscriber-count throughput.** `spike/ntex/REPORT.md` gives
  per-op latency (µs-scale) but no curve for fan-out cost as subscriber count
  grows to reflect 100 live agent sessions. To measure: extend the ntex spike
  bench with N subscribed sessions (10/100/1000) and measure publish-to-all
  latency and CPU at each N; watch for the `fan_out` linear scan
  (`src/broker.rs:1274`) actually showing up as a cost once N gets large
  enough that O(subscribers) stops being negligible.
- **`expire_deadlines` fix, verified.** We identified the missing index but
  did not implement + re-measure the fix. To close the loop: add
  `CREATE INDEX idx_events_type_deadline ON events(type, deadline)` (or
  restructure the `NOT EXISTS` to ride `idx_events_correlation`), rerun the
  same 1k/200k-row bench, confirm `EXPLAIN QUERY PLAN` shows an index scan
  instead of `SCAN e`, and confirm the 9.3ms-at-200k-rows number collapses to
  low-µs like the other two tick queries.
- **Real production event-emission rate.** The 3.6k/s floor at N=100
  concurrent ledger writers was benchmarked against a synthetic tight-loop
  write pattern, not against real agent tool-call cadence. To measure:
  instrument a live instance (or replay ledger history) to get actual
  events/sec/agent under real workloads, and confirm it stays orders of
  magnitude below 3.6k/s as claimed.
- **Memory-block fix, verified.** We reproduced the duplicate-row bug but did
  not implement either candidate fix (transactional upsert vs. sentinel
  value) or re-run the N=10/100 same-key contention test against the fixed
  code to confirm duplicates drop to zero and `SQLITE_BUSY` still doesn't
  surface (transactional fix could reintroduce blocking/backoff behavior
  worth re-measuring).
- **Tick loop cost of the *other* nine tick steps under agent-count scaling**
  (`tick_crons`, `tick_schedules`, `reap`, `settle_code_deliveries`,
  `drive_code_deliveries`, `resume_suspended`, `tick_actors`,
  `release_dead_leases`) — only `announce_ledger_events`, `dispatch_pending`,
  and `expire_deadlines` were benchmarked. The others weren't ruled out as
  scaling with agent count; a full tick-loop profile at 100 concurrently
  active agents (not just 200k historic rows) would confirm nothing else in
  that serial chain grows unexpectedly.
- **Dolt/DoltLite hands-on benchmarking.** All Dolt findings are from
  DoltHub's own published posts/benchmarks and repo docs, not independently
  reproduced against elanus's actual data shapes. If Dolt is ever seriously
  considered for the KB store, worth an actual DoltLite embed spike (even a
  toy one) rather than relying on vendor-published numbers, given DoltLite's
  "newly shipped, inviting issues" maturity status.

---
status: research
last-updated: 2026-07-02
---

# Dolt spike: files+git (Form A) vs Dolt (Form B)

Hands-on spike, both forms built and run in `/private/tmp/.../dolt-spike/`. No mutation
of this repo. Full raw report is the input to this note; numbers below are pulled
straight from it.

## 1. What was actually done

**Form A (files + git, control).** 8-file KB corpus seeded into a git repo.
`write20.sh` mirrors `kb.rs`'s `write()` exactly: sibling-tmp write + rename, `git add
-- path`, `git diff --cached --quiet -- path` no-op check, `git commit -- path`, under
the same hardened env kb.rs uses.

- 20 writes → 20 commits in **0.91s** (~46ms/write)
- clone-as-backup: **0.084s**, 504K → 140K compressed
- `grep -rli "who verifies" kb/` → hit instantly (~10ms), zero setup

**Form B (Dolt).** `dolt init`, one table `docs(path PK, content LONGTEXT)`, 8 rows
seeded, write loop shaped like Form A's (`UPDATE` → `dolt add docs` → `dolt commit`).

- 20 writes → 20 commits in **8.17s** (~408ms/write) — **~9x slower** than git for the
  identical logical operation
- push/clone to a `file://` remote: 0.14s/0.19s, chunk-based transfer — works, but the
  remote is a Dolt-native chunk store, unreadable by anything except a Dolt client
- "who verifies" search: `LIKE '%who verifies%'` returned **0 rows** on first try —
  Dolt's default collation (`utf8mb4_0900_bin`) is case-sensitive; needed `LOWER()`
  added explicitly. `grep -i` gives this for free.
- `dolt commit` has no `-q` flag — small but real git-muscle-memory tax

## 2. Head-to-head on what the KB design actually needs

| Property | Files+git | Dolt | KB needs it? |
|---|---|---|---|
| `path:lines` anchors for pointer blocks | native (`read_to_string` + line count) | **not native** — content is an opaque `LONGTEXT` cell; must `SELECT`, then compute lines app-side, same as today plus a SQL round trip | yes — this is the pointer-block contract (`meta.path`, `meta.lines`, `meta.sha`) |
| Greppability from an agent's plain shell | native | **none** — no files on disk in `form-b/`; `grep` finds nothing, every reader must go through `dolt sql` or a MySQL client | yes — agents use grep/ripgrep/cat routinely |
| Sandbox-gated writes (existing path-discipline model) | drop-in — same git primitives kb.rs already shells to | different shape — no per-row staging or no-op detection; `dolt add <table>` stages the whole table's diff, so you must `SELECT` before `UPDATE` to detect a real no-op | yes — kb.rs's no-op check is load-bearing today |
| Remote backup | `git clone`, 0.084s, any git host, any tool can read it | `dolt clone` to a Dolt-native remote, 0.19s, only Dolt clients can read it | yes |
| Write latency | 46ms/write | 408ms/write (~9x) | yes — this is the hot path for every KB write |
| Row-level diff (`dolt diff`) | no direct equivalent; `git diff -- path` is a text diff | genuinely nicer — clean cell-diff table, highlights the changed span within a cell | not required by the design; D2 already sidesteps concurrent same-entry merge |
| Row history (`dolt_history_docs`) | `git log -p -- path` + manual stitching | real capability — one SQL query returns every historical value + committer + date | nice-to-have, not a current KB requirement |
| Branch/merge on different rows | works (real 3-way merge, verified) | works (auto-merged, verified) | not required — KB writes are single-writer per entry today |
| Branch/merge on the *same* row/file | **conflicts** (verified: `CONFLICT (content)`) | **also conflicts** (verified: `dolt_conflicts_docs`, resolved via `dolt conflicts resolve`) | not an advantage either way — both fail closed identically. Dolt's conflict view is a structured SQL table (nicer for programmatic resolution); git's is inline markers (nicer for a human eyeballing markdown) |
| `AS OF` time-travel query | not declarative — `git show <rev>:path`, scripted per file | real capability — one SQL query, filtered across the whole corpus at a point in time, by hash or timestamp | not required by current KB reads; would only matter if the KB grows a "what did we believe on date X, across all entries" query pattern |

Other measured costs: **115.6MB** Dolt binary (110MB cellar) vs git's ~58MB cellar; `dolt
sql-server` cold start ~1s (fine for a daemon, bad fit for kb.rs's current
CLI-shell-per-write pattern).

## 3. Verdict

**Don't adopt.** Every property the current KB design actually depends on — file+line
anchors for pointer blocks, greppability from an agent's ordinary shell, the per-path
git no-op check kb.rs already relies on, and write latency on the hot path — is either
free with files+git or actively regresses under Dolt (anchors need an extra SQL round
trip and app-side recomputation; grep stops working entirely; write latency is 9x
worse). The properties Dolt adds for free — row-level diff/history, declarative `AS OF`
queries, structured conflict tables — are real and verified, but nothing in the current
KB design (per D2, concurrent same-entry merge is deliberately out of scope) needs them.
Same-row conflict behavior is a wash: both fail closed identically, Dolt's win there is
presentation, not capability.

**Trigger that would reopen this:** if the KB grows a requirement that files+git cannot
serve declaratively — specifically (a) "what did the whole corpus look like at time T"
as a live query rather than an audit-trail curiosity, or (b) genuinely concurrent
writers to the *same* entry becoming a normal (not exceptional) case, such that a
structured conflict table beats inline markers often enough to matter. Neither is true
today. If either becomes true, re-run this spike's write-latency and anchor-reconstruction
measurements against whatever the KB corpus size has grown to by then — the 9x gap may
matter less at higher latencies elsewhere in the write path, or it may not.

## 4. What the spike couldn't test

- Dolt behavior at KB-realistic corpus size (spike used 8 seed files / 20 writes;
  real KB is larger and long-lived — sql-server steady-state memory/CPU under sustained
  small writes wasn't measured).
- Concurrent writers hitting Dolt's SQL engine from multiple processes at once (the
  write loop was single-process, sequential — kb.rs's actual concurrency model, if any,
  wasn't exercised against Dolt).
- Any migration path for existing KB content (no export of the real corpus into Dolt
  was attempted beyond the 8-file seed).
- Dolt as a long-lived `sql-server` daemon instead of per-command CLI invocation — the
  ~1s cold start was measured but a daemon-resident latency profile (which is the shape
  that would actually fix the 9x write gap) was not.

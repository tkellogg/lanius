---
status: implemented
author: Claude Opus 4.8 (planner) in Claude Code on Elanus
last-updated: 2026-07-13
---

# Handoff: incarnation-safe coding-session credentials

Mirror of chainlink #10. This closes the credential-collision half of the
claude-code crash class: a driven resume of a live coding session mints a new
secret over the live incarnation's secret and then deletes the shared token
file, so every subsequent hook publish from the live session is refused
(`bad/unknown session token`). Two long-lived supervised sessions were lost to
this in three days (`code-79985d39`, 1,003 broker refusals; `code-d94cac68`).

The full incident reconstruction is
[docs/bugs/claude-code-adapter-summary-credential-crash.md](../bugs/claude-code-adapter-summary-credential-crash.md).
Two sibling defects it names are already handled and are NOT re-specified here:
phantom edit claims after a crash (fixed in `24ecdfb`) and the adapter-summary
ENOENT race (source fixed in `5c16270`). What remains is the credential
lifecycle (M1–M3) plus a verify-it's-actually-live check on the adapter refresh
(M4).

## Read these first

- [docs/bugs/claude-code-adapter-summary-credential-crash.md](../bugs/claude-code-adapter-summary-credential-crash.md)
  — the evidence, the two candidate credential designs, and the fix direction.
- `src/codesession.rs` — the token store: `mint` (line 2396), `retire`
  (line 2649, currently an unconditional `remove_file`), `reap_orphans` +
  `pid_alive` (2662–2700), `write_0600`/`token_path` (2629), and the
  `BudgetLock` flock pattern (2274–2311) we reuse for serialization.
- `src/codeagent.rs` — the two lifecycle owners: launch mint at 3889 / launch
  retire at 4178, and `resume_capture` (8637) with its mint at 8663 and its
  three retire sites (8730 ACP, 8760 unknown-tool, 8820 CLI).
- `src/broker.rs` **HEAD version** (`git show HEAD:src/broker.rs`) — the working
  tree has another agent's unstaged rate-limit experiment; design against the
  committed `resolve_connect` (lines 255–300), which resolves a `code-*` CONNECT
  by exact `tok.secret == pw` and returns `NotAuthorized`/`bad/unknown session
  token` otherwise.
- `src/dispatcher.rs:1678` — where a delivery job drives `resume_capture`. This
  is the trigger: a `lanius code deliver` to a live TUI enqueues a job that
  resumes the same session.
- `src/initcmd.rs` — `seed_stock_harness_packages` (793) and
  `refresh_adapter_if_stale` (845); relevant to M4.
- Tim's design law (AGENTS.md + memory): capability = installed package +
  granted permission, never a hard-coded special case in the kernel; simplicity
  first; safety = an audit trail, not gates between the human's own agents.

## How the store works today (the shared mutable slot)

A session credential is one file, `.secrets/code-sessions/<principal>.json`,
carrying a single `secret` and the launcher's `owner_pid`
(`codesession.rs` mint, 2396–2634). There is exactly one secret per principal.

- **Launch** (`codeagent.rs:3889`) mints for principal `code-<session>` with
  `owner_pid = std::process::id()` (the launcher/TUI parent) and hands the
  secret to the long-lived adapter, whose hooks present it on every bus CONNECT.
- **Driven resume** (`codeagent.rs:8663`) mints again for the **same principal**
  with a **fresh secret** and `owner_pid` = the resume process's own pid. `mint`
  atomically overwrites the one token file (`write_0600`, 2629). The live
  incarnation's hooks keep presenting the **old** secret, which no longer matches
  → broker returns `bad/unknown session token` (`broker.rs` HEAD 264–270) on
  every subsequent publish.
- **Retire** (`codesession.rs:2649`) is an unconditional
  `remove_file(token_path)`. The resume calls it at every exit (8730 / 8760 /
  8820), deleting whatever secret is on disk — including the live launcher's.

So overlap between a live incarnation and a resume corrupts the live one two
ways: the mint overwrites its secret, and the retire deletes the file out from
under it. Both operations are name-scoped (by principal), never secret-scoped.

## Wonky bits / decisions to confirm (Tim, overrule tonight if wrong)

1. **Credential design ruling: reuse-the-live-credential, not
   multiple-generations** (see M1 for the reasoning). This is the higher-stakes
   call. If you'd rather we carry N independent concurrent incarnations each with
   its own scoped secret (design A), say so — it's a bigger change (store format
   + broker hot path) and I judged it unwarranted by the evidence, but it's your
   call.
2. **Resume gating ruling: a delivery to a *live* session does NOT spawn a
   parallel native resume — it lands in the inbox and is picked up on the live
   session's next turn.** This trades "a delivery to a live TUI is processed
   immediately by a background resume" (which is exactly what corrupts the
   credential today) for "a delivery to a live TUI is seen when the human next
   prompts it." I believe that's correct — a live TUI is human-driven and already
   surfaces its inbox each turn — but it is a visible behavior change to
   `deliver`, so flagging it.
3. **True in-process injection into a live TUI is DEFERRED** (residual R1). The
   bug doc's open question — inject into the running process vs. parallel resume
   — is ruled: for now, neither; queue to inbox. A real control channel into the
   live child is a separate feature.
4. **`owner_pid` liveness is the "live launcher/beacon" signal.** No new
   heartbeat is introduced; we reuse the same `pid_alive` probe that
   `reap_orphans` already uses. Confirm you're happy treating a live `owner_pid`
   as "this session is owned by a live launcher."

## Milestones

### M1 — Incarnation-safe credentials: reuse-live + secret-scoped retire

**Ruling: reuse the live credential; mint an ephemeral one only for a genuinely
idle session; make retirement secret-scoped (compare-and-delete).** Rejected
alternative: multiple active token generations per principal.

Reasoning a reviewer can check:
- The store is keyed by principal with one secret, and the broker resolves by
  exact `secret == pw` (`broker.rs` HEAD 264–270). The *only* reason a resume
  needs its own secret is that it mints one; if instead it **reuses** the live
  secret when a live owner exists, the overwrite that breaks the live incarnation
  never happens. Nothing downstream needs two secrets — the broker is happy with
  multiple connections presenting the same secret.
- Design A (generations) requires changing the on-disk token shape to hold a set
  of secrets, teaching `broker.resolve_connect` to check set-membership on the
  hot CONNECT path, and generation-scoping retire — for a case (two genuinely
  independent concurrent incarnations, each needing a *distinct* scoped
  credential) that the evidence never shows. Every observed collision is
  live-TUI-plus-resume, which reuse handles by sharing one secret. Simplicity
  first: design B adds no store-format or broker change.
- Design B reuses machinery that already exists: `reap_orphans`/`pid_alive`
  (`codesession.rs` 2662–2700) already decides "is this token's owner alive?"
  That same liveness check is the whole of the new logic — no new kernel special
  case, no new type enumeration (Tim's law).

Implementation shape:
- **Resume before minting** (`codeagent.rs:8663`): read the existing token for
  the principal. If it exists **and** `pid_alive(tok.owner_pid)` **and** that pid
  is not this resume's own pid → **reuse** `tok.secret` as `bus_token`; do NOT
  mint, do NOT overwrite, and record that this run is a *guest* on a live
  credential. Otherwise (no token, or a dead owner) → mint a fresh ephemeral
  token as today (`owner_pid` = this resume's pid).
- **Secret-scoped retire.** Add `retire_if(root, principal, expected_secret)`:
  read the file, and `remove_file` **only if** the on-disk `secret` equals
  `expected_secret`. A guest run (reused a live secret) must **not** retire at
  all — it never owned the credential. An ephemeral-mint run retires with its own
  secret via `retire_if`, so it can never delete a credential a concurrently
  relaunched launcher just minted. The launch path (`codeagent.rs:4178`) also
  switches to `retire_if(root, principal, token.secret)`. Keep the name-only
  `retire` only if some caller legitimately needs "delete whatever's there"; the
  three resume sites (8730/8760/8820) and launch must use the scoped form.
- `mint` itself is unchanged for the idle case; the guarding logic lives in
  `resume_capture` where the live/idle decision is made.

**Acceptance:**
- A unit test: two overlapping "incarnations" for one principal — a launch mint,
  then a resume against a live `owner_pid` — leaves the launch secret intact
  (resume reused it, did not overwrite), both authenticate against a real broker
  `resolve_connect` throughout, and the resume's exit does not delete the file.
- A unit test: an idle-session resume (no token) mints, publishes, and its exit
  removes exactly its own secret; if a fresh launch mint replaces the file
  mid-run, the resume's `retire_if` is a no-op and the launch credential
  survives.
- A unit test: `retire_if` with a non-matching secret does not remove the file;
  with a matching secret it does.
- Integration: a live TUI plus a driven resume against an isolated broker — the
  live incarnation publishes with no `NotAuthorized` across the resume's whole
  lifetime, and after the resume exits the live credential is still valid.

### M2 — Resume gating as containment

Even with M1, a background resume spawning a *parallel* native `claude -p
--resume` against a live human-driven TUI is wasteful and confusing. Rule the
`deliver` interaction:

- **Refuse the parallel resume while a live launcher owns the session.** In
  `resume_capture`, after reading the token: if a live owner exists (token
  present, `pid_alive(owner_pid)`, not our pid), return a `ResumeOutcome` that
  does **not** spawn a native child — `success: false` is wrong (nothing failed);
  return a distinct "deferred to live session" outcome with a clear `final_text`:
  the message is already in the session's inbox (deliver wrote it there) and the
  live session surfaces its inbox on its next turn (the inbox-lead injection
  already built in `build_resume_message` / `configure_resume_child_env`,
  `cacdf6f`). The dispatcher (`dispatcher.rs:1678`) treats this as handled, not
  as a retryable failure.
- **Serialize resumes for the idle case.** Two idle resumes of the same principal
  must not overlap (the second would mint over the first). Take a **per-principal
  advisory lock** for the duration of an idle resume, following the `BudgetLock`
  flock(LOCK_EX) pattern (`codesession.rs` 2274–2311) but keyed on the principal
  (e.g. a lock file `<store>/<principal>.lock`). A second idle resume waits (or,
  if you prefer fail-fast, is refused with "a resume of this session is already
  running"); confirm which — I lean **serialize/wait** so a burst of deliveries
  to an idle worker all get processed.

**Acceptance:**
- Integration: with a live launcher owning the session, `resume_capture` returns
  the deferred-to-inbox outcome, spawns no native child, and the message is
  present in the session inbox; the live credential is untouched.
- Integration: two concurrent idle resumes of one principal do not overlap —
  the second observes the first's credential (or waits for it), and neither ends
  with the other's credential deleted; both messages are processed.
- The dispatcher does not mark a deferred-to-live delivery as a failed job.

### M3 — Preserve the terminal reason kernel-side

Today, when the credential is gone, the final `session/stop` publish is refused
and the session's terminal state is simply unknown — the projection still calls
`code-79985d39` `idle` with no end time. Record the stop reason where the bus
can't erase it, and separate the child's real exit from post-exit noise.

- **Kernel-side stop/failure record independent of the bus.** When a launch or
  resume finishes (`codeagent.rs` around the launch retire 4178 and the resume
  tail 8820), write the terminal reason — child exit status / signal, or "killed
  before exit" — to the durable coding-session record (the same store
  `codesession::touch_record`/`upsert_record` already writes), not only to the
  bus. If the bus publish of `session/stop` is refused, the record still carries
  the reason. This is audit-as-safety (Tim's law): the change log records what
  happened even when telemetry is down.
- **Distinguish the child's exit from teardown noise.** The child's
  `ExitStatus` (code / signal) is the terminal reason. The adapter-summary ENOENT
  and any post-exit auth refusal are teardown artifacts and must not be recorded
  *as* the exit reason. Capture `status.code()` / signal at the `child.wait()`
  boundary and stamp that; log summary/auth failures separately as warnings.

**Acceptance:**
- Force the child to exit 0, exit non-zero, and die by signal (SIGKILL): in all
  three the durable record's terminal reason is accurate even when the bus
  refuses the `session/stop` publish (test against an isolated broker with the
  credential deliberately retired).
- A post-exit summary ENOENT or auth refusal does not overwrite or masquerade as
  the recorded exit reason.

### M4 — Verify the adapter refresh is live at the *use* boundary

The bug doc's fix direction #1 ("refresh adapters at the use boundary") is only
**partly** in place. `refresh_adapter_if_stale` (`initcmd.rs:845`) exists and is
correct (mtime compare, macOS fresh-inode rule), but its **only caller** is
`seed_stock_harness_packages` (`initcmd.rs:793`), which runs only from **init**
(`initcmd.rs:692`). The **launch** path does *not* refresh: it only checks
`adapter.exists()` and bails if missing (`codeagent.rs:3880`). So an already
seeded live root whose `harness-claude` was later upgraded via `cargo install`
keeps running the **stale** installed adapter until someone re-runs init — which
is exactly the observed state (installed adapter older mtime + different hash
than the source binary). **This is a real residual, not a done item.**

- Hoist the staleness check to the launch/use boundary. Before an external stock
  harness execs (`codeagent.rs:3880`, right where it validates the adapter
  exists), compare the package `bin/adapter` against the source binary in the
  running lanius's `exe_dir` and refresh if stale, reusing
  `refresh_adapter_if_stale` (make it callable from `codeagent`, or factor it to
  a shared spot). Keep the macOS remove-then-copy-to-fresh-inode rule. No
  separate `lanius init` should be required after an upgrade.

**Acceptance:**
- A launch with a deliberately older package adapter and a newer source binary
  refreshes the adapter to a fresh inode *before* exec, and the launched adapter
  is the new one (assert inode changed and mtime ≥ source).
- A launch with an up-to-date adapter does not re-copy (no needless churn, no
  SIGKILL risk).
- Existing `initcmd` refresh tests still pass; the seed-time refresh is retained.

## Residuals (deferred, on purpose)

- **R1 — in-process injection into a live TUI.** Delivering into a running
  interactive process (vs. queue-to-inbox) needs a control channel into the live
  child; deferred. M2 makes queue-to-inbox the behavior; this residual would make
  a live session act on a delivery without waiting for the human's next turn.
- **R2 — per-incarnation observability.** The projection still merges all
  incarnations under one session id, so "which resume emitted this event" and
  "which resume first broke the live one" remain unanswerable (bug doc open
  question). M1/M2 stop the corruption but don't split the timeline.
- **R3 — the adapter-summary ENOENT source race** is already fixed in `5c16270`
  with a regression test; M4 only ensures the *fixed binary* is what actually
  runs. Not re-opened here.
- **R4 — idle-resume fail-fast vs. wait** (M2 serialization policy) is a small
  UX call left for Tim to confirm; default proposed is wait/serialize.

## Log

- 2026-07-13 — Planner pass. Verified all anchors against current `main`
  (`1ccebe6`): store is one-secret-per-principal (`codesession.rs` mint 2396,
  retire 2649 unconditional); broker resolves by exact secret match
  (`HEAD:broker.rs` 264–270); resume mints over the same principal
  (`codeagent.rs:8663`) and retires unconditionally (8730/8760/8820); deliver
  drives resume via `dispatcher.rs:1678`; adapter refresh is seed-only
  (`initcmd.rs:793`→`845`), launch only checks existence (`codeagent.rs:3880`).
  Ruled: M1 reuse-live + secret-scoped retire (rejected multi-generation); M2
  refuse parallel resume for live sessions (deliver → inbox) + serialize idle
  resumes. M4 flagged as a genuine residual, not done.
- 2026-07-13 — Implemented + verified (planner-reconciled after a sonnet worker
  hit the fleet session limit mid-M1). Landed UNSTAGED across src/codesession.rs,
  src/codeagent.rs, src/db.rs (one migration), src/initcmd.rs. Fable to commit.
  - **M1** — `retire_if` secret-scoped compare-and-delete; `resume_credential_decision`
    (Reuse iff token present ∧ owner_pid≠self ∧ `pid_alive_pub`); launch + all three
    resume exits retire via `retire_if(minted_secret)` only; guest never retires.
  - **M2** — `resume_capture` returns early on a live foreign owner with
    `ResumeOutcome{success:true, deferred:true}` and spawns NO native child; the
    dispatcher settle seam (`failed = !matches!(success, Some(true))`) routes it to
    `done`, never `failed`. Idle (Mint) path serialized by a new blocking
    per-principal `SessionResumeLock` (flock LOCK_EX on `<store>/<principal>.lock`).
  - **M3** — new nullable `code_sessions.terminal_reason`; `terminal_reason_from_status`
    (`exited: N` / `signal: 9 (SIGKILL)`) + best-effort `record_terminal_reason`
    stamped at launch (~4185) and resume (~4188/8937) BEFORE the `?` early-exit, so a
    non-zero/killed child still records even when the bus refuses `session/stop`;
    teardown noise (summary ENOENT / auth refusal) stays an `eprintln!` warning.
  - **M4** — `refresh_stock_adapter_if_stale` at both exec sites (launch ~3916, resume
    ~8611) BEFORE the `exists()`-bail, reusing `initcmd::refresh_adapter_if_stale`/
    `is_adapter_stale` (now `pub(crate)`) + a new `stock_harness_source_binary` lookup;
    only for stock harnesses, fresh-inode + `set_executable`, no re-copy/log when current.
    Closes the observed still-stale installed adapter (a 12:30 launch still ENOENT'd).
  - Reconcile fix: the crashed worker's non-compiling `E0382` (secret move in the Mint
    branch) was corrected.
  - Verify (clean-context opus/high, isolated /tmp roots, no production broker):
    `cargo build` clean; `cargo test --lib --test-threads=1` = 645/645. The M2
    deferred-resume keystone was DRIVEN LIVE (a real live owner pid → deferred outcome,
    no spawn, on-disk credential + secret untouched). All broker/claude-dependent
    clauses audited to exact code sites. No defects; zero fix rounds. Verdict pass.
  - Deviations for Fable's attention: **none** from the two rulings — reuse-live is
    implemented AND, once M2 defers a live delivery to the inbox (never spawning),
    the live-owner case never reaches mint, so M1's secret-reuse is the (correct)
    unreachable safety net for that case; deliver-to-live queues to inbox exactly as
    ruled. `ResumeOutcome` gained a public `deferred: bool` (set false at all prior
    construction sites). Idle-resume policy = serialize/wait (R4 default), as proposed.

---
status: observed
observed: 2026-07-11
sessions: [code-79985d39, code-a26f933c, code-941b2685, code-d94cac68]
---

# Claude Code exited with a stale adapter and a retired live credential

This investigation records two errors that appeared together when the long-lived
Claude Code session `code-79985d39` disappeared:

```text
[code] writing adapter summary /Users/tim/.lanius/root/run/code-79985d39/adapter-summary.json failed: No such file or directory (os error 2)
[lanius] message bus REFUSED this session's credential (authorization refused: broker refused the connection (NotAuthorized)). This is an authorization failure, not a bus-down — it will keep failing until the credential is fixed; lanius will not silently retry past it.
```

They are separate lifecycle defects exposed during the same teardown. The summary
error is a known source-level race still running from a stale installed adapter.
The authorization error means the broker no longer had the token presented by
the session. The evidence strongly supports a token collision between a live TUI
and driven resumes of the same Lanius session. Neither message explains why the
Claude process itself exited; both are emitted after, or while reporting, other
session activity.

## Status and impact

- **Severity: high for long-running supervised sessions.** Once the token is gone,
  every hook publish is refused, so observation, bus-derived sibling awareness,
  and the final `session/stop` record are lost. The broker correctly refuses the
  stale secret and Lanius deliberately does not retry an authorization failure.
- The adapter-summary failure is not the cause of Claude's exit. It happens after
  Claude returns, while the adapter hands its final text and changed-file list to
  the parent. Its effect is to discard that completion summary.
- Files already written in the worktree and Claude's native transcript are not
  deleted by either path. If the host exits while a harness-native subagent is
  running, that work can still be interrupted or left without a completion
  record, as happened around this session's worker-DM work.
- The immediate cause of the Claude process exit remains unknown. There is no
  captured non-zero exit, signal, panic, or final `session/stop` for
  `code-79985d39`; the coding-session projection still calls it `idle` with no end
  time.

## Evidence timeline

The live root is `/Users/tim/.lanius/root`; the source tree observed here was
dirty with other in-flight work, so this report did not modify runtime code.

1. `code-79985d39` was created at `2026-07-09T21:35:20.316Z`. Its projection
   records native Claude session `c185ae6f-5648-4ae1-9487-742fc641e41a`, no end
   time, and three driven resumes.
2. The trace contains resumes of that same Lanius/native pair at
   `2026-07-11T01:21:17.914Z`, `01:38:01.484Z`, and `09:17:26.138Z`. The last
   successfully captured events are two `session/idle` records at about
   `09:20:54Z`; there is no later stop record.
3. The production broker log contains **1,003** refusals for
   `code-79985d39`, all in the explicit form
   `CONNECT refused: bad/unknown session token`. Other clients continued to
   reach the broker, ruling out a bus outage.
4. At investigation time both
   `/Users/tim/.lanius/root/.secrets/code-sessions/code-79985d39.json` and
   `/Users/tim/.lanius/root/run/code-79985d39/` were absent. Absence after the
   failure is consistent with retirement/cleanup; it cannot identify which
   operation removed the token by itself.
5. A fresh `lanius code claude --help` created `code-a26f933c`, connected to the
   bus, exited `0` after 3.015 seconds, and reproduced the same missing-summary
   error. It did **not** reproduce authorization refusal. This separates the
   summary bug from the credential bug.
6. A second fresh Claude/Opus xhigh investigation, `code-941b2685`, launched two
   Luna subagents and then hit Claude's account session limit. Its teardown
   reproduced the same summary ENOENT. This makes the summary failure three for
   three on the currently installed adapter, including successful and
   unsuccessful Claude exits.

## Recurrence: code-d94cac68 (2026-07-11 ~20:10)

The same failure took down a second long-lived interactive session hours after
this doc was first written — `code-d94cac68`, the session that authored the
original version of this report and the worker-surfaces bug doc. It crashed
mid-work; the human relaunched and the successor session (`code-85666abd`)
recorded this entry. State observed immediately after:

- `.secrets/code-sessions/code-d94cac68.json` is **absent** — only the fresh
  successor's credential exists. Same retired-credential signature as
  `code-79985d39`.
- `run/code-d94cac68/` still exists (mtime Jul 10 20:43, the launch time) but
  contains only `codex_home` and `skills` — no `adapter-summary.json`, and the
  parent never reaped the directory, so the parent-side read/cleanup at
  [codeagent.rs](../../src/codeagent.rs) never ran either.
- `elanus-serve.log` shows zero refusals for `code-d94cac68`; the refusal storm
  presumably went to the production broker log as before, or the hooks died
  with the host before retrying.
- The session had active edit claims on 12 files at crash time; those claims
  were still being advertised to peers ~30 minutes later, so a crashed session
  also leaves phantom claims behind (legibility side-effect worth folding into
  the fix).

This was an interactive TUI session with the human attached, like
`code-79985d39` — consistent with the live-TUI-plus-driven-resume collision
hypothesis (deliveries to this session while its TUI was live would each run a
driven resume). It raises the count to two long-lived supervised sessions lost
to this class in three days.

## Code path and causal analysis

### 1. The missing adapter summary is stale deployed code

The parent launcher creates `run/<session>/` and defines
`adapter-summary.json` before starting the adapter
([codeagent.rs](../../src/codeagent.rs), lines 3767–3771). After the adapter
exits, the parent reads the summary and removes the directory (lines 3982–3983).
The Claude adapter writes the summary only after `run_claude_capture` returns
(lines 4406–4426); the writer itself uses `std::fs::write` without creating a
parent directory (lines 7734–7743).

The current source already documents and fixes the old ordering error: the child
must not remove the shared scratch directory before the adapter writes into it
(lines 4367–4373; see
[small-fix-adapter-summary-race.md](../handoffs/small-fix-adapter-summary-race.md)).
That fix landed in commit `5c16270` with a regression test.

The live adapter predates the fix:

| Binary | Modified | Size | SHA-256 prefix |
| --- | --- | ---: | --- |
| `~/.cargo/bin/harness-claude` | 2026-07-09 15:50 | 3,471,408 | `06150643ae8c` |
| `~/.lanius/root/packages/harness-claude/bin/adapter` | 2026-07-06 23:29 | 3,471,040 | `1fe1ffa60d5c` |

The installed package therefore continues to run the pre-fix child cleanup.
The stale-adapter refresh fix also landed in `5c16270`, but it runs while stock
packages are seeded. Rebuilding or reinstalling Lanius does not by itself prove
an already-seeded live root was refreshed. The older mtime and different hash in
the live package are direct evidence that this root was not.

### 2. A resume can invalidate the credential of a live incarnation

Session credentials do not live in `run/<session>/`; they live in the fenced
store at `.secrets/code-sessions/<principal>.json`. The broker reads that file
on every `code-*` CONNECT and returns `NotAuthorized` when the file is absent or
its secret differs ([broker.rs](../../src/broker.rs), lines 262–270 and 593–598).
The missing summary directory therefore cannot directly cause authentication to
fail.

The likely invalidation is visible in the resume implementation:

- The initial launcher mints a token for principal `code-79985d39` and hands its
  secret to the long-lived adapter (codeagent.rs lines 3756–3765).
- A driven resume deliberately mints a new token for the **same principal**
  (lines 8520–8533).
- `mint` generates a new secret and atomically writes the one token path keyed by
  principal ([codesession.rs](../../src/codesession.rs), lines 2472–2500),
  replacing the secret held by any still-running incarnation.
- When the resume finishes, it unconditionally retires that principal by
  deleting the same file (codeagent.rs lines 8678–8683; codesession.rs lines
  2515–2520).

If a live TUI and a driven resume overlap, the resume first replaces the TUI's
secret and then removes the shared token file. The live hook environment keeps
presenting its original secret, so all subsequent CONNECTs receive exactly the
observed `bad/unknown session token`. `code-79985d39` had three such resumes and
then produced 1,003 refusals. This is the strongest supported causal explanation,
but the deleted historical token file means this investigation cannot prove the
exact delete operation after the fact.

The order of the two visible errors is consistent with teardown: the stale child
adapter fails to write its summary, then the parent attempts `session/stop` with
the launch token. The exact warning could also have come from a concurrent hook,
because both use the same credential. They are adjacent because of sequencing,
not because one file backs both mechanisms.

## Reproduction

### Confirmed: stale adapter summary

On the current live root, any short Claude launch is sufficient. Both of these
completed the child process and emitted ENOENT:

```sh
lanius code claude --help
lanius code claude --headless --model opus --effort xhigh "<read-only task>"
```

The first exited successfully, proving the ENOENT is teardown noise rather than
the reason Claude exits.

### Proposed: live-token collision

Do this only against an isolated root and broker:

1. Launch a Claude TUI through Lanius and keep it open.
2. Deliver to or explicitly resume that same Lanius session so
   `resume_capture` runs concurrently.
3. After the resume finishes, trigger any TUI hook event.
4. Assert that the original behavior returns `NotAuthorized`, then use the fix
   candidate to assert that both incarnations remain captured and one
   incarnation's cleanup cannot retire the other's credential.

This reproduction was not run against the production root because it
intentionally destroys a live session credential.

## Fix direction

1. **Refresh adapters at the use boundary.** Before launching a stock harness,
   compare the package adapter with the installed source binary (version/hash or
   at least mtime) and refresh stale adapters onto a fresh inode. Do not require a
   separate `lanius init` after upgrade, and retain the macOS no-copy-over-running-
   binary rule from
   [small-fix-adapter-refresh.md](../handoffs/small-fix-adapter-refresh.md).
2. **Make credentials incarnation-safe.** A live launch and a driven resume must
   not own one mutable token slot. Viable designs are multiple active token
   generations per principal, or reusing the live credential while its launcher
   is proven alive and minting an ephemeral token only for an actually idle
   session. Retirement must be compare-and-delete/generation-scoped so one
   incarnation cannot delete another's token.
3. **Gate or serialize resume as a first containment step.** Until credentials
   support overlap, refuse a driven resume while a live launcher/beacon owns the
   session, with an explicit explanation instead of silently invalidating it.
4. **Preserve the terminal reason.** Record a kernel-side stop/failure fallback
   when session telemetry is unauthorized, and distinguish Claude's exit status
   or signal from post-exit summary/auth failures. The current missing stop row
   makes the initiating failure unknowable.

## Verification

- Unit-test two token generations for one principal: retiring either generation
  leaves the other valid; a stale generation never authenticates after its own
  retirement.
- Integration-test a live TUI plus a driven resume against an isolated broker;
  assert both can publish throughout and that final cleanup leaves no credential.
- Test stock-harness launch with a deliberately older package adapter; assert it
  refreshes to a fresh inode before execution and the summary write succeeds.
- Keep the existing `run_claude_capture_leaves_scratch_for_the_parent_to_reap`
  regression test, then add a cross-process adapter test that asserts non-empty
  final text/file changes reach the parent and stderr contains no summary ENOENT.
- Force the Claude child to exit `0`, exit non-zero, and die by signal; assert the
  recorded stop reason is accurate even when bus publication is refused.

## Open questions

- What terminated the Claude process for `code-79985d39`? The native Claude
  transcript or terminal diagnostics may retain an exit/signal that Lanius did
  not capture.
- Which of the three driven resumes first invalidated the live incarnation, and
  why did some later activity remain captured? The current projection merges all
  incarnations under one session id, obscuring which process emitted each event.
- Should a delivery to a currently connected TUI be injected into that process
  instead of starting a parallel native resume? The answer changes whether
  concurrent credentials are normal operation or only a recovery case.

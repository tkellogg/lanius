# Handoff: uniform launch modes (TUI + headless) across coding harnesses

Make **every** coding harness (Claude Code, Codex, and the ones to come)
launchable in **both** modes — interactive **TUI** and **headless** — with the
*same* semantics and the *same* invocation, so adding the next harness is filling
in a small adapter, not reinventing the surface. This is the abstraction the
coding-agent envelope was always reaching for; today it's lopsided (Claude has
both modes, Codex only headless) and the CLI/briefing express it inconsistently.

> **Status: design + work plan. Not implemented.** Written 2026-06-20 from Tim's
> directive: "both claude and codex need to run in both modes, headless and TUI …
> the semantics for both modes should be the same across the board, or at least it
> should operate how you expect that harness to operate in that mode. We really
> need to make this smooth — I'll probably add other harnesses too."

## Read these first

- [coding-agents.md](coding-agents.md) — the envelope (one envelope, two adapters;
  cage, hook/stream record, mailbox, resume). Its *"Two operating modes, and the
  planner symmetry"* section is the seed of this model — but it assumes interactive
  TUI exists for both tools, which is **not true today** (Codex TUI was never
  wired). Appendix B is the Codex reference. This handoff supersedes that section's
  mode framing and makes it real + uniform.
- [coding-agent-dispatch.md](coding-agent-dispatch.md) — the agent-facing dispatch
  work (front door, spawn, capture, the projection). Its *"Two dispatch modes"* is a
  **different axis** than this handoff's (drive pattern, not launch mode — see the
  two-axes section below); both must agree on vocabulary.
- [../journeys/02-claude-code.md](../journeys/02-claude-code.md) — the why (a human
  at a TUI; a planner running headless).
- [../../src/codeagent.rs](../../src/codeagent.rs) — `Tool`/`Capture` enums,
  `launch`, `run_codex_capture`, the Claude `--worker` branch, `resume`,
  `briefing`, `print_help`. This is what the abstraction below refactors.

## Two axes, kept distinct (this is the crux)

The docs today blur two orthogonal things under the word "mode." Name them
separately and everything else falls out:

**Axis 1 — launch mode: how the harness *process* runs.**
- **`tui`** — the harness's native interactive terminal UI, inherited stdio, a
  **human drives the turns**. elanus owns the cage, *observes* (see capture matrix),
  and *injects per-turn context* — but cannot push a turn (no harness supports
  injecting into a running TUI). The human is the pump.
- **`headless`** — non-interactive, programmatic. One task in → result out,
  fully captured, scriptable. This is what `spawn`/`deliver`/workers use.

**Axis 2 — drive pattern: who advances turns over time, and how the result returns.**
Mostly relevant to headless (a TUI is human-pumped by definition):
- **one-shot (blocking)** — the caller runs the headless launch, **waits**, and
  reads the result inline (a worker called synchronously; `elanus code <h> --headless "task"`).
- **resumable (async)** — the session is durable; the daemon resumes it on a
  mailbox delivery (`deliver`/`spawn`/`resume`). The planner ends its turn and is
  woken later. This is the orchestration loop (M2/M4 of the envelope).

A **TUI session is also durable** and can be resumed headless later (same session
id) — coding-agents.md's "the same durable session bridges both." So the axes
compose: *launch mode* picks the process shape; *drive pattern* picks how turns
flow after launch. Keep these words straight in code, CLI, briefing, and docs.

## Capture matrix (harness × launch mode) — the real engineering

elanus must record what the session does, and **how it can observe differs by
harness and mode**. The obs grammar (`obs/agent/<noun>/<session>/…`,
`session/start|stop`, `tool/<n>/{call,result}`, `assistant/message`,
`session/idle`) stays uniform; only the *capture mechanism* per cell differs:

| harness | `tui` | `headless` |
|---|---|---|
| **Claude Code** | **HookBridge** — generated `--settings` hooks call `elanus code hook`; inherited stdio. *(built)* | **StreamJson** — `claude -p --output-format stream-json`, parse stdout. *(built, `--worker`)* |
| **Codex** | **RolloutImport** *(new)* — launch the real `codex` TUI (inherited stdio) and project its own rollout transcript `~/.codex/sessions/<…>/rollout-*-<thread_id>.jsonl` (the filename embeds the `thread_id` we already capture). Full fidelity, no hooks, no config pollution. *(Alt: HookBridge via a generated hook config + `--dangerously-bypass-hook-trust` — viable per Appendix B, but rollout-import is cleaner; pick during HM2.)* | **StreamJson** — `codex exec --json`, parse stdout. *(built)* |
| **future harness** | declared by its adapter (HookBridge / RolloutImport / Lifecycle) | declared by its adapter (StreamJson / Lifecycle) |

**Capture fidelity is a first-class, declared property** — not every cell is
hooks-grade. The floor is **Lifecycle** (just `session/start`+`session/stop`,
bracketed by the launcher) for any (harness, mode) with no better mechanism; an
adapter must *declare* its capture per mode so the UI/projection can show the
fidelity honestly rather than imply completeness it doesn't have. (`RolloutImport`
needs a small reader for codex's rollout JSONL; it can run during the session by
tailing the file, or once on `session/stop`.)

## Uniform CLI (operate how you'd expect the harness to behave)

Invocation is identical across harnesses; the prompt/flag semantics match what
each native tool does, so it "operates how you expect that harness to operate."

- `elanus code <harness>` → **TUI** (a human at a terminal). For every harness.
- `elanus code <harness> "<prompt>"` → **TUI seeded with that prompt** — matching
  the native tools (`claude "x"` and `codex "x"` both open the TUI with the prompt).
- `elanus code <harness> --headless "<task>"` → **headless one-shot**: runs the
  task, captures it, prints the result inline. (`--worker` becomes a deprecated
  alias of `--headless`.)
- `elanus code spawn <harness> "<task>"` → async headless (detached; result to the
  spawner's mailbox) — unchanged, but internally selects the headless mode.
- `elanus code resume <id> "<msg>"` / `deliver <worker> "<msg>"` → drive-pattern
  (headless resumable) — unchanged.

**Breaking changes to migrate (HM3):** today `elanus code codex "task"` is headless
and bare `elanus code codex` errors; under the uniform model a bare/prompt codex
launch is the **TUI**, and headless is `--headless`. So `spawn`/worker internals
must pass `--headless` (for both harnesses) instead of relying on the codex
positional / claude `--worker` split. Keep `--worker` as a back-compat alias.

## The harness adapter abstraction (so the next harness is easy)

Replace the hard-coded `Tool`/`Capture` enums + scattered `match self` with a
**Harness adapter** seam. The envelope — launch, per-session grant-scoped identity,
provider-cred scrub, obs grammar, mailbox, `spawn`/`deliver`, the projection, the
briefing — is harness-agnostic and parameterized by `(Harness, Mode)`. A new
harness implements one interface and registers; it does not touch the envelope.

Sketch (Rust; trait object or an enum-of-structs registry — implementer's call):

```
enum Mode { Tui, Headless }
enum Capture { HookBridge, StreamJson, RolloutImport, Lifecycle }

trait Harness {
    fn id(&self) -> &str;            // "claude" | "codex" | …  (CLI verb + alias set)
    fn agent_noun(&self) -> &str;    // "claude-code" | "codex"  (obs noun)
    fn binary(&self) -> &str;        // the real executable

    fn capture(&self, mode: Mode) -> Capture;            // the matrix cell
    fn command(&self, mode: Mode, ctx: &LaunchCtx) -> std::process::Command; // build argv+stdio+env per mode
    fn resume(&self, rec: &SessionRecord, msg: &str) -> ResumeSpec;          // daemon resume (headless)
    fn map_event(&self, src: EventSource, raw: &Value) -> Option<(String, Value)>; // hooks/stream -> obs leaf
}

fn harness(name: &str) -> Option<&'static dyn Harness>;  // the registry (claude, codex, …)
```

`LaunchCtx` carries the shared, harness-agnostic bits the envelope already
assembles: session id, scratch dir, the briefing text, scrubbed+set env, workdir,
prompt (for headless / TUI-seed), room, reply-to. The launcher picks `Mode` from
the CLI, asks the harness for the `Command` and the `Capture`, runs it inside the
envelope, and routes capture through the matching mechanism (hook bridge already
exists; stream-json already exists; add a rollout-importer; lifecycle is the
no-op-but-bracketed floor).

## Current state → target

| | claude tui | claude headless | codex tui | codex headless |
|---|---|---|---|---|
| **today** | ✅ | ✅ (`--worker`) | ❌ (errors / n/a) | ✅ (positional) |
| **target** | ✅ | ✅ (`--headless`) | ✅ (real TUI + rollout import) | ✅ (`--headless`) |

## Milestones

### HM1 — the adapter seam (refactor, no behavior change)
Introduce `Harness` + `Mode` + the `Capture` variants; move the two existing
adapters (Claude, Codex) behind it; the envelope calls `harness(name)` and
`capture(mode)`/`command(mode, ctx)`. Net behavior identical; the point is the
seam. **Acceptance:** all current `elanus code …` paths work unchanged; the
`Tool` match sites are gone; adding a harness is implementing one trait.

### HM2 — Codex TUI (the missing cell)
Wire `codex` in `tui` mode: launch the real interactive `codex` with inherited
stdio (like Claude's TUI), inside the cage, with the envelope briefing (Codex has
no `--append-system-prompt`; inject via the documented system/developer channel or
prepend). Capture via **RolloutImport**: resolve the session's rollout file from
the `thread_id` and project its turns into the obs grammar (a new reader; reuse
the projection's leaf vocabulary). Decide rollout-import vs hooks-via-bypass here
and record why. **Acceptance:** `elanus code codex` opens a usable codex TUI; the
session and its turns appear on the bus / in `elanus code sessions` with real
(not just lifecycle) fidelity.

### HM3 — the uniform CLI + mode flag
`--headless` (with `--worker` as a deprecated alias) across all harnesses; bare /
prompt → TUI for all harnesses; bare `codex` no longer errors. Migrate
`spawn`/worker internals to select headless via the new flag for both harnesses.
Update `print_help`. **Acceptance:** the same invocation grammar works for claude
and codex; `spawn codex` / `spawn claude` both run headless and route completions;
nothing regresses in M1–M4 obs or the dispatch tests.

### HM4 — briefing + docs reflect the uniform model
Rewrite `briefing()` to teach the uniform model (launch mode vs drive pattern; the
`--headless` flag; bare → TUI), and reconcile the dispatch handoff's "two dispatch
modes" vocabulary with this handoff's two axes. **Acceptance:** the briefing, help,
journeys, and handoffs all describe one model; no doc says "codex is exec-only" or
implies only Claude has a TUI.

### HM5 — "add a harness" readiness (the payoff)
A short template/checklist: implement `Harness`, declare the capture per mode,
register it, add the rollout/stream reader if novel. Validate by sketching a third
harness (e.g. an aider/opencode-style tool) against the trait without touching the
envelope. **Acceptance:** the checklist exists and a dry-run third adapter compiles
against the seam.

## Honesty / guardrails

- **Declare fidelity; don't fake it.** A `Lifecycle`-only or `RolloutImport`
  (post-hoc) cell must be visible as such (the projection/UI should not imply a
  codex TUI has the same live granularity as a hook-bridged Claude TUI).
- **Don't regress the headless obs.** Claude `-p` stream and codex `exec --json`
  are the verified high-fidelity headless paths (envelope M1); the refactor must
  preserve them and the projection that reads them.
- **Back-compat:** keep `--worker`; keep `elanus code <h> "task"`'s *result* still
  reachable (now via the TUI seeded with the prompt, or `--headless` for the old
  one-shot behavior) — and announce the codex-positional behavior change in help.
- **No new bus authority / cage change** — this is launch-shape + capture
  plumbing, not a permission or sandbox change (sandbox stance unchanged from
  coding-agents.md "one cage").

## Open questions

- Codex TUI capture: rollout-import (preferred — clean, full fidelity, but post-hoc
  unless tailed) vs hooks-via-`--dangerously-bypass-hook-trust` (live, but config
  to generate + trust caveats). HM2 picks one and records the trade.
- Codex rollout JSONL schema: confirm the per-turn/event shape so the importer maps
  cleanly onto the existing obs leaves (it should resemble `codex exec --json`).
- Tailing a rollout *live* (for a TUI session the human watches in the web UI in
  real time) vs importing once at stop — depends on the M3 live-feed work.
- Resuming a *TUI-started* codex session headless later (codex `exec resume
  <thread_id>`) — confirm a TUI session's thread is resumable by the daemon.

## Log

- 2026-06-20 — Written. Grounded checks: codex persists rollout transcripts at
  `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<thread_id>.jsonl` (filename embeds the
  thread id we already record → rollout-import is viable for codex TUI); codex
  exposes `--dangerously-bypass-hook-trust` + `-c`/`-p` config injection (the
  hooks-bridge alternative). Current state confirmed: bare `elanus code codex`
  errors (post dispatch-D2), `elanus code codex "task"` is headless exec, codex has
  no TUI path; Claude has both modes. The envelope's existing "two operating modes"
  framing (coding-agents.md) is the seed but assumes a codex TUI that was never
  built — this handoff makes it real and uniform, and is the canonical mode model
  the other coding-agent docs should defer to.

---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-21
---

# Handoff: onboard opencode as a third coding agent

Make `opencode` a first-class coding harness under elanus, the same way `claude`
and `codex` are: `elanus code opencode "<task>"`, captured to the bus, resumable
from the mailbox, briefed, dispatchable. Tim is already doing real work in opencode
("works well"); this brings it inside the envelope.

Answers the "I just added opencode … onboard that as another coding agent" item in
[../_questions.md](../_questions.md).

## Read the wonky bits first (decisions to confirm)

1. **The crux — do this *now* against the enum, or fold it into the harness-modes
   refactor?** [harness-modes.md](harness-modes.md) HM1 plans to **delete** the
   hard-coded `Tool`/`Capture` enums (`src/codeagent.rs:216`) for a `Harness` trait
   registry, and HM5 literally validates that seam by *"sketching a third harness
   (e.g. an aider/opencode-style tool)."* So opencode is the intended forcing
   function for HM1. Two orderings:
   - **(A) Headless-now, against the existing enum.** Add `Tool::OpenCode` + match
     arms + one stream parser (mirrors Codex). Ships a usable `elanus code opencode`
     in a day; the third concrete case then *de-risks* HM1 (a real third adapter
     beats a hypothetical sketch). Cost: you write `match` arms HM1 will refactor.
   - **(B) HM1-first, opencode as the first trait impl.** Cleaner end state, but
     couples "onboard the tool Tim is using today" to a larger refactor that also
     touches Claude + Codex.
   - **Recommendation: (A).** Tim wants opencode usable soon, and a working third
     adapter is the best possible input to HM1. Onboard headless against the enum;
     let it *drive* HM1 rather than wait on it. This handoff is written for (A) and
     notes the HM1 hand-off points.

2. **opencode's TUI capture is *better* than the codex TUI plan — adopt a new
   capture variant.** opencode is **client/server**: `opencode serve` exposes an
   HTTP + **SSE event stream**, and `opencode attach <url>` runs the TUI against a
   server. So a human-driven opencode TUI can be captured **live** off the server's
   event stream — no post-hoc import. Codex's TUI plan in harness-modes
   (`RolloutImport`) is *post-hoc* (read the rollout JSONL at stop). opencode gets a
   strictly nicer cell: a `ServerEvents`/SSE capture. The `Capture` matrix should
   grow this variant (and codex could later adopt it if it ever ships a server).

3. **The one real implementation unknown is the `run --format json` event schema.**
   Everything else is confirmed from the installed binary (1.17.9, see Log). The
   parser keys off the JSON event shape (message / tool-call / tool-result / session
   id / done), which I did not capture live (it spends a model call). **First task
   of OC1 is to pin that schema** from a real sample. The server's OpenAPI/SSE schema
   (`opencode serve` → its `/doc`) is the same event vocabulary and a no-model way to
   read it.

4. **No cage change — same posture as the others.** opencode brings its **own**
   sandbox/permission model (`--dangerously-skip-permissions` is its headless
   auto-approve). Per [coding-agents.md](coding-agents.md) "one cage," keep
   opencode's own containment active; do **not** bypass it onto elanus's write-only
   cage (that's the same deferred milestone gating Claude/Codex — `codeagent.rs:52-61`).
   This handoff is launch + capture plumbing only, no permission/sandbox change.

## What opencode gives us (installed 1.17.9, mapped to the seam)

The adapter surface a new tool must fill today (`src/codeagent.rs`): `parse` /
`agent_noun` / `from_agent_noun` / `binary` / `capture()->Capture` / `settings()` /
`map_event()`, plus a stream parser and a resume-command mapping. opencode fills it
like **Codex** (StreamJson, no settings, no home pollution):

| envelope need | opencode mechanism |
|---|---|
| **binary** | `opencode` (`/opt/homebrew/bin/opencode`) |
| **headless one-shot** | `opencode run --format json "<task>"` → **raw JSON events on stdout** (`--format` choices: `default`\|`json`). Same shape as `codex exec --json` / `claude -p --output-format stream-json`. |
| **capture (headless)** | `Capture::StreamJson` — a new `capture_opencode_stream` mirroring `capture_codex_stream` (`codeagent.rs:2100`) / `capture_claude_stream` (2733). |
| **settings / home pollution** | **None** — like Codex, no generated config. `--pure` runs without external plugins (the analog of Claude's `--setting-sources ''`) so the user's opencode plugins don't perturb a captured run. |
| **resume (durable session)** | `opencode run --session <id> "<msg>"` (or `--continue`); `opencode session` lists, `--fork` branches, `opencode export <id>` dumps JSON. Maps onto `resume`/`resume_capture` (`codeagent.rs:2553`/`2590`) and `resume_command` (2471). Capture the opencode session id off the JSON stream the way codex's `thread_id` is captured. |
| **TUI (human-driven)** | `opencode` (default) or `opencode <project>`; richer: `opencode serve` + `opencode attach <url>` → **live SSE event capture** (wonky bit #2). |
| **provider creds** | opencode brings its own (`opencode auth`/`providers`). The existing provider-cred scrub (`codeagent.rs:157`) must not leak elanus's DeepSeek/`ELANUS_*` env into it; confirm opencode's own auth (config/env) survives the scrub. |
| **headless auto-approve** | `--dangerously-skip-permissions` (a worker can't answer interactive permission prompts). |
| **obs noun** | `opencode` (CLI verb + obs `agent_noun`). |

Also present, not needed for v1 but worth knowing: `opencode acp` (Agent Client
Protocol server — a standardized editor↔agent protocol, an alternative
capture/drive path), `opencode web`, `opencode models`, `opencode stats`,
`opencode github`/`pr`. ACP is over-engineering for onboarding; `run --format json`
+ server-SSE are the pragmatic paths.

## Milestones

### OC1 — Headless opencode adapter (the usable win)
Add `Tool::OpenCode` to the enum and fill every `match self` arm (`parse` accepts
`"opencode"`/`"oc"`; `binary` = `opencode`; `capture` = `StreamJson`; `settings` =
`None`; `agent_noun` = `opencode`; `map_event` = a `generic_event`-style or
opencode-specific mapper). Write `capture_opencode_stream` modeled on
`capture_codex_stream`, parsing `opencode run --format json` into the obs grammar
(`tool/<n>/{call,result}`, `assistant/message`, `session/start|stop`). Scrub
provider creds; pass `--pure` and (for workers) `--dangerously-skip-permissions`.
- **First task: pin the `run --format json` event schema** from a real sample (or
  the server OpenAPI), then write the parser to it.
- **Acceptance:** `elanus code opencode --headless "<task>"` (and `spawn opencode`)
  runs, the turn's tools/messages appear on the bus and in `elanus code sessions`
  with StreamJson fidelity, and no elanus provider env leaks into the child.

### OC2 — Resume / mailbox drive
Capture the opencode session id from the stream into the `SessionRecord`; teach
`resume_command` to build `opencode run --session <id> "<msg>"`; wire
`resume`/`resume_capture` so a mailbox delivery resumes a durable opencode session.
- **Acceptance:** `elanus code resume <id> "<msg>"` and `deliver`/`spawn` round-trip
  through a real opencode session (same id), result returns to the spawner's mailbox.

### OC3 — TUI via the server event stream (live capture)
Add a `ServerEvents` (SSE) `Capture` variant. Launch opencode TUI wired to a server
elanus observes — `opencode serve` (note the port/hostname/`-p` auth) + the human's
TUI via `opencode attach`, or the default TUI if it exposes the same stream —
subscribe the SSE event stream, project events into the obs grammar **live**.
- **Acceptance:** `elanus code opencode` opens a usable TUI; its turns appear on the
  bus live (not post-hoc), with fidelity declared honestly (harness-modes "declare
  fidelity, don't fake it").

### OC4 — Front door: briefing, help, dispatch hint
List opencode everywhere the other two appear: `print_help`, `briefing()`, the
`DISPATCH_HINT` (`codeagent.rs:101`), and the dispatch front-door
([coding-agent-dispatch.md](coding-agent-dispatch.md)).
- **Acceptance:** `elanus code help` and the per-session briefing teach opencode with
  the same grammar as claude/codex; nothing implies it's second-class.

### OC5 (hand-off to HM1) — fold all three into the `Harness` trait
When [harness-modes.md](harness-modes.md) HM1 lands, opencode becomes the first
`Harness` trait impl and the `ServerEvents` variant joins the declared capture
matrix. This milestone is opencode's contribution to HM1's "add-a-harness"
checklist (HM5) — keep OC1–OC3 shaped so the trait extraction is mechanical.

## Read these first

- [harness-modes.md](harness-modes.md) — the adapter-seam refactor opencode is the
  forcing function for; the capture matrix this adds a cell + variant to. **The
  sequencing decision (wonky bit #1) lives here.**
- [coding-agents.md](coding-agents.md) — the envelope (cage, capture, mailbox,
  resume) opencode plugs into; "one cage" (the no-bypass posture, OC's wonky bit #4).
- [coding-agent-dispatch.md](coding-agent-dispatch.md) — the front door OC4 extends.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `enum Tool` (216), `Capture`
  (HookBridge/StreamJson, 265), `capture_codex_stream` (2100) and
  `capture_claude_stream` (2733) = the parser templates, `resume`/`resume_capture`
  (2553/2590), `resume_command` (2471), the provider-cred scrub (157), `briefing` /
  `print_help` / `DISPATCH_HINT`.

## Log

- 2026-06-21 — Written from introspecting the installed binary
  (`/opt/homebrew/bin/opencode`, **v1.17.9**) + the adapter seam. Confirmed:
  `opencode run --format json` emits raw JSON events on stdout (StreamJson, the
  Codex template — no hooks, no home pollution); `--session`/`--continue`/`--fork` +
  `opencode session`/`export` give first-class durable resume; `--pure` is the
  no-external-plugins analog of Claude's `--setting-sources ''`;
  `--dangerously-skip-permissions` is the headless auto-approve; opencode brings its
  own provider auth (existing scrub applies). Key finding: opencode is
  **client/server** (`serve` + SSE event stream, `attach`), so its TUI can be
  captured **live** — a strictly better cell than codex's post-hoc `RolloutImport`,
  warranting a new `ServerEvents` capture variant. The one open implementation
  unknown is the exact `run --format json` event schema (deferred to OC1's first
  task to avoid a speculative model call). opencode is the concrete third harness
  harness-modes HM1/HM5 was written to absorb.

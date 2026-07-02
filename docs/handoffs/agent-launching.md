---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: agents launching agents, first-class and easy

Tim's `_questions.md` lead item: "Let's say you're an agent, either coding agent
or native elanus agent. Can you launch elanus agents? … Can you launch one with
a certain package? Can you introspect available packages & profiles …? I want
all these things to be possible, maybe even easy."

The good news from grounding: **the machinery mostly exists — it's just
invisible to agents.** `elanus agent catalog / run / spawn` (`src/agentcli.rs`)
already inventories profiles+packages+tools, runs a blocking turn, and spawns a
durable background agent via its mailbox. But: (a) it is **undocumented** — the
only mention in all of docs/ is a one-line pointer at `docs/README.md:106`, and
`docs/handoffs/small-fixes.md:105` already flagged the gap; (b) a **native**
elanus agent has no tool for it — `tool_defs()` (`src/exec.rs:1495`) defines
exactly six tools and none launches an agent; (c) `--with-package` is a
visibility *assertion*, not an override — "launch one with package X" bails
unless the package is already on the profile's path (`agentcli.rs:252-271`);
(d) nobody can ask "what was that dead session doing to my files?" without
hand-crafting history queries. Per journey-14 doctrine, every one of these must
also be *taught* to the agent, or it doesn't exist.

## Wonky bits / decisions to confirm

1. **Ephemeral launch overrides are NOT config-repo proposals.** The config
   repo's proposal→accept flow (`src/config_repo.rs`) is for *durable* config
   changes with human review; "spawn a worker with package X just this once"
   is neither durable nor config. The simplest honest mechanism: `spawn`/`run`
   already carry the payload `{prompt, profile, session}` onto the mailbox
   (`agentcli.rs:132-146`) — add `with_packages` to that payload, and have the
   exec side extend the run's package visibility for **that run only** (the
   run-scoped analog of the profile's `elanus_path`), recorded on the run's obs
   trace for audit. Per Tim's doctrine, safety here is the *record*, not a
   gate — the launcher already has the authority to edit the profile durably,
   so a run-scoped extension grants nothing new; it just avoids a
   write-profile/launch/revert dance. Constraint: only **granted** (approved)
   packages may be added — this widens *visibility*, never *authority*
   (`packages::may` still gates what the package can do on the bus). *Fable:
   confirm run-scoped visibility extension over both alternatives (config-repo
   proposal = wrong tool; temp-profile-clone = litters `profiles/`).*

2. **One new native tool, `launch_agent`, not three.** Catalog introspection
   doesn't need a tool — the catalog is *context*, better served by the skill
   telling the agent to shell out (`elanus agent catalog --json`) or, for
   bus-only contexts, folded into a computed block later. The launch gesture
   does need a tool, because `spawn` requires ledger+mailbox plumbing an agent
   shouldn't hand-roll via `emit_event` (and `emit_event` refuses `in/*` except
   self, per timers M1 — so an agent literally *cannot* hand-emit another
   agent's mailbox today; `launch_agent` is the sanctioned door). Args:
   `{profile, prompt, with_packages?, session?}`; the arm reuses
   `agentcli::spawn`'s logic (gate on `matching_exec_handlers`, emit onto
   `topic::agent_mailbox`). Homogeneous authority: no permission model beyond
   what spawn already enforces; the audit is the ledger row + `created_by`.
   *Fable: confirm tool-for-launch, CLI-for-introspection split, and that
   `launch_agent` = spawn semantics (durable, async) — a blocking `run` from
   inside another agent's turn invites deadlock and is out of scope.*

3. **Coding agents get the CLI, not the tool.** A coding worker shells out;
   `elanus agent …` and `elanus code spawn …` are its interface. The gap there
   is (a) docs/help and (b) provider threading — and grounding shows provider
   threading is **already done**: `take_provider_flag` (`src/codeagent.rs:246`)
   is parsed once at `src/main.rs:1360` and threaded to *both* verbs — spawn
   re-injects `--provider <name>` before the tool token on the detached child
   (`codeagent.rs:986-988`) and launch takes it at `main.rs:1608`. So milestone
   (b) is **verify + document**, not build. What genuinely doesn't exist:
   `--provider` on **native** `elanus agent run/spawn` (`AgentCmd`,
   `src/main.rs:389-410`, has no provider arg) — the native analog of "spawn a
   worker on DeepSeek." Add it as a launch-time override in the mailbox
   payload, same shape as wonky bit 1. *Fable: confirm scoping "provider on
   spawn" as verify-for-coding + add-for-native.*

4. **`explain-session` is a skill that dispatches a reader, not a kernel
   feature.** The substrate exists twice over: the `history` package
   (`kits/stdlib/packages/history/` — a read-only HTTP daemon answering
   agents/sessions/transcript/search queries from sqlite) and the Rust
   projection (`code_projection::session_detail`, `src/code_projection.rs:893`,
   the full event timeline for a dead session). The skill teaches: given a
   session id (or a file whose history you want explained), spawn a reader —
   `elanus agent spawn` a native profile, or `elanus code spawn` a cheap
   worker — whose prompt says "query history for session X, explain what it was
   doing to <files>, mail me the explanation." Read-only by construction (the
   history daemon holds a `mode=ro` connection). This is exactly Tim's
   `_questions.md` sketch: "launch off a regular elanus agent … to read the old
   session file, figure out what it was doing with particular files, and
   explain." *Fable: confirm skill-over-kernel — no new query surface, the
   history DSL already covers it.*

5. **Teaching, per journey-14:** a `launching-agents` skill (the detailed
   how-to: catalog → pick profile → spawn, launch-time overrides, explain-
   session recipe, when to use `elanus agent` vs `elanus code`), real `--help`
   text on `elanus agent` and its subcommands (the agent already knows the CLI
   exists once the skill names it), and **no memory block** — launching agents
   is not high-availability "prompt customization"; the skill's one-line
   description is availability enough. Docs: a section in docs/README.md's CLI
   index + cross-links. *Fable: confirm no-block.*

**Product language.** These are builder/agent-facing surfaces; kernel words are
fine in the skill and CLI help. Nothing here touches the product interface
([../layering.md](../layering.md)).

## Milestones

### M1 — Introspection + launch documented and complete on the CLI
- Real `--help` on `elanus agent` and `catalog`/`run`/`spawn` (`src/main.rs:389-410`,
  `src/agentcli.rs:16-33`): what each does, spawn-ready vs run-only
  (`daemon_drivable`, `agentcli.rs:161-204`), the payload shape spawn emits.
- `catalog` already lists profiles + coding tools + providers (`agentcli.rs:36-91`);
  verify its `--json` is complete enough for an agent to choose (add package
  lists per profile if missing).
- Document the surface: a "launching agents" section in docs (README CLI index
  at `docs/README.md:106` grows from one line to an honest paragraph), closing
  the `small-fixes.md:105` gap.

**Acceptance:** `elanus agent --help` and each subcommand's `--help` describe
the verb, its args, and spawn's mailbox/exec-handler requirement; `elanus agent
catalog --json` gives a machine-readable inventory sufficient to pick a profile
and its packages (a test asserts the fields); docs/ grep for "agent spawn" now
hits real documentation. `cargo test` green.

### M2 — Launch-time overrides: `--with-package` becomes a run-scoped extension, `--provider` lands on native spawn
- Change `ensure_profile_packages` semantics (`src/agentcli.rs:252-271`): for
  `run`/`spawn`, a `--with-package` naming a **granted** package not on the
  profile's path no longer bails — it rides the mailbox payload
  (`agentcli.rs:132-146`) and the exec side (`src/exec.rs`, where the run's
  package visibility / `elanus_path` is assembled) extends visibility for that
  run only, with an obs record of the extension. An unapproved package still
  bails (visibility, not authority — wonky bit 1).
- Add `--provider <name>` to `RunOpts`/`SpawnOpts` (`agentcli.rs:16-33`,
  `AgentCmd` `src/main.rs:392-410`), carried in the payload and applied at the
  run's model resolution, mirroring how the dispatcher's `[model].provider`
  works (see [model-providers.md](model-providers.md) M3).
- Verify (don't rebuild) the coding side: `elanus code spawn` already threads
  `--provider` (`src/codeagent.rs:952,986-988`; `src/main.rs:1360,1407-1417`) —
  a live check that a spawned worker actually runs on the named provider.

**Acceptance:** `elanus agent spawn <profile> --with-package <granted-pkg>
--prompt …` runs with the package's skills/tools visible for that run and the
extension recorded on the run's obs trace; the same with an un-granted package
refuses; the profile's `profile.toml` is byte-unchanged after the run. `elanus
agent spawn --provider <name>` runs the turn on that provider (test via the
provider resolution seam). A live `elanus code spawn --provider …` check
confirms the coding path (log the result; no code change expected). `cargo
test` green.

### M3 — The `launch_agent` native tool
A seventh tool in `tool_defs()` (`src/exec.rs:1495`) + a `run_tool` arm
(`:1887`): `launch_agent{profile, prompt, with_packages?, provider?}` —
validates the profile, gates on `matching_exec_handlers`
(`src/packages.rs:498`) exactly as `agentcli::spawn` (`agentcli.rs:112-146`)
does (factor the shared core so CLI and tool cannot drift), emits onto
`topic::agent_mailbox(profile.agent)` with `created_by` = the launching agent,
and returns `{correlation, session, mailbox}` so the launcher can watch for the
reply (failure-mail on the correlation already covers the death case). Async
only (wonky bit 2).

**Acceptance:** a unit test — `launch_agent` from a running agent context emits
one mailbox event with the right payload + provenance and returns the
correlation; a profile with no exec handler returns a clear refusal (not a
silent drop); `with_packages`/`provider` ride the payload identically to M2's
CLI path (shared-core test). The timers-M1 `in/*` guard still refuses a raw
`emit_event` to another agent's mailbox (no regression; `launch_agent` is the
sanctioned door). `cargo test` green.

### M4 — The `explain-session` skill
A skill package (kits/, modeled on `kits/stdlib/packages/history/SKILL.md`)
teaching the recipe: identify the dead session (via history `sessions`/`search`
queries or `elanus code sessions`), dispatch a reader (`elanus agent spawn` or
`elanus code spawn` on a cheap tier) whose prompt directs it at the history
package's query DSL (`kits/stdlib/packages/history/SKILL.md:41-72`) /
`session_detail` timeline, scoped to the files in question, mailing back an
explanation. The skill states plainly: the reader explains intent, it cannot
change course (Tim's exact framing).

**Acceptance:** the skill exists and renders to agents (`elanus context render`
or the skill-visibility check); a live dry-run — dispatch a reader over a real
dead session from the ledger and get a mailed explanation naming what the
session did to a specific file (manual verification, logged in this handoff's
Log).

### M5 — Teach it (journey-14 doctrine)
The `launching-agents` skill (wonky bit 5): catalog → choose → spawn; run vs
spawn; launch-time `--with-package`/`--provider`; the `launch_agent` tool for
native agents vs the CLI for coding agents; pointer to `explain-session`. No
memory block. Cross-link from the existing `elanus` dispatch skill so a coding
planner discovers native-agent launching too.

**Acceptance:** skill visible to the intended profiles; a fresh agent given
only the skill can (in a scripted test or logged live run) go from "what
agents can I launch?" to a successful spawn without human help.

## Read these first
- The existing surface: `src/agentcli.rs` — opts `:16-33`, `catalog` `:36`,
  `run` `:92` (in-process `exec::run` `:95`), `spawn` `:112` (handler gate
  `:120-127`, mailbox emit `:132-146`), `ensure_profile_packages` `:252`,
  `profile_rows` `:161`; CLI enum `src/main.rs:389-410`, dispatch `:1133`.
- Introspection: `src/profilecli.rs:14` (`list`), `:74` (`get`);
  `src/packages.rs:67` (`discover`), `:71` (`discover_for_profile`), `:498`
  (`matching_exec_handlers`).
- The provider threading already done on the coding side: `src/codeagent.rs:246`
  (`take_provider_flag`), `:952` + `:986-988` (spawn re-injects), `src/main.rs:1360`,
  `:1407-1417`, `:1608`; [model-providers.md](model-providers.md).
- The tool seam: `src/exec.rs:1495` (`tool_defs`), `:1887` (`run_tool` match);
  the timers-M1 self-only `in/*` guard (why `launch_agent` must be a real tool):
  [timers.md](timers.md) wonky bit 1.
- The history substrate for M4: `kits/stdlib/packages/history/SKILL.md`
  (the query DSL `:41-72`), `src/code_projection.rs:893` (`session_detail`),
  `:882` (`list_sessions`).
- The doctrine: [../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md)
  ("tell the agent" — blocks sparingly, skills as expando-prompts, CLI help);
  [../_questions.md](../_questions.md) lines 6-9 and 29-36; the no-trust-
  boundary-between-own-agents stance (safety = audit, not restriction).
- The why-not-config-repo: `src/config_repo.rs` (proposals are durable config,
  not launch args); [configuration-ux.md](configuration-ux.md).

## Log
- 2026-07-02 — Implemented M1–M5. **M1:** real `--help` on `elanus agent` +
  `catalog`/`run`/`spawn` (args, spawn-ready requirement, emitted descriptor);
  `catalog --json` already carries per-profile package lists — a test
  (`agentcli::catalog_profile_rows_are_machine_pickable`) pins the fields; the
  README CLI index grew from one line to an honest "Launching agents" paragraph.
  **M2:** `--with-package` on `run`/`spawn` is now a run-scoped VISIBILITY
  extension — a granted package not on the profile's path rides the mailbox
  payload (`with_packages`) and `packages::discover_for_profile` unions it in for
  that run only via `ELANUS_WITH_PACKAGES` (env is the run-scoped bridge, same
  idiom as `ELANUS_ACTOR`/`ELANUS_CONFIG_DIR`); recorded on
  `obs/…/launch/with_packages`; an un-granted/uninstalled package refuses
  (`agentcli::validate_with_packages` + `packages::is_granted`). `profile.toml`
  is never written. Native `--provider` added to `RunOpts`/`SpawnOpts`/`AgentCmd`
  and applied by overriding `prof.model.provider` in `run_turn` (the same seam as
  the dispatcher's `[model].provider`). Coding-side `--provider` **verified by
  code inspection** (`codeagent::spawn` signature takes `provider`, re-injects
  `--provider <name>` before the tool token ~`:984`; `main.rs:1385`
  `take_provider_flag`) — no change; a live provider spawn is DEFERRED (needs real
  credentials + a daemon; must not run against the live root). **M3:** seventh
  tool `launch_agent{profile,prompt,with_packages?,provider?}` in `tool_defs`
  with a `run_tool` arm delegating to the shared `agentcli::spawn_core` (CLI and
  tool cannot drift); returns `{correlation,session,mailbox}`, `created_by` = the
  launching agent; the timers-M1 `in/*` `emit_event` guard was extracted to
  `emit_event_in_plane_refused` and unit-tested (self-mailbox allowed, sibling +
  owner-mail refused) — no regression. Tests: `spawn_core_emits_mailbox_event_
  with_overrides_and_provenance`, `spawn_core_refuses_profile_without_exec_handler`,
  `with_package_ungranted_bails_but_granted_passes`, `run_scoped_env_widens_
  visibility`, `is_granted_tracks_approval`. **M4:** `explain-session` skill
  (kits/stdlib) — the read-only-reader recipe over the history DSL /
  `session_detail`; renders (verified via `elanus agent catalog --json` in a
  scratch root). **M5:** `launching-agents` skill (kits/stdlib) — catalog→choose→
  run/spawn, `launch_agent` vs `elanus code`, launch-time overrides, pointer to
  `explain-session`; cross-linked from the coding-session `elanus` dispatch skill
  (`codeagent.rs` `ELANUS_SKILL`). No memory block (per wonky bit 5). DEFERRED:
  the live LLM dry-runs in M4/M5 acceptance (dispatch a real reader over a dead
  session; fresh-agent question→spawn) — both need a running daemon with provider
  credentials over a populated ledger, which the containment rules keep off the
  live root; mechanics are covered by unit tests + the render check. `cargo test
  --lib` green (434). NOTE: a pre-existing flaky `dev::first_free_port…` test can
  fail under a full parallel `cargo test` (port race) but passes in isolation —
  unrelated to this work.
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull. Grounded
  against the worktree: `elanus agent catalog/run/spawn` fully exists but is
  undocumented (one line at `docs/README.md:106`); no native tool launches an
  agent (six tools in `tool_defs`); `--with-package` asserts visibility rather
  than granting it (`agentcli.rs:264-270`); coding `spawn` already threads
  `--provider` (so that sub-item is verify-only); the history package + 
  `session_detail` cover explain-session with zero new query surface. Judgment
  calls for Fable: run-scoped package-visibility extension via the spawn
  payload, not config-repo proposals or temp profiles (1); one async
  `launch_agent` tool, introspection stays CLI (2); native `--provider` added,
  coding `--provider` verified (3); explain-session as a skill over the
  existing history DSL (4); skill + CLI help, no memory block (5).

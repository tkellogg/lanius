---
status: implemented (M1–M4)
author: Opus (planner) under Fable; implemented by Opus (implementer)
last-updated: 2026-07-02
---

# Handoff: the user's MCP servers survive an elanus launch

Tim's backlog ([../_questions.md](../_questions.md)): "Each coding agent's
native MCP servers seem to not be able to load on launch. At least claude code
& codex." The isolation that elanus deliberately applies at launch — so a
session doesn't drag in the user's global hooks/plugins — currently throws the
user's **MCP servers** out with the bathwater. Grounding confirms the shadowing
for two of three harnesses and complicates the third:

- **claude — confirmed.** The launch passes `--setting-sources ''`
  (`src/codeagent.rs:3424-3425` worker, `:3492-3493` interactive; rationale
  comment `:3417-3419`), never passes `--mcp-config`, and the generated
  `--settings` file contains **only hooks** (`claude_settings()`,
  `codeagent.rs:7161-7194`; the test at `:7752-7761` asserts exactly one
  top-level key). Claude Code discovers MCP servers via user/project settings
  and `.mcp.json` — all disabled. Zero MCP servers load. 
- **codex — hypothesis refuted on paper; needs live root-cause.** elanus
  *does* override `CODEX_HOME` to a per-session dir (`:3776` headless, `:3917`
  TUI) — but `build_codex_skills_home()` **copies the user's real
  `~/.codex/config.toml` verbatim** into it (`:1328-1337`), symlinks
  auth (`:1313-1326`), and `append_codex_hook_config()` only *appends* a hooks
  block (`:1345-1358`). `[mcp_servers]` lives in that copied file, so it
  *should* load. If Tim still sees no codex MCP, the real culprit is
  something else — a scrubbed child env (`PROVIDER_CRED_VARS` etc.) breaking
  the MCP server's own command/env, project-local codex config not being
  copied, or relative paths resolving against the new home.
- **opencode — confirmed.** `--pure` is passed (`:4585`; comment `:4541-4542`
  calls it "the analog of Claude's `--setting-sources ''`") and
  `OPENCODE_CONFIG_DIR`, when set at all, points at a skills-only scratch dir
  (`:4572`, `:4636`) — the user's `~/.config/opencode` (where MCP lives) is
  never read.

## Wonky bits / decisions to confirm

1. **Merge, don't reopen the floodgates.** The isolation exists for good
   reasons (the user's global hooks would double-fire against elanus's hook
   bridge; plugins pollute capture; provider creds are scrubbed on purpose —
   `docs/coding-harness-onboarding.md:129-134` codifies it as requirement #2).
   The fix is **selective merge**: carry over exactly the user's **MCP server
   registrations**, keep excluding everything else (their hooks, their
   plugins, their misc settings). Concretely per harness: claude — read the
   user's real MCP registrations and hand them back via a dedicated channel
   (`--mcp-config <generated-file>`, which composes with `--setting-sources
   ''`; verify flag semantics against the installed CLI before building — the
   hard-won onboarding caveat at `coding-harness-onboarding.md:190-193` says
   smoke-test the lever, never trust memory); codex — already copied, fix
   whatever M1 finds; opencode — generate a session config that includes the
   user's `mcp` section while keeping `--pure`'s plugin exclusion (verify
   `--pure`'s exact blast radius live — if it also kills config-file MCP
   entries, the merge must ride whatever channel survives `--pure`, or
   `--pure` gets replaced by targeted exclusions). *Fable: confirm
   MCP-only merge, everything-else-still-excluded.*

2. **Where the user's claude MCP registrations actually live must be pinned
   in M1, not assumed.** Claude Code has had several homes for MCP config
   (`~/.claude.json` per-user/per-project entries, project `.mcp.json`,
   settings-file `mcpServers`, `claude mcp add` state) and they vary by
   version. M1 enumerates what the *installed* version reads (a scratch-root
   live test with `claude mcp add` + a trivial stdio server), and the merge
   reads from that source-of-truth, with a plain "couldn't read your MCP
   config" stderr note when parsing fails (never a launch failure).
   *Fable: confirm.*

3. **An MCP server is user-authority, not elanus-authority — no gate, but a
   record.** Per Tim's doctrine (safety = audit, not restriction; no trust
   boundary between the user's own agents), elanus does not approve/filter
   which of the user's MCP servers load — the user configured them for this
   tool; an elanus launch shouldn't be the place they silently vanish OR get
   permission-prompted. But the launch should *record* what was merged (an
   obs note on `session/start` listing merged server names) so a session's
   capabilities are reconstructable from its trace. *Fable: confirm
   record-not-gate.*

4. **The env scrub is a live suspect for codex (and for any stdio server).**
   MCP servers are child processes of the harness; the harness is a child of
   elanus with a scrubbed env (`PROVIDER_CRED_VARS`, `codeagent.rs:214-216`,
   plus whatever else the launch scrubs). A server whose command needs a
   scrubbed var (or an npm-installed binary needing the user's PATH) dies at
   spawn — which *looks like* "MCP didn't load". M1's matrix must include an
   env-sensitive server to catch this class; the fix must not un-scrub
   provider creds (that's load-bearing for nesting — model-providers), so if
   env is the culprit the answer is scoping the scrub, not removing it.
   *Fable: confirm scrub stays, scope adjusts only with evidence.*

## Milestones

### M1 — Reproduce + root-cause, per harness (live, on a scratch root)
Build a trivial stdio MCP server (a few lines — a `scratch-mcp` echo tool)
plus one env-sensitive variant. On a scratch root: register it natively with
each tool (claude: the installed version's real mechanism, wonky bit 2;
codex: `[mcp_servers]` in `~/.codex/config.toml` — pointed at a scratch
CODEX_HOME-as-user-home so the real `~/.codex` is never touched; opencode:
its config in a scratch `OPENCODE_CONFIG*`). Verify the server loads under a
**bare** tool launch, then under `elanus code <tool>` (TUI + headless), and
record per cell: loaded / not loaded / spawn-failed, with the root cause
(flag shadowing, config not read, env scrub, path). For codex specifically,
determine why Tim's servers don't load despite the config copy.

**Acceptance:** a written per-harness root-cause table in this handoff's Log
(harness × mode × {bare, elanus} × {plain server, env-sensitive server}),
each "not loaded" cell explained with a file:line or an observed spawn error.
No production code changes in M1. All scratch processes killed; the user's
real `~/.claude`, `~/.codex`, `~/.config/opencode` untouched.

### M2 — Merge-not-replace, claude
Generate a per-session MCP config from the user's registrations (M1's
source-of-truth) and pass it through the channel that composes with
`--setting-sources ''` (expected: `--mcp-config`; verified in M1). The
generated `--settings` file stays hooks-only (the `:7752` test's invariant
holds); elanus's hook bridge, plugin dir, and briefing are unchanged. Merge
failure degrades to today's behavior with one loud stderr line.

**Acceptance:** on a scratch root with a user-registered scratch MCP server:
an `elanus code claude` session can call the server's tool, and the captured
session shows it — the PreToolUse hook capture carries the `mcp__<server>__
<tool>` tool name (`tool_name()`, `codeagent.rs:7243-7247`) in the obs trace;
the session/start obs lists the merged server names (wonky bit 3); a launch
with NO user MCP config is byte-identical to today's. Existing settings tests
green; `cargo test` green.

### M3 — Merge-not-replace, codex + opencode
- codex: fix M1's actual root cause (candidates: copy project-local config
  too; scope the env scrub, wonky bit 4; path handling under the session
  home). The config-copy + hook-append design (`:1298-1358`) stays.
- opencode: carry the user's MCP entries into the session config while
  keeping plugin exclusion (per M1's finding on `--pure`'s exact semantics).

**Acceptance:** the same live test as M2, adjusted per harness to what each
harness's cage actually permits (M1 finding):
- **codex** — the user-configured scratch MCP server **loads** in an `elanus
  code codex` session (the config copy carries `[mcp_servers]`) and its name
  appears on the `session/start` obs record. A *completed* tool CALL is
  asserted in the **interactive TUI** cell (where the user approves — the obs
  mapping `mcp_tool_call` → `tool/<name>/result` with `body["server"]`,
  `codeagent.rs:6119-6135`, test pattern at `:7909-7923`). The headless
  `codex exec` cell auto-cancels the tool CALL under codex's default
  approval/sandbox (the server still loads) — this is a codex cage-policy
  residual escalated to Fable/Tim (see "codex residual" in the Log), NOT a
  merge defect, and elanus does not silently bypass approvals to satisfy it.
- **opencode** — the server is callable in a captured `elanus code opencode`
  session (generic tool capture, `opencode_map_event` `:5585`).
The env-sensitive server case from M1 either works or fails with a
**visible** captured error (no silent absence). `cargo test` green.

### M4 — The capability honesty pass
Update `docs/coding-harness-onboarding.md` requirement #2 (`:129-134`) to
state the refined contract: isolate hooks/plugins/settings, **merge MCP
registrations** (with the per-harness channel), and record merged servers on
session/start — so the next harness adapter (pluggable-coding-harness) builds
merge-not-replace from day one. Note it in the harness checklist (`:168-180`).

**Acceptance:** the onboarding doc names the MCP-merge requirement and the
per-harness mechanism table matches what M2/M3 shipped.

## Read these first
- The launch isolation being refined: `src/codeagent.rs` — claude
  `--setting-sources ''` `:3424-3425`/`:3492-3493` (comment `:3417-3419`),
  `claude_settings()` `:7161-7194` + invariant test `:7752-7761`; codex
  `CODEX_HOME` override `:3776`/`:3917`, `build_codex_skills_home` `:1298`
  (config copy `:1328-1337`, auth symlinks `:1313-1326`),
  `append_codex_hook_config` `:1345-1358`, `dirs_next_home_codex`
  `:1391-1398`; opencode `--pure` `:4585` (comment `:4541-4542`),
  `OPENCODE_CONFIG_DIR` `:4572`/`:4636`; the env scrub `:214-216`.
- How MCP shows in capture (the acceptance seam): codex `mcp_tool_call`
  mapping `:6119-6135` + test `:7909-7923`; claude `tool_name()`
  `:7243-7247`; opencode `opencode_map_event` `:5585`.
- The isolation doctrine being preserved: [../coding-harness-onboarding.md](../coding-harness-onboarding.md)
  requirement #2 (`:129-134`), the smoke-test-the-lever caveat (`:190-193`);
  [coding-agents.md](coding-agents.md) Appendices (per-tool launch contracts);
  [pluggable-coding-harness.md](pluggable-coding-harness.md) (the adapter SDK
  this contract feeds).
- Why the scrub can't just go away: [model-providers.md](model-providers.md)
  (the scrub enables provider nesting).
- Tim's stance on gating: safety = audit, not restriction (record merged
  servers, don't approve them).

## Log

### 2026-07-02 — M1 live root-cause matrix (implementer)
Tooling: a trivial stdio MCP server (`scratch_ping`, node) + an env-sensitive
variant (`SCRATCH_MCP_REQUIRE_ENV=1` needs `SCRATCH_MCP_SECRET` to spawn).
Installed versions: Claude Code **2.1.198**, codex **0.142.5**, opencode
**1.17.9**. Every real dir was left untouched (scratch HOME / CODEX_HOME /
OPENCODE_CONFIG / project-scope `.mcp.json`).

| harness | mode | bare tool | under `elanus code` | root cause |
|---|---|---|---|---|
| **claude** | headless | **loads + callable** with `--mcp-config <file>` even under `--setting-sources ''` (`pong: alpha`) | **was NOT loaded** — elanus passed `--setting-sources ''` and no `--mcp-config`; user/project settings + `.mcp.json` all disabled | **flag shadowing** (`codeagent.rs` claude launch). FIXED in M2. |
| claude | TUI | same as headless (`--mcp-config` composes) | same shadow, same fix | — |
| **codex** | headless (`exec`) | server **starts** (`mcp: scratch/scratch_ping started`) but the tool CALL is **auto-cancelled** ("user cancelled MCP tool call") under codex's default approval/sandbox; only `--dangerously-bypass-approvals-and-sandbox` lets it `completed` | identical — `build_codex_skills_home` copies `config.toml` verbatim, so `[mcp_servers]` **loads**; same headless cancel | **NOT a shadow** — the config copy carries MCP and the server loads. The cancel is codex's non-interactive approval model, which elanus deliberately does not bypass (cage; `codeagent.rs:3695`). See "codex residual" below. |
| codex | TUI | loads; interactive approval lets the call run | same (copy carries it) | works interactively. |
| **opencode** | headless (`run --pure`) | **loads + connects** — `--pure` disables ONLY plugins (per `--help`); `OPENCODE_CONFIG`/global `mcp` block is read | **loads + connects** under the exact elanus posture (`--pure` + `OPENCODE_CONFIG_DIR` override): a proof with an XDG-global `mcp` server showed it still `✓ connected` | **NOT shadowed** on 1.17.9. `OPENCODE_CONFIG_DIR` only adds a skills-scan dir; it does NOT shadow config-file MCP. The handoff's "opencode — confirmed" was **wrong on the ground** for this version. |
| env-sensitive server | any | spawns iff its env var is present | elanus scrubs only `PROVIDER_CRED_VARS` + launch-control (not PATH/HOME), so a node/PATH server spawns fine; a server needing a scrubbed provider var would die at spawn | scrub stays (load-bearing for nesting); no evidence any real server needs a scrubbed var. |

**Net:** only **claude** had a genuine MCP *load* shadow. codex and opencode both
LOAD the user's MCP already; the handoff over-claimed their shadowing.

**codex residual (not fixed, by design):** `codex exec` (the headless/captured
cell) auto-cancels MCP tool CALLS unless approvals+sandbox are fully bypassed.
That is a cage-policy decision (the launch deliberately does NOT bypass —
`codeagent.rs:3695`), not a config shadow, so it is left for Fable/Tim: MCP calls
are user-authority (record-not-gate) and could be auto-approved for MCP
specifically, but changing the sandbox posture is out of this handoff's safe
scope (wonky bit 4: posture changes only with a decision, not silently). The
interactive TUI already works (the user approves). Recorded here so it is not
silently lost.

### 2026-07-02 — M2/M3/M4 implemented
- **M2 claude:** `write_claude_mcp_config()` reads `~/.claude.json` `mcpServers`
  (user-scope registry, pinned live) into a per-session `mcp-config.json`, passed
  via `--mcp-config` in BOTH the worker and interactive claude launches. The
  `--settings` object stays hooks-only (invariant test still green; new test
  `settings_stay_hooks_only_even_with_mcp_merge`). None ⇒ no flag ⇒
  byte-identical. Verified live: the flag composition calls the tool (`pong`),
  and `elanus code claude` now emits `--mcp-config` with the server carried
  verbatim (fake HOME so `~/.claude.json` was untouched).
- **M3 codex:** no code change — the config copy already carries `[mcp_servers]`
  and the server loads (M1). The session/start record covers it.
- **M3 opencode:** no launch change — the user's MCP already loads under the
  elanus posture (M1). Adding an `OPENCODE_CONFIG` override would RISK dropping
  the user's model/provider config, so it was deliberately NOT done.
- **record-not-gate (wonky bit 3):** `merged_mcp_server_names(tool)` reads each
  harness's user MCP registry and the launcher stamps `mcp_merged: [names]` on
  the `session/start` obs — for all three harnesses.
- **M4 docs:** `coding-harness-onboarding.md` requirement #2 now carries the
  MCP-merge contract + the per-harness mechanism table.
- Tests: `mcp_servers_from_json_tolerates_shape`, `strip_jsonc_comments_*`,
  `write_claude_mcp_config_none_when_no_user_servers`,
  `merged_mcp_names_unknown_tool_is_empty`, plus the settings invariant. `cargo
  test --lib` = 437 passed.

### 2026-07-02 — codex residual RESOLVED (Tim's ruling: headless auto-approve)
The "codex residual" above (M3 Log entry) escalated one question to Fable/Tim:
`codex exec` auto-cancels MCP tool CALLS under its default approval/sandbox, and
elanus deliberately did not silently bypass approvals to fix it. `docs/
notes-headless-elicitation.md` researched whether elicitation (pause, ask a
human, resume) is possible for this transport instead of blanket auto-approve —
finding it is **architecturally impossible** for `codex exec`: no TTY, no
approval callback, stdin is reserved for the prompt/briefing; only a full
transport swap to `codex app-server`'s bidirectional JSON-RPC (future work, not
this fix) could hold a decision open (§2–3 of that doc). Per its doctrine
(§2, "General principle for 'elicitation impossible' cases"): auto-approve at
the tightest scope the harness offers, and record the ungated fact in the
audit trail.

Tim's ruling: headless codex workers auto-approve so MCP tool calls complete,
scoped as tightly as possible, and the ungated posture is RECORDED.

Implemented in `src/codeagent.rs`, headless path (`run_codex_capture` /
`codex_headless_base_args()`) ONLY — the interactive TUI (`run_codex_tui_import`
/ `codex_tui_base_args()`) is untouched; the human approves there. Live-tested
against a scratch stdio MCP server (`scratch_ping`, one `ping` tool) on a
scratch `CODEX_HOME`, four postures:
- default (no override): MCP call auto-cancels ("user cancelled MCP tool call").
- `-c approval_policy=never` alone: **still** auto-cancels.
- `-c approval_policy=never -c sandbox_mode=workspace-write`: **still**
  auto-cancels — `workspace-write` is not sufficient.
- `-c sandbox_mode=danger-full-access` (with or without `approval_policy=never`):
  the call **completes** (`"status":"completed"`, `pong: <arg>` returned).

So `danger-full-access` is not a discretionary escalation — it is the floor
`codex exec` requires before a headless MCP tool call can complete at all;
`workspace-write` (the narrower sandbox originally hoped for in `docs/
notes-headless-elicitation.md` §2) does not unblock MCP calls. elanus passes
both `-c` overrides explicitly (`approval_policy=never`,
`sandbox_mode=danger-full-access`) rather than
`--dangerously-bypass-approvals-and-sandbox`, so the posture stays a legible,
scoped config override instead of the special-cased CLI bypass (which also
disables unrelated checks). The posture is stamped on the `session/start` obs
record (`"approvals": "auto", "sandbox": "danger-full-access"`, alongside
`mcp_merged`) for every headless codex launch, so a session's ungated
authority is reconstructable from its trace — never silent. See
`docs/security.md` (new entry) for the ledger record. Revisit when the
`codex app-server` driver lands (§3 of `notes-headless-elicitation.md`) — that
is the actual fix for elicitation, not this stopgap.

### 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull. Grounded
  against the worktree: claude's shadowing is total and deliberate
  (`--setting-sources ''`, hooks-only settings, no `--mcp-config`); opencode's
  `--pure` + scratch config dir likewise; but codex **copies the user's
  `config.toml` verbatim** (so `[mcp_servers]` should load), which refutes the
  simple hypothesis for one of the two harnesses Tim named — hence M1 is a
  real reproduction matrix, with the env scrub and project-local config as
  live suspects. Judgment calls for Fable: MCP-only selective merge, all
  other isolation intact (1); pin the installed claude version's MCP config
  source live, don't trust memory (2); record-not-gate for merged servers
  (3); the provider-cred scrub stays, scoped only with evidence (4).

---
status: planned
author: Opus (planner) under Fable
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

**Acceptance:** the same live test as M2 per harness: a user-configured
scratch MCP server is callable in a captured `elanus code codex` session
(assert the obs mapping — `mcp_tool_call` → `tool/<name>/result` with
`body["server"]`, `codeagent.rs:6119-6135`, test pattern at `:7909-7923`) and
in an `elanus code opencode` session (generic tool capture,
`opencode_map_event` `:5585`). The env-sensitive server case from M1 either
works or fails with a **visible** captured error (no silent absence).
`cargo test` green.

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
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull. Grounded
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

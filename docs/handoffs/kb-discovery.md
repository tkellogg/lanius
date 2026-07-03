---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: kb-discovery — "you don't have X enabled, but it exists and matches"

Decomposed from [knowledge-base.md](knowledge-base.md) build step **B7** (ruling
D7). The privileged discovery package: **a package that supplies a privileged
tool** (Tim's own framing) which searches **available** (not just enabled)
packages across everything they can carry — `kb/`, `SKILL.md`, tools, stages,
harnesses, MCP servers — and answers "you don't have the discord package enabled,
but it exists and matches your query." It is **privileged** because it reads the
instance's **package universe** (`packages::discover`) rather than the agent's
own **visibility set** (`discover_for_profile`). Enablement then rides the
existing config-proposal machinery. Lands last (D7 rationale: it needs the union
to be worth searching); depends on [kb-core.md](kb-core.md) and on
**[kb-search.md](kb-search.md) M0 — the `[[tool]]` manifest seam** — through
which this package supplies its tool.

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **Primary surface: a `[[tool]]` named `find_capability`, supplied through the
   kb-search M0 seam. [FABLE RULING 2026-07-02 — replaces my CLI-primary call.]**
   My original call made a privileged `elanus discover` CLI the primary surface
   (with an optional MCP wrapper), because packages could not supply tool
   definitions. Fable's ruling lands the `[[tool]]` seam in kb-search M0 and
   directs discovery to use it — Tim's words were "a package that supplies a
   privileged tool." So: the `discovery` package declares `[[tool]] name =
   "find_capability"` (`query` → matches across available packages), exec-mode
   dispatch per the seam's contract. **The CLI (`elanus discover <query>`), the
   skill, and the high-awareness block stay as secondary surfaces** — the CLI
   both serves humans/scripts and is what the tool's exec script shells out to
   (next bit).

2. **The privileged read lives in the kernel CLI; the tool script wraps it.**
   `packages::discover` (`src/packages.rs:67` — the whole universe, no profile
   filter) versus `discover_for_profile` (`:71` — the agent's filtered visible
   set) is kernel code; a package's exec-mode tool script cannot link it. **My
   call: implement the universe scan once as `elanus discover <query> --json`
   (kernel CLI), and the package's `[[tool]]` script is a thin wrapper that
   shells it and reshapes the JSON into the tool result.** One implementation,
   three surfaces (tool, CLI, skill-taught). *Fable: confirm the
   thin-wrapper-over-CLI shape — the alternative (the script re-scanning package
   dirs itself) duplicates manifest parsing in a package script and drifts.*

3. **Privilege is about *visibility*, not *authority* — and the grant covers
   it.** Reading the universe is a read of package metadata on disk; it grants
   nothing. The existing invariant: `ELANUS_WITH_PACKAGES` / run-scoped extension
   widens **visibility** only — "the grants ledger still gates what it may do on
   the bus, so this is never an authority grant" (`src/packages.rs:150-151`).
   Discovery is the read-only end of the same distinction: it tells an agent a
   capability *exists*; getting it still goes through proposal + grant. Under
   the M0 seam, supplying `find_capability` is itself **grant-gated** (kind
   `"tool"`), so the human explicitly approves this package's privileged
   surface into existence — the right place for the "privileged" property to be
   visible and decided.

4. **Teaching the tool's own availability (journey-14, the crux of D7).**
   Discovery's whole reason to exist is that an agent **doesn't know** a
   capability exists — so *discovery's own* availability cannot itself be
   discovered; it must be **taught**. Per journey-14's tiers: the tool's
   presence in the array is itself high-availability once granted; a **skill**
   makes the pattern legible (when to reach for it, what the results mean); and
   a **seeded high-awareness memory block** on the default/dispatching profile
   says "if you lack a capability for the task in front of you, use
   `find_capability` — packages you don't have enabled may carry it." **My
   call: ship all three** (tool + skill + block), so a fairly default agent
   simply knows discovery is there (D6's "melded" bar).

5. **Enablement rides the config-proposal flow (verified).** The output tells the
   agent: to get package X, propose enabling it. The existing machinery
   (`src/exec.rs:258-332`): the agent writes to its disposable config clone
   (`$ELANUS_CONFIG_DIR`) and commits a `proposal/<id>` branch; `reap_proposals`
   (`src/config_repo.rs:479`) harvests it; it records `obs/config/proposed`; then
   `configcli::classify` accepts (autonomy) or holds it for the human. Enabling a
   package means adding it to the profile's `elanus_path` (or the equivalent
   config change). Discovery does **not** invent a new enable path.

## Milestones

### M1 — the privileged catalog read: `elanus discover <query>` (kernel)
Add `elanus discover <query>` (a `Cmd::Discover`, or a `KbCmd::Discover` under
the kb-core `Cmd::Kb`) that runs `packages::discover` over the **whole
universe** and, for each package **not** in the caller's visible set, scans
everything it carries — `kb/` files, `SKILL.md` (name+description),
`provides_builtin_tools`, `[[tool]]`, `[[stage]]`, `[[mcp]]`, `[[harness]]` —
for a match against the query. It reports each hit as "package `<name>` (not
enabled) carries `<what>` matching `<query>`; enable by proposing it to your
profile." `--json` for machine use (the tool wrapper's input, M2).

**Acceptance:** on a scratch root with a `discord` package present but **not**
in the agent's profile, `elanus discover "discord api"` names the discord
package, what enabling it would add (its `kb/discord-api-notes.md`, its skill,
any tools), and the enable path. A capability already visible to the agent is
not re-surfaced as "missing." `--json` output is machine-stable.

### M2 — the discovery package: the `find_capability` tool + taught availability
Ship the `discovery` package (stdlib) declaring `[[tool]] name =
"find_capability"` through the kb-search M0 seam — its exec script a thin
wrapper over `elanus discover --json` (wonky bit 2) — plus the skill and one
seeded high-awareness memory block on the default/dispatching profile (wonky
bit 4). Supplying the tool is grant-gated (kind `"tool"`, M0); the human
approves the privileged surface explicitly.

**Acceptance:** with the package approved + visible, an agent's tool array
carries `find_capability`; a call about a capability the agent lacks returns the
owning package, what enabling it would add, and the enable path — through the
tool alone, from a cold start; before approval the tool is absent (the M0 grant
gate, asserted); `elanus context render` for the default profile shows the
awareness block; the skill's frontmatter makes the pattern high-availability.

### M3 — enablement rides the existing config-proposal flow
Discovery's "enable this" guidance uses the existing proposal machinery (no new
mechanism): the agent proposes adding the package to its profile;
`reap_proposals` records `obs/config/proposed`; autonomy/human accepts.

**Acceptance:** an agent, told by `find_capability` that a package exists, files
an enablement proposal through the existing flow (an `obs/config/proposed` event
appears attributed to the agent), and a human/autonomy can accept it — after
which the package is in the agent's visible set and (kb-search) its content is
findable.

## Read these first
- The settled design: [knowledge-base.md](knowledge-base.md) D7 (the discovery
  gap; privileged read of the package universe; enablement rides
  config-proposal), build step B7.
- **The seam this tool rides:** [kb-search.md](kb-search.md) M0 + wonky bit 1
  (the `[[tool]]` declaration/grant/dispatch contract, Fable's ruling, the
  collision rule) — a hard dependency.
- The privileged-vs-visible distinction: `src/packages.rs:67` (`discover` —
  whole universe), `:71-99` (`discover_for_profile` — filtered visible set),
  `:150-151` (visibility-not-authority invariant). The catalog precedent this
  generalizes: `src/agentcli.rs:52-70` (`catalog`) + `:274`
  (`packages_for_profile` uses `discover_for_profile` — today's catalog is
  visibility-scoped; discovery is the universe-scoped generalization).
- The enablement flow (verified): `src/exec.rs:258-332` (`config_clone_setup` →
  `reap_proposals` → `obs/config/proposed` → `configcli::classify`),
  `src/config_repo.rs:441-479`.
- Teaching tiers: [../journeys/14-timers-and-scripts.md](../journeys/14-timers-and-scripts.md)
  ("tell the agent").

## Log
- 2026-07-02 — Decomposed from knowledge-base.md B7 by Opus (planner) under
  Fable. Grounded against the sprint-4 worktree: `packages::discover`
  (`src/packages.rs:67`) is the whole-universe read (no visibility filter) vs
  `discover_for_profile` (`:71`), so "privileged discovery" = reading the
  former; `ELANUS_WITH_PACKAGES` widens visibility not authority (`:150-151`).
  Enablement path verified: proposal clone → `reap_proposals` →
  `obs/config/proposed` → classify (`src/exec.rs:258-332`). Original judgment
  call: CLI+skill+block primary (no tool), because no tool-def seam existed.
- 2026-07-02 (later) — **Fable's ruling folded in:** the `[[tool]]` seam now
  lands in kb-search M0, and discovery **supplies its privileged tool through
  it** (`find_capability`) — Tim's own words, "a package that supplies a
  privileged tool." CLI + skill + block demoted to secondary surfaces; the
  kernel CLI remains the single implementation of the universe scan, with the
  tool script a thin wrapper over `elanus discover --json` (wonky bit 2 —
  confirm). Supplying the tool is grant-gated via M0's kind `"tool"`, which is
  where the "privileged" property gets its explicit human approval (wonky
  bit 3).

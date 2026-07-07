---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-27
---

# Handoff: materialize a profile's skills into coding harnesses

From `docs/_questions.md`: *"How does it work when we have a list of 5 kits and 10
skills and we wire it into claude code? codex? …"* — and the follow-ons: *"when we
'render' is that a copy or a link? For coding agents I'm expecting a link. Or even
better, a layered FS so it's completely ephemeral and missing for all other agents.
Is that crazy?"*

**Status: implemented on branch `coding-skill-materialization`** (binding + all three
harnesses, both TUI and headless cells). Verified: `cargo test` 392 pass (3 new
regression tests), clean build, all three skill-injection mechanisms empirically
smoke-tested against the installed CLIs, and an end-to-end `elanus code … --headless`
run confirming a profile skill reaches the agent. See **Verification** below.

## What this closed

Native elanus agents saw the profile's full skill inventory; coding harnesses saw
only the bootstrap `/elanus` dispatch skill. Now a coding session adopts a profile's
**visible** skills (the same `discover_for_profile ∩ skill_visible ∩ has-SKILL.md`
set the native renderer surfaces) and materializes them into each harness's native
skills dir — by **symlink** (Tim's "I'm expecting a link"), into a **per-session run
scratch** that is `remove_dir_all`'d at launch exit (ephemeral + private to the
session, no union FS needed).

## Decisions (locked with Tim)

- **Binding: default profile always, `--profile <name>` overrides.** Every
  `elanus code <tool>` materializes the `"default"` profile's visible skills; the new
  long `--profile` flag selects another. (Codex's own config-profile is still its
  short `-p` — elanus consumes only the long form.)
- **Link, not copy.** Symlink each package dir → `<skills_dir>/<name>` (the
  `<name>/SKILL.md` shape every loader scans). One operation; only the parent dir
  differs per harness.
- **No layered/union FS.** overlayfs is Linux-only; primary platform is darwin. The
  per-session scratch already gives ephemerality + privacy; CoW isolation isn't
  needed for read-mostly skill dirs.
- **Scope: all three harnesses, both cells.**

## The verified per-harness mechanism (the research was half-wrong for these versions)

Smoke-tested with a marker skill ("state handshake code X") against the installed
CLIs — **codex-cli 0.141.0, opencode 1.17.9**:

| Harness | Mechanism (what actually works) | Ephemeral? |
|---|---|---|
| **Claude Code** | **`--plugin-dir <scratch>/plugin`** — a per-session plugin (`.claude-plugin/plugin.json` + `skills/<name>`). `--add-dir` does **NOT** register skills, and `--setting-sources ''` (which elanus uses to isolate from `~/.claude`) **disables `.claude/skills` discovery entirely** — so `--plugin-dir` is the ONLY channel that surfaces skills under that isolation from an ephemeral path. (`extraKnownSkills` settings key also failed.) | ✅ scratch |
| **Codex** | **`CODEX_HOME=<scratch>`** with the user's real `auth.json`/`config.toml`/`version.json` SYMLINKED in (login survives) + skills under `<scratch>/skills/<name>`. `AGENTS_HOME` and `skills.config` both **FAILED** on 0.141.0 (they postdate it); codex only scans `$CODEX_HOME/skills` and project `.codex/skills`, so an isolated `CODEX_HOME` is the only ephemeral lever | ✅ scratch |
| **opencode** | **`OPENCODE_CONFIG_DIR=<scratch>`** with skills under `<scratch>/skills/<name>` — **confirmed it scans `skills/`** (the docs omit skills from that env's list; the smoke test settled it: `→ Skill "secret-handshake"`) | ✅ scratch |

Three corrections vs. the research/earlier draft, all caught by smoke tests:
- **Claude:** `--add-dir` does NOT load skills, and `--setting-sources ''` disables
  `.claude/skills` discovery — so the original `--add-dir <skillroot>` plan was
  wrong, AND the pre-existing `/elanus` bootstrap skill had **never actually loaded
  as a skill** (it survived only because the dispatch knowledge also rides the
  launch brief / system reminder). The fix — a `--plugin-dir` plugin — finally makes
  `/elanus` a real loaded skill too.
- **Codex:** `AGENTS_HOME` plan was wrong for 0.141.0 → corrected to `CODEX_HOME`.
- **opencode:** the `OPENCODE_CONFIG_DIR`-scans-skills question (flagged "UNVERIFIED"
  in research) is now **verified true**.

## Implementation (all in `src/codeagent.rs`)

- `take_profile_flag` — parses `--profile`, default `"default"`; strips it from argv.
- `visible_skill_packages(root, profile)` — the `(name, dir)` set; best-effort (logs
  and yields empty on profile-load/discovery error, never blocks a launch).
- `link_skill_packages(skills_dir, skills)` — symlinks each package; no-op on empty.
- `build_codex_skills_home(root, session, skills)` — builds the per-session
  `CODEX_HOME` (mirrors auth/config by symlink, links skills); None on empty.
- `build_claude_skill_plugin(scratch, skills)` — builds the per-session Claude
  plugin (manifest + bootstrap `/elanus` skill as a real file + symlinked profile
  skills); always Some (the bootstrap skill is always present).
- `launch()` computes `skills` once; `StreamLaunch` carries it.
- Claude: `--plugin-dir <plugin>` in both the worker and TUI launch branches
  (replacing the old `--add-dir <skillroot>`).
- Codex: `CODEX_HOME` set in `run_codex_capture` (exec) and `run_codex_tui_import`.
- opencode: `OPENCODE_CONFIG_DIR` set in `run_opencode_capture` (run) and on the
  served instance in `run_opencode_tui_server_events`.

The credential note (security): codex's `auth.json` is **symlinked, never copied** —
the secret stays in `~/.codex`, read in place by the user's own codex (homogeneous
authority, no boundary crossing, per `tim-safety-audit-not-restriction`). The
scratch (and the symlinks, not the secrets) vanishes at launch exit.

## Verification

- `cargo test` → **392 pass** (389 prior + 3 new), clean build.
- New regression tests: `profile_flag_default_and_override`,
  `link_skill_packages_symlinks_each_and_is_noop_when_empty`,
  `codex_skills_home_mirrors_auth_and_links_skills`.
- Mechanism smoke tests (real CLIs): codex `CODEX_HOME` (auth survived, marker
  returned), opencode `OPENCODE_CONFIG_DIR` (skill invoked, marker returned).
- End-to-end (all three harnesses): `elanus init` root + a marker skill on the
  default path → `render` shows it in the inventory → `elanus code <tool> --headless`
  returns the marker for **claude, codex, AND opencode** (the full
  `launch → discover → link → env → harness-loads-skill` chain, real CLIs).

## Known residuals / follow-ons

- **`--provider` + skills interaction** (codex/opencode): the per-session
  `CODEX_HOME`/`OPENCODE_CONFIG_DIR` is set after the provider injection env; they
  target different layers (skills dir vs. auth/config-content) and should coexist,
  but the combined path wasn't exercised under a real provider — worth a check.
- **Newer codex**: when the pinned codex gains `AGENTS_HOME`/`skills.config`, the
  `CODEX_HOME`-mirror dance can be replaced with a lighter lever (less to symlink).
- **M4 providers** (live `[[provider]]` output into harnesses) — deferred, as drafted.
- Web UI surface for per-session skill selection — not in scope.

## Out of scope
- `kit install` copy/link semantics — untouched.
- Per-skill in-harness permission scoping — homogeneous authority; profile
  visibility is the gate.
- A union/overlay FS — explicitly rejected.

## Log
- 2026-07-07 — Confirmed shipped+merged on main; the core helpers
  (`take_profile_flag`, `visible_skill_packages`, `link_skill_packages`,
  `build_codex_skills_home`, `build_claude_skill_plugin`) are present in
  `src/codeagent.rs` and survived the PH4 pluggable-coding-harness refactor
  (`3720df3` deleted the `trait Harness`/`HARNESSES` registry and moved dispatch
  to packages; this handoff's materialization logic was untouched by that
  migration). Status flipped to `done` (was stale at `implemented`). The two
  residuals below are still open and worth tracking (previously recorded only
  under "Known residuals" here, not elsewhere): the `--provider` + skills
  interaction is unverified in combination, and the CODEX_HOME-mirror comment
  in `src/codeagent.rs` still cites codex-cli 0.141.0 while the repo's pinned
  version is now 0.142.5 (confirmed via `docs/appserver-spike/README.md`), which
  may support a lighter `AGENTS_HOME`/`skills.config` lever — worth a review.

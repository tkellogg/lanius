---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-01
---

# Handoff: the macOS cage learns reads and network (first enforcement increment)

[../sandbox.md](../sandbox.md) decided (2026-06-19, "Promoted from deferred to
target") that the end state is a **single complete cage**: write + read + network
restriction enforced by elanus, so that the coding tools' own sandboxes
eventually become redundant. Today the cage is write-only: `sbpl()`
(`src/sandbox.rs:180`) emits `(allow default)` + `(deny file-write*)` with write
allow roots and a few protected denies. There is **no read scoping and no network
restriction anywhere in the tree** (verified: no other `deny file-read*` or
`network` rule exists outside the secrets fence).

Seatbelt already supports both missing halves — `(deny file-read*)` with allow
lists, and `(deny network*)` with local/remote filters — so a macOS enforcement
increment is buildable now, without the hard read-*camera* work. This handoff
adds the **mechanism, config surface, live tests, and honest status** for read
and network enforcement, **default-off**. It changes nothing for anyone who does
not opt in.

## The one rule above everything

**A default install must behave bit-for-bit as it does today.** Over-tight read
denial would break every spawned process on the machine — interpreters, dynamic
libraries, shells — which is catastrophic and hard to diagnose from inside (the
process just sees EPERM everywhere). So every new restriction in this increment
is opt-in, and one milestone's acceptance is literally "the default profile
produces the same Seatbelt profile string as before this change."

## Wonky bits / decisions up front

1. **Rollout gate: per-profile opt-in, default off, no flag day.** New `[sandbox]`
   profile keys (`network`, `fs_read_deny`, `fs_read_allow` — M3) that are absent
   by default. Absent = today's write-only cage, byte-identical. The dispatcher's
   package-actor cages (`src/dispatcher.rs:438`) and the MCP server cage
   (`src/mcp.rs:213`) keep today's behavior in this increment — the policy
   parameter exists on `Cage` so they *can* be wired later, but only the agent
   shell path (`Cage::from_profile`, `src/exec.rs:446`) reads the new keys now.
   Flipping any default (for packages, or system-wide) is a later, deliberate
   step. *Fable: confirm per-profile is the right gate — the alternative is a
   root-wide `bus.toml` switch, but a bad read policy should only ever break the
   one agent whose profile asked for it.*

2. **Reads ship deny-list-first; the allow-list mode is offered but marked
   experimental.** sandbox.md is explicit: the read envelope is a different shape
   from the write envelope — "a broad sensible baseline with deny-listed
   sensitive regions, not the tight allowlist the write side uses." So:
   - `fs_read_deny` (the supported mode): baseline reads stay open; the listed
     trees become unreadable, on top of the secrets fence that already exists
     (`Protect::deny_all_trees`, `src/sandbox.rs:53`). Low breakage risk;
     obvious value (e.g. deny `~/.ssh`, `~/.aws`, another agent's state dir).
   - `fs_read_allow` (experimental): flips to `(deny file-read*)` with an allow
     list. Whoever sets it owns the baseline problem (interpreters, `/usr/lib`,
     `/System`, the repo). We ship the mechanism and a live test proving a caged
     `sh -c 'echo hi'` still runs under a sane allow list — we do NOT ship a
     default baseline in this increment. Getting that baseline right is the real
     M2-of-the-end-state work.
   - **No new default deny entries.** Tempting to deny `~/.ssh` for everyone,
     but a caged `git fetch` over ssh legitimately reads it. Config-driven only.

3. **Network policy has three values: `open` (default), `loopback`, `none`.**
   `loopback` exists because caged actors must reach the broker and the local
   HTTP read planes — a plain `(deny network*)` would cut every actor off from
   the bus. `none` is for pure-compute spawns. Note `loopback` is also the shape
   that would eventually make the web-server confused deputy unreachable from
   the cage (security entry 13's "OS-level complement") — but that needs a
   port-level carve-out we do not build yet; record the connection, don't chase
   it here.

4. **Exact SBPL syntax is a spike inside M1, not an assumption.** `sandbox-exec`
   is deprecated-but-functional and its network filter grammar
   (`(deny network*)`, `(allow network* (local ip "localhost:*") (remote ip
   "localhost:*"))`) is under-documented. Prior art to crib from: Claude Code's
   own sandbox and Anthropic's sandbox-runtime (both named in sandbox.md,
   "Platform notes"). The live test, not the string test, is the arbiter.

5. **Read scoping rides the grant model; leases stay write-only.** The profile's
   `[sandbox]` block *is* the whole-agent grant surface today (`SandboxCfg`,
   `src/profile.rs:162`), so the new keys live there — same vocabulary, as
   sandbox.md requires. Leases (`fs_lease`) remain exclusive *write* authority;
   when `narrowed_cage` (`src/exec.rs:1035`) rebuilds the cage from held leases,
   it must carry the read and network policy through unchanged. Read leases are
   not a thing in this increment.

6. **Violations must stay legible.** A read/network denial surfaces as an EPERM
   tool error the agent sees (sandbox.md, "Platform notes"). No new event plane;
   the honest status surface (M4) is where a human learns what posture a spawn
   ran under.

## Explicitly out of scope (say why, then stop)

- **Linux.** No enforcement mechanism is wired off macOS today (`can_enforce`,
  `src/sandbox.rs:122`); Landlock + bubblewrap land with the VPS move. This
  increment must not pretend otherwise — the M4 status reports "unavailable
  here" off macOS, exactly like the read camera does.
- **The read-event camera (read-provenance M2).** Observation of *allowed* reads
  needs syscall interception (seccomp-unotify / Endpoint Security) — a genuinely
  different mechanism. This handoff is enforcement only; the two-tier
  `ReadCameraStatus` honesty (`src/sandbox.rs:341-404`) is unchanged.
- **Bypassing the coding tools' own sandboxes.** Claude Code / Codex / opencode
  deliberately run OUTSIDE elanus's cage with their own sandboxes
  (`src/codeagent.rs:2305-2307`). Reconstructing their posture onto elanus's
  cage is the flip sandbox.md's end state describes — it happens only after this
  increment has soaked, as its own deliberate step. Do not touch the coding
  spawn paths.
- **Exec-mode package handlers.** They spawn uncaged today (security entry 16);
  caging them at all is that entry's work, not a read/network add-on.

## Milestones

### M1 — `sbpl()` learns read and network arms (mechanism + string tests)
Extend `Cage` with the policy: a `NetworkPolicy` (`open`/`loopback`/`none`) and
read config (deny trees; optional allow roots). Extend `sbpl()`
(`src/sandbox.rs:180`) to emit the corresponding arms, keeping last-match-wins
ordering (the existing fence discipline, `src/sandbox.rs:191`): network denies
and read denies after the allows; in allow-list read mode, `(deny file-read*)`
with allow subpaths plus the always-needed holes (mirror the write side's
`/private/tmp`, `/private/var/folders`, `/dev`). The Protect fence (secrets
unreadable) must survive every combination. Includes the syntax spike (wonky
bit 4).

**Acceptance:** unit tests in the style of `sbpl_contains_roots_and_denies_writes`
(`src/sandbox.rs:559`): each policy value produces the expected rules in the
expected order; `open` + no read config produces a string **identical** to
today's; secrets remain `deny file-read*` in all modes. `cargo test` green.

### M2 — Live proof it cages (and does not over-cage)
Extend the `seatbelt_actually_cages` live test (`src/sandbox.rs:590`, macOS +
`sandbox-exec` gated) with the new arms:
- `network = none`: a caged `curl`/`nc` to a listener the test binds on
  127.0.0.1 **fails**.
- `network = loopback`: the same local request **succeeds** (and the test does
  not depend on external network access — assert only loopback behavior).
- `fs_read_deny` on a scratch tree: a caged `cat` of a file inside it fails; a
  read outside it succeeds.
- allow-list mode with a sane test baseline: a caged `sh -c 'echo hi'` **still
  runs** and can read its allow roots — the anti-catastrophe assertion.

**Acceptance:** the live test passes on a macOS dev machine; all cases above
asserted; the test is skipped (not failed) where `sandbox-exec` is absent, same
as today.

### M3 — The profile surface, wired through leases
Add `network`, `fs_read_deny`, `fs_read_allow` to `SandboxCfg`
(`src/profile.rs:162`), all optional. `Cage::from_profile` (`src/sandbox.rs:78`)
reads them; `narrowed_cage` (`src/exec.rs:1035`) carries them through when
leases narrow the write set; `workdir` semantics unchanged. Dispatcher and MCP
spawn sites pass the do-nothing default. Document the keys where `[sandbox]` is
documented, in plain words ("what this agent may read / whether it may use the
network"), and add a paragraph to [../sandbox.md](../sandbox.md) recording this
increment.

**Acceptance:** a profile with no new keys produces a byte-identical SBPL string
to the pre-change build (a regression test asserts this); a profile with
`network = "none"` yields a shell tool call that cannot reach a loopback
listener even while holding a lease (proving `narrowed_cage` carries policy);
`cargo test` green.

### M4 — Honest status: what posture is this cage actually in?
Mirror the `ReadCameraStatus`/`TierStatus` pattern (`src/sandbox.rs:341-404`):
a `CageStatus` reporting, per dimension — write, read, network — whether
enforcement is available on this platform (macOS + `sandbox-exec` present, the
existing `can_enforce` probe) and what the active policy is. Surface it where
the read-camera status already lands (the system status / trust surface the web
UI reads). Product words: "writes fenced", "reads open / some folders hidden /
allow-list", "network open / this machine only / off" — never "SBPL",
"Seatbelt", or "cage" in the interface.

**Acceptance:** a unit test in the style of `read_camera_status_two_tiers`: each
config produces the right status, and off-macOS every enforcement dimension
reports unavailable (never a silent "on"). The status endpoint includes it and
the trust card renders it; a `ui.spec.mjs` assertion covers one state.

## Read these first
- The design and its history: [../sandbox.md](../sandbox.md) — all of it,
  especially "The whole-agent grant", the 2026-06-19 single-cage promotion
  (:98-110), and "Platform notes".
- The mechanism today: `src/sandbox.rs` — `Cage` :23, `Protect` :50,
  `from_profile` :78, `from_roots` :106, `sbpl()` :180, the status pattern
  :341-404, the string test :559, the live test :590.
- The spawn sites: `src/exec.rs` :446 (grant cage), :1035 (`narrowed_cage`),
  :2043 (`run_shell`); `src/dispatcher.rs` :438-474 (package actors);
  `src/mcp.rs` :213 (MCP servers); `src/codeagent.rs` :2305-2307 (why coding
  agents are NOT here).
- The config surface: `src/profile.rs` :162 (`SandboxCfg`).
- Why the deputy matters to `loopback`: [../security.md](../security.md) entry
  13 (the OS-level complement paragraph).

## Log
- 2026-07-01 — Created from the 2026-07-01 vision-drift recon. Verified in the
  worktree: `sbpl()` is write-only plus the Protect fence; no read scoping or
  network rule exists anywhere; the enforcement-availability probe and the
  two-tier status pattern are the exemplars to mirror. Judgment calls for Fable:
  per-profile opt-in as the rollout gate (vs a root-wide switch, wonky bit 1);
  deny-list-first reads with the allow-list mode shipped but explicitly
  experimental and baseline-less (wonky bit 2); three-value network policy with
  `loopback` as the bus-preserving middle (wonky bit 3); package/MCP cages left
  on today's posture this increment.
- 2026-07-01 — All milestones implemented and adversarially verified (Opus
  impl/verify under Fable orchestration); landed on sprint-recon-2026-07.
  Status flipped to done.

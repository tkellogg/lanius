---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-01
---

# Handoff: two small guards + a docs truth sweep

Three cheap, independent milestones. M1 and M2 are one-function code fixes with
a test each; M3 is a documentation sweep with no code. None of them block or
depend on the other handoffs from this sprint.

## Wonky bits / decisions up front

1. **The `emit_event` guard's home is the tool arm in `src/exec.rs`, not deep in
   `events::emit`.** Security entry 15's update names the threat: the agent's
   `emit_event` tool calls `events::emit` directly (a ledger write, never checked
   against the broker's publish ACL), so an agent can mint an event whose type is
   `in/dm/...` addressed to *another* agent and poison that agent's recall.
   Guarding inside `events::emit` itself is tempting but wrong-shaped: the kernel
   and ingress bridges legitimately emit `in/...` events through the same
   function (the broker path with a verified sender, `send_message` via
   `emit_message`), and `emit` cannot reliably tell an agent apart from the
   kernel (`ELANUS_ACTOR` is self-reported, `src/events.rs:68-72`). The
   `emit_event` tool arm (`src/exec.rs:1866`) is exactly the agent-reachable
   surface and only that surface — refuse the whole reserved ingress plane
   (`in/...`) there, with an error that points the agent at `send_message` /
   `ask_human` for owner mail. The deeper reservation (only bridges may publish
   `in/dm/...` on the bus; narrowing broad publish grants) stays with the
   actor-authorization work as entry 15 already records — this milestone closes
   the tool path, not the whole class. *Fable: confirm refusing all of `in/*`
   (not just `in/dm/*`) — I chose the whole plane because `in/agent/<other>` and
   `in/human/<owner>` forgeries are the same trick, and agents have real verbs
   for every legitimate case.*

2. **`kit unlink` reuses the exact `revoke` guard.** `elanus revoke` refuses a
   protected stdlib package without `--force` (`src/main.rs:1222-1231`, via
   `kit::protected_packages`, `src/kit.rs:324`). `elanus kit unlink`
   (`src/kit.rs:348`, dispatched at `src/main.rs:1050-1053`) has no such check —
   and unlinking a protected kit silently removes its packages from the path,
   which is the same off-switch the web UI exposes. Guard at the kit level: a kit
   whose `kit.toml` sets `protected` (`kit_is_protected`, `src/kit.rs:311`)
   refuses to unlink without `--force`, with the same loud wording revoke uses.

3. **Docs edits here may collide with a sibling session.** `docs/handoffs/
   README.md` and `docs/README.md` are also being edited by an in-flight session
   in the main checkout. Editing them on this branch is fine, but the eventual
   merge may need manual reconciliation — flagged so the implementer expects it.

## Milestones

### M1 — `kit unlink` honors the protected gate
Add a `--force` flag to `KitCmd::Unlink` and a protected check before
`kit::unlink` runs: if the kit resolves to a protected kit (`kit_is_protected`,
`src/kit.rs:311`), refuse without `--force`, same message shape as
`Cmd::Revoke` (`src/main.rs:1225-1231`).

**Acceptance:** a unit test in `src/kit.rs` (model it on
`protected_packages_tracks_kit_toml`, `src/kit.rs:537`): unlinking a kit whose
`kit.toml` has `protected = true` fails with the loud refusal; with `--force` it
succeeds; unlinking an ordinary kit is unchanged. `cargo test` green.

### M2 — `emit_event` refuses the reserved ingress plane
In the `emit_event` tool arm (`src/exec.rs:1866`), refuse any `type` beginning
`in/` before calling `events::emit`. Error text tells the agent what to use
instead ("the in/ plane is reserved for ingress; use send_message or ask_human to
reach your owner"). Everything else (`obs/...`, custom event types) is unchanged;
`send_message`/`ask_human` keep emitting `in/human/<owner>` through their own
arm. Update [../security.md](../security.md) entry 15 (`docs/security.md:335`)
to record the tool path as closed and the bus-grant half as the remaining
residual.

**Acceptance:** a unit test drives the `emit_event` tool with
`type = "in/dm/bluesky/somebody"` and asserts a refusal (no ledger row), and with
`type = "obs/custom/thing"` and asserts success; a `send_message` in the same
suite still lands on `in/human/<owner>`. `cargo test` green.

### M3 — Docs truth sweep (no code)
Every item verified against this tree on 2026-07-01; each is a small edit:

- `docs/handoffs/sibling-intent.md:2` and
  `docs/handoffs/sibling-resolution-skills.md:2` — `status: draft` but the work
  shipped; set `status: done` (confirm each Log's last entry says shipped before
  flipping).
- `docs/handoffs/model-providers.md:2` — `status: verifying` but the branch was
  merged; set `status: done`.
- `src/provider.rs` — the comment block at :162-166 and the
  `#[allow(dead_code)]` attributes at :169/:181/:193/:204/:247 say `materialize`
  and `Consumer` are "deliberately unwired" — false since the M2/M3 consumers
  landed (`src/exec.rs:1160-1163`, `src/codeagent.rs:3551-3554` call them). Drop
  the stale allows (the compiler confirms which are truly dead) and rewrite the
  comment. This is a comment/attribute-only code touch inside the docs milestone;
  `cargo build` must stay warning-free.
- `docs/ui-flows/README.md:36` — points at `ui/web/server.mjs`, which no longer
  exists; point at `src/web.rs`.
- `docs/bus.md:487` — the identity/auth question is marked `[OPEN]` ("should
  unauthenticated become deny rather than allow?") but the broker handshake now
  enforces deny-by-default (`src/broker.rs:424`: wrong credentials AND no
  credentials are both refused). Mark it decided/built with a pointer to
  `src/broker.rs`.
- `docs/_bugs.md:1` — the SSE-reconnect bug ("UI doesn't reconnect to MQTT …
  /api/stream was interrupted") was fixed in commit `b40dc7a`; mark it fixed with
  the commit hash.
- `docs/README.md` "Implementation Anchors" (:75) — add pointers for surfaces it
  omits: `src/agentcli.rs` (`elanus agent`), `src/profilecli.rs`
  (`elanus profile`), `kits/funnel`, `kits/memory-blocks-demo`,
  `packages/triage-demo`. Match the existing bullet format.

**Acceptance:** each listed line is changed; `grep -rn 'server.mjs'
docs/ui-flows/` returns nothing; `cargo build` clean after the provider.rs
attribute removal; a reviewer can confirm every frontmatter flip against that
handoff's own Log. Note in the commit message that `docs/README.md` may need
merge reconciliation against the sibling session's branch.

## Read these first
- The threat M2 closes: [../security.md](../security.md) entry 15
  (`docs/security.md:306`, the residual + its 2026-06-14 update at :335).
- The guard M1 copies: `src/main.rs:1222-1231` (`Cmd::Revoke`),
  `src/kit.rs:307-343` (`kit_is_protected`, `protected_packages`), `src/kit.rs:348`
  (`unlink`).
- The tool surface M2 touches: `src/exec.rs:1866` (`emit_event` arm), :1887
  (`send_message` arm — the legitimate path that must keep working),
  `src/events.rs:52` (`emit`, sender fallback :68-72).
- Why protected exists: [../config.md](../config.md) ("Stdlib: the configuration
  that is always there").

## Log
- 2026-07-01 — Created from the 2026-07-01 vision-drift recon. All anchors
  re-verified in the worktree after the pluggable-harness merge (revoke gate
  moved to `main.rs:1222`; unlink dispatch to `main.rs:1050`; provider.rs
  consumers now live at `exec.rs:1160` / `codeagent.rs:3551`). Judgment calls:
  the emit_event guard refuses ALL of `in/*` at the tool arm rather than only
  `in/dm/*` inside `events::emit` (decision 1); the kit-unlink guard keys on the
  kit's own `protected` flag rather than per-package membership (decision 2).
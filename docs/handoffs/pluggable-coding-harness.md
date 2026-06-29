---
status: draft
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-29
---

# Handoff: pluggable coding harnesses — make a harness a package, not a PR

**The goal, stated as the promise it makes to an adapter author:** you can add a new
coding tool (gemini-cli, aider, cursor-cli, …) to elanus by writing a small adapter
and shipping it as a **package** — no fork of elanus, no `impl Harness` in
`src/codeagent.rs`, no PR, no merge wait. Your adapter is the tool-specific 20%
(launch the tool, read its event stream); an `elanus-harness` **library** is the
shared 80% (be a well-behaved elanus citizen). `elanus code <yourtool>` discovers it
and just works.

The why is [../journeys/13-adding-a-harness-without-forking.md](../journeys/13-adding-a-harness-without-forking.md):
the harness adapter is the one elanus capability that isn't a package — the most
requested extension (the "remaining dozen") welded into the binary. This makes it the
same as every other capability: a package with a manifest and a scoped grant.

## The three pieces

### 1. The adapter SDK — the orchestration "verbs" as a LIBRARY (the keystone)
The parts elanus keeps for itself today — session identity, the bus, claims, comms,
last-active — become a reusable crate (`elanus-harness`) an adapter builds ON, not a
CLI it shells across. elanus mints the session + identity + scoped bus token and
hands them to the adapter; the SDK wraps "do the right elanus thing" so the adapter
author never hand-rolls the obs grammar or the ledger writes.

An adapter receives a context and implements launch + translate. Sketch:

```rust
// the whole adapter, roughly
fn main() -> elanus_harness::Result<()> {
    let ctx = elanus_harness::Ctx::from_env()?; // session id, bus token, root, workdir,
                                                // mode, prompt, briefing, skills dir
    let mut child = launch_my_tool(&ctx)?;      // tool-specific: spawn gemini-cli
    for ev in my_tool_events(&mut child) {      // tool-specific: parse its stream/hooks/SSE
        ctx.emit(ev.leaf, ev.body);             // → obs/agent/<noun>/<session>/<leaf>
        if let Some(p) = ev.edited_path { ctx.claim(&p); }   // advisory edit-claim
        if let Some(id) = ev.native_session { ctx.record(id); } // durable resume record
    }
    ctx.finish(child.wait()?)                   // completion / failure routing
}
```

`Ctx` methods (the verbs, as library calls — each is an existing elanus primitive):
- `emit(leaf, body)` — publish `obs/agent/<noun>/<session>/<leaf>` in the documented
  grammar, stamped with the session identity (wraps `publish_obs`).
- `claim(path)` / `unclaim(path)` — advisory edit-claim in the session's room (wraps
  `auto_claim_write`/`add_claim`); this is what makes the new tool sibling-aware.
- `record(native_session_id)` — persist the durable record for resume.
- `bump_active()` — keep `last_active` fresh for sibling-intent.
- `inbox()` / `deliver(to, msg)` — the comms rails, if the adapter wants dispatch.
- helpers: `scrub_provider_creds(cmd)`, `materialize_skills(dir)`, briefing accessor.

**Refactor, don't reinvent:** these already exist in `src/codeagent.rs`
(`publish_obs`, `auto_claim_write`, `inbox_for_session`, `deliver`, the launch-env
contract). PH1 is extracting them into the crate behind a stable surface — and the
proof it's right is that the three built-ins can be rebuilt on it.

**Decoupling note:** v1 the SDK may write the ledger directly (the adapter is a local
trusted process with `$ELANUS_ROOT`). A fully bus-only variant — claims/records as bus
messages a core subscriber persists — is the harder, more decoupled future (works
remotely, no shared db file); name it, don't block on it.

### 2. Dispatch — ONE way: a package that declares `[[harness]]`
`elanus code <x>` resolves the package whose `elanus.toml` declares `[[harness]]` with
`name = "<x>"`. That's the only mechanism — same discovery + grant + scoped-token +
sandbox path as every other package (capabilities-as-grants, the elanus doctrine; one
way to do things). The manifest names the verb (`name`), aliases, the agent noun, the
adapter binary (`run`, relative to the package dir), and the bus grants the adapter
needs (`[request]`). No PATH fallback, no second mechanism — a harness is a package,
full stop.

### 3. The launch + obs contract — what core hands over, what the adapter must emit
- **Core → adapter (env/args):** `ELANUS_ROOT`, `ELANUS_CODE_SESSION`, `ELANUS_AGENT`
  (noun), `ELANUS_BUS_TOKEN`, workdir, mode (`tui`/`headless`), prompt, briefing,
  skills dir. (Core has already minted the session, scrubbed provider creds, and set
  identity — the adapter receives, never mints.)
- **Adapter → bus (the obligations):** a `session/start`, the tool's tool-calls as
  `tool/<name>/{call,result}`, assistant messages, a completion (and `{failed:true}`
  on failure, the failure-mail contract), and edit-claims for writes. This IS the
  existing grammar ([../topics.md](../topics.md)) — so conformance is "speak what the
  recorder/projection already read."

## Milestones
- **PH1 — extract the `elanus-harness` crate.** Carve the orchestration primitives out
  of `src/codeagent.rs` into a library with a stable `Ctx`. Make `elanus` a `lib`+`bin`
  (or a workspace crate). Acceptance: it compiles and exposes `emit/claim/record/
  inbox/deliver/bump_active` + the env contract.
- **PH2 — rebuild ONE built-in on the SDK (dogfood the seam).** Re-implement
  `codex` (or `opencode`) as an adapter that uses only `Ctx`, in-process at first.
  Acceptance: that harness behaves identically through the SDK — the proof an outsider
  could have written it. (This mirrors how opencode validated the `Harness` trait.)
- **PH3 — `[[harness]]` package dispatch + a reference external adapter.** Add the
  `[[harness]]` manifest + the `elanus code <x>` → package resolution (the one
  mechanism); ship a tiny reference adapter package (even an "echo" harness, or
  codex-as-an-external-package). Acceptance: `elanus code <ref>` runs a harness that
  lives entirely outside the elanus binary, captured to the bus.
- **PH4 — migrate the built-ins + docs.** Reimplement claude/codex/opencode as stock
  harness packages seeded by `init` (like the stock skills) and DELETE the `Harness`
  trait + `HARNESSES` registry — one way, no dual path. The onboarding guide already
  leads with the package recipe; finalize it once the SDK is real.

## Identity & trust
A package-declared adapter gets a **scoped session bus token** and ledger grants like
any package — homogeneous authority (the user's own tools cooperating, no trust
boundary between them; [security.md] doctrine). Because a harness is just a package,
it inherits the same approve/grant/sandbox story as every other capability — that's a
reason to have only the one mechanism (a raw PATH binary would have no scoping). The
adapter
launches the underlying tool with the tool's OWN auth (scrub elanus's provider creds),
exactly as the built-ins do.

## Costs / wrinkles (name them)
- **A stable API surface** (the `Ctx` + the obs contract) elanus must version — but
  it's ~90% the already-stable obs grammar; the genuinely new part is `Ctx`'s method
  signatures.
- **The built-ins migrate to packages** — the end state is ONE way: every harness is a
  package, the `Harness` trait + `HARNESSES` registry are deleted (claude/codex/opencode
  become stock harness packages seeded by `init`, like the stock skills). A transitional
  window where the trait built-ins and the first package adapters coexist is fine, but
  it's a migration, not a permanent dual path — don't enshrine two ways.
- **Capture for external adapters is self-contained** — an external adapter does its
  own hooks/stream/SSE/rollout capture and emits obs, rather than plugging into the
  trait's `run_*_capture`. The SDK should offer capture helpers (a JSONL-stream reader,
  a hook-bridge config generator) so each adapter doesn't re-solve them.

## Anchors
- `src/codeagent.rs` — `trait Harness` (~415) + `HARNESSES` (~854) (the seam to open);
  `publish_obs`, `auto_claim_write`, `inbox_for_session`, `deliver`, the codex/claude
  launch-env contract, `build_codex_skills_home`/`build_claude_skill_plugin` (skills
  materialization to lift into the SDK), the hook bridge (`hook()` + the generated
  configs) as a capture helper to generalize.
- `src/packages.rs` / `src/manifest.rs` — the package + `elanus.toml` model to extend
  with `[[harness]]`.
- [../coding-harness-onboarding.md](../coding-harness-onboarding.md) — the current
  (in-tree) onboarding guide PH4 rewrites to lead with the SDK path.
- [../topics.md](../topics.md) — the obs grammar that IS most of the contract.

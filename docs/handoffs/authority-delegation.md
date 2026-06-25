---
status: in-progress
author: Opus 4.8 in Claude Code
---
# Handoff: delegated authority is a subset of the spawner

Make authority **narrow monotonically down a spawn chain**: a thing the kernel
launches gets authority that is a strict subset (≤) of whoever launched it,
reconstructed and re-authenticated by the harness at spawn — never blindly
inherited, never silently equal-or-greater. `child.grants ⊆ parent.grants`, by
construction and enforced at mint, so no descendant can out-authorize an
ancestor.

This is the **delegation** half of the identity model. docs/identity.md settles
*who you are* (the broker stamps a verified sender; the launcher vouches for what
it starts). It does not settle *what you may do relative to who started you*.
This handoff is that rule.

Tim's framing (2026-06-20): "the scope & permissions should be a strict subset
(equal or less) than the parent. Not blindly passed down — each subagent should
be reconstructed by the harness to be authenticated & given appropriate
authority." And the budget case: "with [an RLM-style] call, that's a perfect case
where you might cut authority in half or a quarter — a legit way of passing around
the context."

## Why this is the missing through-line, not a new feature

The security ledger keeps recording the *same shape of bug*: an actor ends up with
**more authority than its position warrants.**

- **Entry 20** — a coding session's credential was a full-authority principal; no
  ACL gate ran. Fixed by giving `code-<session>` a **structural** scope (publish
  only its own `obs/agent/<agent>/<session>/#`, subscribe nothing). But that scope
  is *fixed* — "a session has no manifest" — so it is **independent of who spawned
  it.** A worker spawned by a tightly-scoped agent gets the same bus scope as one
  spawned by the owner. There is no `child ⊆ parent` relation; there is only a flat
  per-kind scope.
- **Entry 16** — exec handlers publish as the *owner* (wrong identity, over-attributed).
- **Entries 13 / 21** — confused deputies: a low-authority caller borrows a
  higher-authority surface's standing (the web server; the daemon's `reply_to`).

And the fix already exists for **one dimension**: docs/sandbox.md ships
`lease ⊆ grant` — a canonicalized prefix check, deliberately "a decidable, boring
function," with the spawn cage set to the lease's write set. **That is exactly the
discipline this handoff generalizes** — to every authority dimension, and across
the spawn boundary (`child ⊆ parent`), not just lease-within-a-single-grant.

So this is not a new permission model. It is: take the `⊆` rule the sandbox already
trusts for filesystem writes, make it the **contract of every spawn**, and make the
minted scope a function of the *spawner's* grants instead of a flat per-kind constant.

## What already holds today (build on it, don't re-pave it)

Be accurate about the starting point — half of the mechanism is already correct:

- **Children are re-minted, not inherited.** `scrub_spawn_wrapper_identity_env`
  (src/codeagent.rs) strips `ELANUS_PACKAGE` / `ELANUS_BUS_TOKEN` /
  `ELANUS_CODE_SESSION` from a spawned worker's environment, and the launch wrapper
  mints a *fresh* scoped token (`codesession::mint`). So a spawned agent is
  reconstructed and re-authenticated — Tim's "not blindly passed down" is already
  true for the worker-spawn path.
- **Tool invocations correctly inherit.** A session that shells `elanus bus pub` is
  the *same* agent using a tool — it authenticates as itself (the trivial subset,
  equal). buscli reads `ELANUS_PACKAGE`/`ELANUS_BUS_TOKEN` from the env precisely so
  a tool acts as the session that ran it. That is right and stays.
- **Spawn depth is already tracked** (`ENV_SPAWN_DEPTH`, src/codeagent.rs) — a
  counter rides down the chain but carries no authority consequence yet.

The three gaps:

1. The minted child scope is a **flat per-kind constant** (entry 20's structural
   scope), **not** `subset(spawner.grants)`. You cannot express "give this child a
   narrower slice than I hold."
2. There is **no enforced `child ⊆ parent` invariant** at mint — nothing refuses a
   spawn that would widen authority.
3. Authority is modeled **per dimension, scattered** (bus ACL in the broker, fs
   write as `lease ⊆ grant`, cost as a profile `max_turns`), with **no single
   thing** that gets subset-ed at a spawn — and **no budget (fungible) dimension at
   all**, which is exactly the RLM case.

## The model

**One `Grants` value per principal**, capturing every authority dimension the
harness controls, with **two kinds** of dimension — the distinction is the crux,
and the RLM example is specifically the second:

- **Capability dimensions (non-fungible).** Bus subscribe/publish topic patterns,
  fs write roots (already `lease ⊆ grant`), fs read scope, tool/command allowlist,
  the `blocking` grant. Rule: `child ⊆ parent` — the child gets a *subset*; siblings
  may overlap. This is `lease ⊆ grant` generalized.
- **Budget dimensions (fungible).** Turn/cost budget, wall-clock, spawn fan-out and
  depth. Rule: `Σ children ≤ parent` — the child gets an *allocation carved out of
  the parent's remaining*, and siblings *partition* it. **"Cut authority in half to
  pass context down" is this** — a divisible budget split across a spawn, not a
  subset. (`ENV_SPAWN_DEPTH` is the seed of the fan-out budget already.)

**Spawn = reconstruct → narrow → attest.** Extend what `launch()` already does:

1. Read the **spawner's effective Grants** from its principal record (the fenced
   token / ledger), **never from inherited env**.
2. `child = narrow(parent, request)` — `request` is what the caller asked for.
   Default: inherit-equal. Explicit-narrower allowed (the deliberate halving).
3. **Assert the invariant** — `child ⊆ parent` for capabilities, `Σ ≤ parent` for
   budgets — *at mint*, and **refuse the spawn** if it would widen. This is the real
   check, not a convention. It is the same "decidable, boring function" sandbox.md
   demands of `lease ⊆ grant`.
4. Mint the child token carrying `child`; inject only that. (Env-scrub already does
   the "never pass the parent's token through" half.)

**Enforcement stays where it already lives.** The broker gates the bus ACL by
principal; extend the principal record to carry the full `Grants` and gate each
capability dimension there. The sandbox already enforces fs writes (`lease ⊆ grant`).
The invariant lives at *mint*; runtime enforcement lives at the broker + sandbox.
Nothing new gets a second enforcement path.

**Why this is the right shape for "safety = audit" (Tim's frame).** The contract is
**monotone**: authority only narrows down a chain, so no descendant can exceed any
ancestor — by construction, not by a gate someone could forget. A compromised deep
agent cannot escalate; the worst it can do is what some ancestor already could. And
every spawn *records* "P granted C the subset S" on the bus — the delegation
**is** the audit trail. That is strictly stronger than "homogeneous," and it is
Tim's `≤ parent` exactly. (See the reconciliation note in docs/identity.md: the
coding-agents handoffs' "homogeneous authority among the user's own agents" is the
*equal case* of this `⊆` rule — a fine default for sibling sessions the user spawns
directly — not a competing model.)

## Milestones

### M1 — the invariant, on the budget dimension (the RLM case, easiest to assert)
Give `codesession::mint` (and the `codeagent::launch` path) an explicit budget
carved from the spawner's remaining, and assert `Σ ≤ parent` before minting. Thread
the spawner's budget in (the `code_delivery`/session record already persists per
session). Budget first because it is fungible, it is Tim's named example, and
`Σ ≤ parent` is a one-line check. A spawn that would over-allocate is refused at
mint with a clear error.
**Acceptance:** a child cannot be spawned with a turn/cost budget exceeding the
spawner's remaining; a regression test asserts it at the mint layer (the level
entry 20's fix taught us to test — *authority*, not shape).

### M2 — `Grants` as one value; capability subset on the bus
Unify the scattered dimensions into a single `Grants` carried on the principal
record. Make the broker gate the bus ACL from it, and make `codesession::mint`
compute the child's bus scope as `subset(spawner.bus_grants, request)` instead of
the flat structural constant — asserting `child ⊆ parent`. Default still equals the
entry-20 structural scope (no behavior change for the common case); the difference
is it is now *derived from and bounded by the spawner*, and narrowable.
**Acceptance:** a worker spawned by a bus-scoped agent cannot subscribe/publish
outside the spawner's scope; the broker denies it; a child cannot widen.

### M3 — the remaining capability dimensions
Fold fs read/write, the tool/command allowlist, and `blocking` into the same `⊆`
contract, reusing `lease ⊆ grant`'s canonicalized-prefix machinery for paths.
**Acceptance:** every authority dimension a spawn confers is `⊆` the spawner's, by
the same decidable check; no dimension is exempt.

### M4 (optional) — make narrowing first-class in the UX
A way for a spawner to *request* a narrower child explicitly — e.g. a `--grants` /
budget argument on `elanus code <tool>`, or a per-spawn policy — so RLM-style
halving is a deliberate, visible move, not an implicit default.
**Acceptance:** the owner can spawn a worker with a deliberately smaller slice and
see it refused if it tries to exceed it.

## Open decisions (Tim's calls; recommend, don't assume)

1. **Default narrowing policy** — inherit-equal (narrow only on explicit request),
   or auto-narrow per depth? Recommend **inherit-equal**: auto-narrow is a footgun
   until the dimensions are real, and it would change behavior for every existing
   spawn. Make narrowing *possible and cheap*, not *mandatory*.
2. **Which dimension anchors the machinery** — recommend the **budget** dimension
   first (M1): fungible, it is the RLM example, `Σ ≤ parent` is trivially decidable,
   and it touches no existing capability scope, so it ships the `Grants`/assert
   skeleton with zero behavior change before any ACL is rewired.
3. **Is a coding session "one of the user's agents" or "a caged actor"?** — the
   reconciliation the homogeneous-authority language begs. Recommend: it is a caged
   actor whose grants happen to *default* to a broad (often owner-equal) slice
   because the user spawned it directly — i.e. homogeneous is the *equal case* of
   `⊆`, and the moment one session spawns another, `⊆` (not equality) is the rule.

## Read these first

- [../identity.md](../identity.md) — *who you are* (launcher-vouched minting, the
  verified sender). This handoff is the *what-may-you-do-relative-to-your-spawner*
  it doesn't cover; a new "Delegation" section there states the doctrine.
- [../sandbox.md](../sandbox.md) — `lease ⊆ grant`, the "subset-checkable, decidable,
  boring function" this generalizes. The prefix-check machinery is reusable.
- [../security.md](../security.md) — entries 13, 16, 20, 21 (the "more authority than
  warranted" class this closes the doctrine on) and the new entry 22 (this gap,
  recorded as a held-line doctrine + latent residual).
- [../../src/codeagent.rs](../../src/codeagent.rs) — `scrub_spawn_wrapper_identity_env`
  (re-mint, the correct half) and `launch()` (where narrowing attaches);
  `ENV_SPAWN_DEPTH` (the fan-out budget seed).
- [../../src/codesession.rs](../../src/codesession.rs) — `mint` (where the assert
  lives) and the structural scope of a `code-*` principal (entry 20).
- [../../src/broker.rs](../../src/broker.rs) — the per-principal ACL gates that would
  read `Grants`.

## Log

- 2026-06-20 — Written after the web-packaging work surfaced the gap concretely: a
  CLI shelled from inside a supervised session presents that session's *scoped*
  principal, not the owner, and the broker refuses it — the cage working as
  designed, but with no `child ⊆ parent` model to reason about it and an error that
  hid which identity was tried (the error is fixed separately: buscli now names the
  identity and separates auth-refusal from a dead daemon). Key finding: the `⊆`
  discipline is not new — sandbox.md's `lease ⊆ grant` is it for one dimension, and
  the worker-spawn path already re-mints rather than inherits; what is missing is
  making the minted scope a function of the *spawner's* grants, enforcing `⊆`/`Σ≤`
  at mint, and adding the fungible **budget** dimension (the RLM case). Recorded as
  security.md entry 22 + a Delegation section in identity.md.

- 2026-06-20 — **M1 implemented** (budget dimension + `Σ children ≤ parent` asserted
  at `codesession::mint`; inherit-equal default; owner path unbounded + zero
  behavior change). Acceptance met: a child cannot be minted with a budget exceeding
  the spawner's remaining, refused at the mint layer, with a regression test. Built
  implement→validate with an adversarial loop (medium-effort impl, xhigh validation),
  which caught **two real over-grant bugs** in the bound itself before commit: (1) a
  concurrent-sibling TOCTOU — the decrement had no cross-process lock, defeatable by
  the `elanus code spawn` fan-out — fixed with `libc::flock` over the whole
  read→check→decrement→write-back; (2) a torn-read **fail-open** — a lock-free peek
  plus non-atomic `write_0600` let an unreadable token be granted as unbounded —
  fixed by atomic temp+`rename` writes and gating the lock-free path on file
  *existence* (fail-closed on an unparseable token under the lock). Both proven
  load-bearing by neutralization. Details in security.md entry 22 [M1 LANDED].
  **M2 next**: unify `Grants` + subset the bus ACL from the spawner; also re-key the
  spawner lookup off `ELANUS_CODE_REPLY_TO` (env) onto a capability reference (TODO
  marked at the lookup) before budget becomes runtime-enforced.

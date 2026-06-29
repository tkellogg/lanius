---
name: Adding a harness without forking elanus
description: A would-be integrator's account of wanting to drive their favorite coding tool (gemini-cli) through elanus and hitting the one wall that isn't a package — the harness adapter requires editing elanus's source and sending a PR. The vision: a harness should be a package too, built on an adapter SDK that hands you the orchestration (session, bus, claims, comms) so your adapter is a hundred lines, not a fork.
---

# Why this journey exists

elanus's whole shape is "capabilities are packages." A skill is a folder with a
`SKILL.md`. A provider is a row in the ledger. A context stage, a hook, a daemon
actor, an egress — each is a package you drop in, grant, and use. Nothing about
extending elanus asks you to recompile it.

Except the one thing a third party most often wants to add: **another coding
harness.** I use gemini-cli. I want `elanus code gemini` — captured to the bus,
briefed, skill-equipped, resumable, sibling-aware — the same first-class seat
`claude`/`codex`/`opencode` get. So I open
[../coding-harness-onboarding.md](../coding-harness-onboarding.md) and read step one:
*edit `src/codeagent.rs`, `impl Harness`, add a line to the registry.* Fork the
binary. Send a PR. Wait for it to merge. That is the opposite of how everything else
in elanus works, and it's the capability with the longest tail of demand — the
"remaining dozen" (aider, cursor-cli, cline, amp, goose, crush, …), each one a PR.

# The wall, concretely

The harness adapter is compiled in. The registry is a `static` array of trait
objects. There is no folder I can drop a `gemini` adapter into, no manifest that says
"here is a new harness," no way to ship it as a package alongside my own skills. The
one extension point that should be the *most* open — because it's the most requested
by people who aren't elanus maintainers — is the *only* one welded shut.

And it stings more because the substrate is already there. elanus's universal
interface is the **bus**: every coding session's life is already
`obs/agent/<noun>/<session>/…` traffic that the recorder and projection consume
without caring who emitted it. Packages are *already* out-of-process programs that
speak the bus with a scoped token. A harness adapter is just another such program —
"launch tool X, translate its events to the bus." There is no technical reason it has
to live inside the binary.

# What I actually want to write

Not a trait impl buried in a 10,000-line file. A small, standalone adapter that reads
like this:

> elanus handed me a session: an id, a bus token, the workdir, the mode, the prompt,
> the briefing, a skills dir. My job is to launch gemini-cli, watch what it does, and
> tell elanus. For each tool event I call `ctx.emit(leaf, body)`; when it edits a file
> I call `ctx.claim(path)`; when I learn its native session id I call `ctx.record(id)`
> so it's resumable. That's the whole adapter.

The key is that the parts elanus keeps for itself — minting the session, the identity
and the scoped token, recording claims, the inbox/deliver comms, last-active — should
not be a wall I shell across. They should be a **library** I build *on*. An
`elanus-harness` SDK that gives me a `ctx` with those primitives, so my adapter is the
tool-specific 20% (launch gemini, parse its event stream) and the SDK is the shared
80% (be a well-behaved elanus citizen). The bus is the contract; the SDK is the
scaffolding that makes honoring it trivial.

Then I ship it the way I ship everything else in elanus: as a **package** that
declares `[[harness]]` in its `elanus.toml`. `elanus code gemini` discovers it, hands
it a session, and it just works — with a scoped token and grants like any package, no
elanus PR, no fork, no merge wait.

# Why this is the right shape

- It matches elanus's grain: capabilities are packages; the bus is the interface;
  authority is a grant. A harness-as-package is the missing member of that set.
- It turns the remaining dozen from a maintainer backlog into a community: anyone can
  publish a `harness-aider` package without touching elanus.
- It keeps the built-ins honest. Reimplementing one of `claude`/`codex`/`opencode` on
  the same SDK an outsider uses is the proof the seam is real — the same way opencode
  was the forcing function that turned the hardcoded `Capture` enum into the `Harness`
  trait.

# The catch worth naming
A library + a dispatch contract is a *stable API surface* elanus must then keep — but
it's mostly the obs grammar elanus already documents
([../topics.md](../topics.md)), so the new surface is small. And there should be just
ONE way: a harness is a package, the same as everything else. The built-ins
(claude/codex/opencode) migrate to stock harness packages too — a transitional window
where the old trait and the first package adapters coexist is fine, but the end state
is one mechanism, not two.

I don't want to fork elanus to add my tool. I want to write a hundred-line adapter on
top of an SDK and drop it in as a package. Make the harness a package, not a PR.

# Work plan

[../handoffs/pluggable-coding-harness.md](../handoffs/pluggable-coding-harness.md) —
the `elanus-harness` adapter SDK (the orchestration verbs as a library), the
package-declared (`[[harness]]`) dispatch — the one mechanism — and the launch/obs
contract; with the onboarding guide rewritten to lead with "build an adapter package,"
not "edit the source."

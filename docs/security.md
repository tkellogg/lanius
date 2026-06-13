# Security ledger

The running aggregation of known security problems (Tim's request,
2026-06-12): record them as they're found, fix them when the security model
lands. This file INDEXES; canonical writeups stay where they are (bus.md
KNOWN GAP is the spine). Entries carry a status:

- **[LEGS]** — part of, or falls to, the three-legs pass (bus.md).
- **[OPEN-TIM]** — blocked on a call that's explicitly Tim's.
- **[LATENT]** — not exploitable yet; becomes real when a planned feature
  lands. The feature's design must cite the entry.
- **[DOCTRINE]** — not a bug; a design rule to hold the line on.

The honest summary as of 2026-06-12: **there is no unforgeable human gesture
anywhere in the system.** Authority is decided by locality, every channel the
human has (CLI, files, loopback) is at least as available to an agent with a
shell tool, and the ledger itself sits inside every cage. Everything below is
a facet of that.

## 0. [DOCTRINE] The boundary: fence the inside, invite the world in

A hard rule, decided 2026-06-13, and the lens for reading everything else in
this file. **elanus guarantees its security fences *within* elanus** — among
the actors that participate through its own mechanisms: the write-cage, the
bus, the grants ledger, identity. It does **not** try to defend the machine
against the outside world. If a person with shell access, or a process that
is not an elanus actor, reaches around elanus and edits a file, reads another
process's memory, or otherwise meddles from outside the system's mechanisms,
that is outside the contract. We do not try to save the world; we try to
invite the world into a safe place.

This boundary is not a cop-out — it is set by the goal it serves. The
direction for elanus is to **host other agents**: to load tools like Claude
Code, Codex, or Cursor *into* elanus so they run as ordinary actors and
automatically inherit the same safeguards every actor has — the write-cage,
the leases that stop two actors clobbering the same files, the grants that
bound what each may touch, the identity that records who did what. The fences
exist so that inviting a powerful, possibly careless, possibly
prompt-injected agent into the system is safe for the other actors and for
the human. They are hospitality infrastructure, not a wall against the
universe.

Two consequences:

- **The in-scope adversary is a participant misbehaving** — most concretely,
  an agent that has been prompt-injected and is now trying, through its
  ordinary tools, to read another actor's secrets, forge a message, grab
  authority it was not granted, or trample another actor's files. The fences
  must hold against that, because that is exactly the case we are inviting in.
- **The out-of-scope adversary is anything operating outside elanus's
  mechanisms** — the operating system turning hostile, a separate process the
  user starts by hand, someone with direct disk or memory access. We make no
  promise against these and will not contort the design chasing them.

A corollary about fence *strength*: a fence is only as strong as the
operating system's sandbox beneath it. On macOS the sandbox can deny reads,
so an actor genuinely cannot read another's secrets. On Linux before Landlock
the sandbox cannot fence reads at all, so isolation between co-resident
actors is best-effort there until Landlock (or an equivalent) is in place.
That is a limitation of the fence's strength on a platform — it concerns one
in-scope actor reading another — and the fix is a stronger OS sandbox, not a
change to the boundary above. It does not weaken the elanus-versus-outside
line; it weakens actor-versus-actor isolation on one OS, which is a known
gap to close, not a hole in the promise.

## 1. [LEGS] The containment boundary (canonical: bus.md KNOWN GAP)

Three interlocking legs that must move together: bus authz default
(unauthenticated must become deny, not "the human"), fs_read scoping (else
the credential is just read off disk), exec-handler containment (uncaged,
holds HARNESS_DB). Closing one alone closes nothing. Network-egress control
is the cleaner cut for leg 1.

## 2. [DESIGN DRAFTED] Identity model — see docs/identity.md

The question that was deferred here — should local clients authenticate
positively so that unauthenticated becomes deny — now has a full design in
docs/identity.md (2026-06-13). The short version: the broker is the one
thing that says who sent a message, and the ledger can change only by going
through the broker; the kernel mints a per-actor secret presented once at
connect; the broker stamps the verified sender and ignores any claimed one;
the human is a deliberately special actor whose surfaces carry delegated
authority, with an out-of-band gesture reserved for the highest-stakes
actions. Crucially, identity is the same work as legs 1–3 below — it is not
real until the ledger is kernel-only-writable (legs 3 and entry 3) and
secrets are unreadable by other actors (leg 2 / entry 6). Identity is the
purpose those legs serve; finishing them is finishing identity. Open
decisions on scope and sequencing are listed at the end of docs/identity.md.

## 3. [LEGS] The ledger is inside every cage

The write-cage exempts the harness root ("the harness must not cage itself
out of its own ledger" — profile.rs SandboxCfg), and the root contains
harness.db. So any caged process — the agent's own shell included — can edit
the grants table with sqlite3, no `elanus approve` needed. The exemption is
load-bearing for the recorder (the kernel must always reach its WAL); the
fix is splitting "the kernel's write path" from "what children may write,"
not narrowing the exemption in place.

## 4. [LEGS] No unforgeable commit gesture

Corollary of 1–3, recorded separately because designs keep reaching for one:
the TTY is not a trusted channel (the agent runs `elanus` via shell more
easily than the human types it); an installer can run `elanus daemon`; a web
UI is a terminal with different chrome. **Stage-then-commit is workflow
structure and good UX, not a security boundary, until identity lands.** Any
design whose safety argument ends "...and then the human approves" must say
which channel carries the approval and why an agent can't drive it.

## 5. [LATENT] Privileged gestures becoming scriptable endpoints

The containment punt is justified by "packages are human-installed." Every
feature that turns install/grant/profile-edit into an endpoint (UI admin,
`kit add` behind HTTP, MCP-exposed admin tools) erodes that justification.
Browser-side extras for the UI server: CSRF from hostile origins, DNS
rebinding, plus any local process can curl loopback (network is uncaged).

**[DECIDED 2026-06-12, Tim]** The UI commits (approve/revoke buttons). The
earlier ship-as-staging-only gate claimed the CLI was a safer channel; per
entries 3–4 it is not — the terminal is MORE available to an agent, and
the ledger itself is cage-writable, so refusing the button bought zero
containment while costing the workflow. What a browser adds that a
terminal doesn't is hostile-ORIGIN traffic, and that differential is
guarded for real: mutating routes require a genuinely-local Host (DNS
rebinding) and a matching Origin when one is sent (CSRF); UI decisions
carry decided_by=ui in the ledger for the trail. The endpoint inherits
entry 3's blast radius, no more — and when the identity model lands, it
authenticates like every other privileged client.

## 6. [LEGS] Secrets are readable

.env (API keys, 600 but same-uid), future cookies/tokens, and
ELANUS_BUS_TOKEN in spawned children's environments. fs_read scoping (leg 2)
is the fix; until then any minted credential must be assumed agent-readable.

## 7. [LATENT] Package-manager launchers bypass code-hash pinning

Grants pin manifest + code hash of *referenced executables*
(manifest.rs:147). A run line of `npx foo` / `uvx foo` pins the one-line
launcher while the actual code is fetched remote and unpinned at spawn time
— the grant ceremony is intact and meaningless. Becomes real the moment a
kit ships an npx-launched MCP server or MQTT client. Mitigations to weigh
at design time: lockfile-pinned local installs at kit-install time (the
install gesture downloads, the hash covers what landed), or recording the
resolved version in the grant.

## 8. [LATENT] MCP surface (when [[mcp]] lands)

- **Tool poisoning**: server-supplied tool descriptions enter the prompt as
  trusted text; a hostile/compromised server injects through them. As built
  (2026-06-12): TOFU pin — sorted tools JSON hashed into kv on first load,
  mismatch refuses the tools until `elanus approve` re-pins. Two honest
  limits: the FIRST load is unreviewed (TOFU catches drift, not day-one
  malice), and the pin lives in harness.db, which entry 3 says is writable
  from inside every cage — so it is drift-detection, not a defense against
  a ledger-writing attacker. Pin-at-review (a sandboxed dry-run launch of
  the server during decide()) is the upgrade path; it was not built because
  it executes code as part of deciding whether to approve that code.
- **Port squatting**: harness-negotiated ports mean a local process can bind
  the port first or impersonate the server across restarts. Bind
  127.0.0.1 only; verify liveness via the harness-spawned pid, not the port.
- **Session/origin checks**: streamable-HTTP MCP without Origin validation
  and a per-launch token is reachable by browsers and any local process
  (same class as entry 5).

## 9. [LATENT] Linked kits are a cross-root supply chain

ELANUS_PATH-style linking means a shared package dir is writable by whoever
manages it — by design (single agent managing a fleet's packages). The
designed mitigation is per-root hash-pinned grants: upstream edit → stale
grant → re-review in every linking root. VERIFY when building: staleness
must be checked at dispatch/spawn time, not only at sync time, or the window
between edit and re-sync is an unpinned execution window.

## 10. [LATENT] History over HTTP exposes the crown jewels

Transcripts are the most sensitive data the harness holds. An
unauthenticated loopback HTTP endpoint serving arbitrary history queries is
readable by every local process and every caged agent (network uncaged).
Also: the query DSL must never pass through to SQL — the package owns its
schema precisely so the wire query is an interpreted structure, not a
sublanguage of SQLite.

## 11. [LATENT] Discovery via retained obs messages is spoofable

If negotiated endpoints (MCP, history HTTP) are announced as retained
messages under obs/, then until bus authz (leg 1) anyone local can publish a
counterfeit endpoint and become the history server. Until then, discovery
should come from the harness's own state (run/ dir), not the bus.

## 12. [DOCTRINE] Grant envelopes, not prompt position, are the injection
boundary

Untrusted payload reaching the model is a given (the funnel exists to point
agents at firehoses). The defense is the capability envelope per rung —
the injected funnel scout can only mail KEEP to the owner, which the sift
was going to do anyway. Prompt *position* (system vs message) is a severity
dial worth defaulting sensibly (stock context stages keep payload in message
position), but no design may claim safety from position alone. Kit-config
tampering is covered by grant review; payload content never is.

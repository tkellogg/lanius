# elanus v2 — Sandboxing: the cage and the camera

> Status: design, agreed 2026-06-10, same conversation as [bus.md](bus.md);
> this doc carries the authority model (grants/leases), fs sandboxing, and fs
> events. Nothing here is built. Same conventions: **[DECIDED]** is settled
> with rationale, **[OPEN]** needs a decision. MQTT citations are to MQTT 5.0
> (OASIS Standard, 2019-03-07).

## Doctrine

**[DECIDED]** Sandboxing and file-change events are different planes with
different failure semantics — the same interception/observation split as the
hook and observation planes (LSM hooks vs tracepoints, netfilter vs pcap):

- **The cage** — OS-enforced write restriction applied at spawn. Fail-closed,
  raceless, zero per-event cost. Seatbelt (macOS), Landlock + bubblewrap
  (Linux). It emits nothing; it is a static restriction on the action space —
  variety attenuation.
- **The camera** — `fs/...` events on the observation plane. QoS 0, advisory,
  droppable; feeds handlers, recorder rules, dashboards.

The dependency runs one way. Enforcement never depends on the event stream
(the cage doesn't depend on the camera — the black-box principle again), but
observation **completeness** is allowed to depend on enforcement: if the
subprocess tree can only write inside declared roots, then diffing those
roots captures *every* write by construction. Declared outputs make capture
sound — the Bazel/Nix insight. Without the cage, any watcher is a camera
pointed at one door of a building with many.

## Authority model: grants and leases

**[DECIDED]** Two kinds of authority, different objects with different
lifetimes — not two sizes of one thing:

```
human (root authority)
 ├─ agent GRANT       durable artifact, human-reviewed      "go wild in ~/Documents"
 │   └─ LEASES        dynamic, agent-acquired, ⊆ grant      "&mut this subtree"
 │       └─ spawn cage = the lease's write set, OS-enforced
 └─ package grants    durable; manifest-requested, approval-ledger-approved
                      (the request/approve flow lives in bus.md, Packages)
```

The Rust framing is literal, not metaphor: a write lease is `&mut`
(exclusive), reads stay shared (`&`), and **the kernel is the borrow
checker** — it refuses to issue overlapping write leases. Subtree semantics
are the textbook hierarchical lock manager (database intention locks):
exclusive on a directory conflicts with any lock beneath it.

**[DECIDED]** Leases cost nothing extra to enforce: a subprocess's spawn cage
*is* its lease's write set. A sibling cannot write into a leased subtree
because its Seatbelt/Landlock profile never included it. Exclusivity is
kernel bookkeeping at acquisition time; enforcement is the cage that exists
anyway. Side effect: write-attribution ambiguity between concurrent actors is
*prevented*, not detected — two writers on one subtree cannot coexist.

**[DECIDED]** Leases are crash-only: lease lifetime = dispatch lifetime,
recorded as a ledger row, released by the supervisor when the dispatch dies.
No lock leaks; same recovery path as everything else.

**[DECIDED 2026-06-10, built]** Acquisition surface: kernel tool call
(`fs_lease`) — it is a capability negotiation and belongs in the ledger.
As built: lease ⊆ grant is a canonicalized prefix check; conflict = prefix
overlap in either direction against active leases of other holders; holder
identity = enclosing dispatch (survives suspend/resume) else pid; release =
dispatch end, dead-pid reap, or clean standalone-exec exit. Holding leases
narrows the shell spawn cage to leases + harness root.

## The whole-agent grant

The human-authored outer envelope ("yes, go wild in ~/Documents"). Design
requirements in priority order; ergonomics deliberately last — tooling writes
this file, humans only review it:

1. **Subset-checkable.** `lease ⊆ grant` must be a decidable, boring
   function. That means canonicalized absolute path-prefix rules, not glob
   soup — globs invite symlink/TOCTOU surprises, and Landlock and Seatbelt
   both speak path prefixes natively, so globs would be translated away
   regardless.
2. **Deny-by-default**, explicit rules only.
3. **Audit-shaped.** Append-friendly; each rule carries provenance (when,
   why); revocation is an explicit entry, not a deletion — `git log` on the
   file reads as a capability history. Precedent: systemd unit hardening
   (`ReadWritePaths=`, `ProtectSystem=`) — declarative fs envelopes in a
   reviewable file beside the process spec.

**[OPEN]** Location: a `[grant]` section in profile.toml (keeps the
one-profile convention) vs its own file referenced from the profile. Lean own
file: its review lifecycle is security-artifact, not tuning-knob, and mixing
the two muddies what a profile diff means.

Deferred but named: `fs_read` scoping (secrets, injection-payload sources) —
the genuinely hard half; same grant vocabulary when it arrives. Network
egress — fs-write scoping without egress control is half a threat model: the
camera sees staging, exfil is a network event. Both get their own design
pass.

**[Note — security review 2026-06-11].** These two deferred passes turned out
to be load-bearing for *bus* security too, not just fs exfil. The
per-connection bus ACL (bus.md, Packages) cannot contain a malicious local
package while reads and loopback egress are open: the package's script can
read any privileged-client token and/or simply connect to the loopback broker
without credentials. So `fs_read` scoping + egress control are the
prerequisites for the bus ACL to be a *security* boundary rather than a
correctness one. Until they land, the enforced boundary against a hostile
package is the OS write-cage + the audit ledger, and packages are assumed
human-installed. See bus.md "Packages" → the KNOWN GAP block.

## The camera: fs events

**[DECIDED]** Topic = `fs` + canonical absolute path, leading `/` dropped:
`fs/Users/tim/code/elanus/src/main.rs`. Per-file topics buy **spatial
subscription** — `fs/Users/tim/code/elanus/#` is "watch this subtree": a
lease-holder watches its lease, an indexer watches the notes dir, nobody
filters by session to find a place. Limits, eyes open: MQTT filters cannot
express "*.rs at any depth" (`+` is single-level, `#` is tail-only) —
extension tripwires match on payload path in the handler; subtree scoping is
what the topic form is for.

**[DECIDED]** Path segments percent-encode exactly `+`, `#`, `%` (`%2B`,
`%23`, `%25`): wildcards are legal in filenames but illegal in topic names
[MQTT-4.7.1-1]. Filters are authored against the encoded form; nothing ever
decodes for matching. Paths are canonicalized (symlinks resolved) before
emit.

**[DECIDED]** Events come from **boundary diff**, not a live watcher. The
kernel snapshots the cage's writable roots at tool-call start, diffs at
tool-call end (stat-walk: mtime/size/inode cache, content-hash on suspects;
`git status --porcelain` fast path when a root is a repo), and emits one
event per changed file:

```
topic:   fs/<encoded canonical path>
cause:   <tool_use event id>            # attribution is structural, not inferred
payload: { op: create|modify|unlink|rename, renamed_from?,
           agent, session, dispatch, bytes, digest }
```

Rename is one event on the new path (`renamed_from` carries the old); the old
path gets an unlink-shaped tombstone. Attribution covers the whole process
subtree for free: cages are inherited across fork/exec, so everything a bash
script's grandchildren wrote landed inside the diffed roots.

Why boundary diff beats raw inotify/fsevents: the envelope has `cause`, and a
kernel watcher cannot fill it in — a raw write event is a causal orphan. The
dispatcher already brackets every tool call; the diff inherits the bracket's
identity. Gaps accepted for v1: write-then-revert within a single tool call
is invisible (transient staging — but exfil needs network, the cage's other
half), and there is no real-time tailing of long-running tools (a watcher may
later supplement *liveness*; the boundary diff remains the record).

**[DECIDED]** Burst policy. `cargo build` touches thousands of files; the
design must not pretend otherwise. In-process QoS 0 fan-out is cheap — the
real costs are the stat-walk and disk. So: recorder default for `fs/#` is
`none` (persistence is opt-in per subtree); grants and leases carry
capture-exclusion patterns (.gitignore-shaped: `target/`, `node_modules/`,
`.git/`). Exclusion is never silent: each tool call's delta includes an
`excluded` count.

Later option: retain the last event per file topic — "who last wrote this
file" becomes a subscribe, not a query.

## Platform notes

- macOS: Seatbelt profiles via the `sandbox-exec` mechanism —
  deprecated-but-functional; it is what Claude Code's own sandbox and
  Anthropic's sandbox-runtime use. Linux: Landlock (unprivileged policy) +
  bubblewrap (namespaces).
- Violations surface as EPERM tool errors the agent sees; the variety ladder
  handles the residue (retry differently, or `signal/pain`).
- Explicitly **not** the baseline: fanotify permission events (root,
  Linux-only), macOS Endpoint Security (entitlements), eBPF (privileged,
  observe-only), FUSE / overlayfs upper-layer diffing (upgrade paths if
  stat-walk ever measures slow — not the start).

## Open questions

1. ~~Lease acquisition API~~ — resolved: `fs_lease` tool call (see above).
2. Whole-agent grant location (profile section vs own file; lean own file).
   Still in profile `[sandbox]` as of step 5; package fs grants went to the
   approval ledger, the agent's own grant has not hoisted yet.
3. Exclusive **publish leases on topic prefixes** — `&mut ingress/discord/#`
   for the discord adapter = source authenticity by construction. Lean yes;
   the per-actor publish ACL (step 5) gives scoping but not exclusivity yet.
4. Zero-cage floor for packages: as built (step 5) = scratch-dir writes +
   approved fs_write, reads unrestricted (write-cage only), own
   `obs/skill/<name>/#` publish floor. Read-scoping and spawn policy for
   untrusted package roots remain open.
5. Resource limits for daemon actors (supervision currently has restart
   backoff only).
6. Default capture-exclusion set and where it is declared (grant vs lease vs
   profile).

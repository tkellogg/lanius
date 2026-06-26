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
holds ELANUS_DB). Closing one alone closes nothing. Network-egress control
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
elanus.db. So any caged process — the agent's own shell included — can edit
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

**[UPDATE 2026-06-13]** "no more" is now too generous. The identity
increments fenced the ledger (entry 3) and gated the CLI read-path on macOS,
but the web server did not change — it still reads the human secret at
startup and acts for any loopback client. So on macOS it is no longer an
equal of entry 3; it is the PRIMARY remaining path to human authority for a
caged agent. Tracked, with the precise mechanism, as entry 13.

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
  malice), and the pin lives in elanus.db, which entry 3 says is writable
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

**Stdlib exception (2026-06-15, docs/config.md).** `history` is now auto-approved
at init as a member of the protected `stdlib` kit, rather than left pending for a
human `approve`: the product (the web UI's sessions tab) depends on it, so
"present but off, with no way to turn it on" was itself a defect. Compensating
controls — the exposure is unchanged (still loopback-only, still the
confused-deputy surface of entry 13, not widened), and the package is now
*protected*: it cannot be silently revoked (`--force` required), so the auto-on
state is deliberate and visible. The read-confidentiality concern above is
unchanged and still wants the scoped, authenticated read plane; auto-approving
does not make it worse.

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

## 13. [OPEN-TIM] The web server is a confused deputy for the human credential

The CLI and the web server both authenticate as the human by reading
`<root>/.secrets/human` and presenting it at connect. They differ in WHEN
the read happens relative to the cage, and that difference is the whole
security story:

- The **CLI fuses holding and using the credential in one caged act.** An
  agent's shell running `elanus bus pub` reads `.secrets/human` at point of
  use, inside its own sandbox, so the Seatbelt deny-read rule fires and the
  read fails — on macOS the agent is refused. (On Linux pre-Landlock the
  read is not fenced — entry 6 / leg 2 — so the CLI loses this property
  there too; "the CLI is secure" is a macOS statement.)
- The **web server splits holding from using, across the cage boundary.**
  The human launches it uncaged; it reads the secret once at startup; from
  then on it acts as `human` for whoever reaches `127.0.0.1:7180`. A caged
  agent never touches the file — it `curl`s the loopback port and the server
  spends the credential on its behalf. The cage fences the FILE, not the
  PORT, and the network is uncaged (entries 5/10/11), so the server launders
  cage-fenced authority onto an unfenced channel. A classic confused deputy.

The Origin/Host guard does not close this: by design (server.mjs `originOk`)
a request with no `Origin` header passes — that guard targets hostile
browsers (CSRF, DNS rebinding), explicitly NOT local processes. So a caged
agent's `curl` reaches `POST /api/publish` (publish as human) and
`POST /api/admin/approve` (approve its own grants, decided_by=ui) unguarded.
This works on macOS too, where the file fence is solid — because the cage
fences the file, not the port.

Why this entry exists now: the identity increments (2026-06-13) closed the
direct-sqlite path (the ledger is fenced, entry 3) and the direct-CLI path
(the cage gates the read, on macOS). **The web server is the last confused
deputy standing on macOS** — the one side door that bypasses both, and the
reason entry 5's "no more" needed the update above.

The fix shape, for when this is taken up: the web surface must stop being a
free deputy. Either it requires the client to prove the human for mutations
— the layered model: free reads/converse, a gesture (notification tap or
passkey) gating approve and publish-as-human — or it becomes kernel-launched
with that gesture wired to high-stakes actions. An OS-level complement is to
deny caged actors loopback egress to the web port (a network-cage), so the
deputy is simply unreachable from inside the cage.

**[DECIDED 2026-06-13, Tim] Deferred.** "The CLI actually is secure afaict,
but the web UI is a hole. But that's fine. We can close that some other
time." Recorded so the web path is not mistaken for covered by the identity
work; not the next action. (When taken up, note that the web server resolves
the owner name + secret once at startup; with the configurable owner name
(entry-free identity work), a rename needs a re-read or per-request resolution
so the surface follows the renamed owner without a restart.)

## 14. [LATENT] Phonebook identity directory over unauthenticated loopback HTTP

The phonebook package (docs/identity.md) serves its read plane — `POST /query`
(resolve / identity / identities / channels / whois) — on a negotiated
127.0.0.1 port with NO authentication and no Origin/Host guard, exactly like
the history package. The network is uncaged (entries 5/10/11/13) and the port
is discoverable from `run/pkg-phonebook/http.json` (which the cage does not
fence), so any local process and any caged agent can `curl` it and read the
whole who-is-who graph: every identity, every channel address (Bluesky /
Discord / SMS / email handles), every alias, and the confidence/provenance on
each link. That is reconnaissance-grade for a prompt-injected agent (who can I
impersonate, who talks to whom, what addresses reach the human) — the same
class as entry 10 (history transcripts over HTTP), lower sensitivity but a
contact graph rather than message bodies.

Free reads are a deliberate design choice (the ingress-event / egress-command
split in docs/actors.md; reads mirror history), so this is recorded as
acceptable-for-v1, not a new defect. The fix arrives with the identity model's
authenticated read plane (the same work that closes entry 10). Mitigation
already present: the query DSL is an interpreted structure (the `QUERIES`
dict with bound parameters), never SQL passthrough, satisfying entry 10's
second requirement. WRITES are not exposed here — they go over the
authenticated bus, where the broker stamps the verified sender as provenance,
so an agent can only ever propose as itself (verified end to end).

**[UPDATE 2026-06-14]** The HTTP read plane is not the only exposure: phonebook
writes go over `in/package/phonebook/...`, which are ledgered events, so the
who-is-who graph (identities, channel addresses, links) also lands in
elanus.db as event payloads — readable by any caged agent via a raw `sqlite3`
read, the same cross-actor db-read gap that exposes transcripts and mail (the
deferred read-confidentiality item; section 0 / Linux read-fence). So the fix
"arrives with the authenticated read plane" only for the HTTP port; the raw-
ledger copy is closed by the read-fence work (Landlock / scoped reads), not by
authenticating the port.

## 15. [DECIDED 2026-06-14 / LATENT residual] Recall must key the correspondent on verified provenance

The recall context stage (docs/identity.md) assembles another identity's
cross-channel message *bodies* into the agent's prompt, keyed on who the
"correspondent" is — so the correspondent is authority-bearing, the same class
of data as entry 10. A review (verify-recall workflow) found the first cut
derived the correspondent from agent-forgeable fields (the event topic *and* a
`payload.channel`/`from` body field), so a prompt-injected agent could
`emit_event` to its own mailbox with `payload.channel = {victim}` and make
recall pull the victim's history into a run whose prompt it also wrote, then
exfiltrate it (`publish` grant permitting). **Fixed (2026-06-14):** the
broker-verified `sender` now rides into the context document's `event`
(src/exec.rs); recall takes the correspondent ONLY from the kernel-stamped
channel-faithful topic, NEVER a body field, and NEVER on an event whose
verified sender is the running agent itself (a self-forged dispatch). This is
the phonebook's doctrine applied to reads: identity comes from verified
provenance, never a chosen field.

**[LATENT] residual.** The topic of an `in/dm/...` event is trustworthy only
because a caged agent should not be publishing one. Today an agent's publish
grant can be broad (e.g. a `#` wildcard), so in principle one agent could forge
an `in/dm/...` event that is dispatched to *another* agent on the channel plane
(the self-sender guard only catches the agent forging its OWN dispatch). This
is the same class as the over-broad-grant problem generally. The deeper fix is
to reserve the ingress prefix — only ingress bridges, never agents, may publish
`in/dm/...` — which belongs with the actor-authorization work (narrowing agent
publish grants; bridges vs agents). Until then: keep agent publish grants
scoped, and treat channel-plane agent dispatch as opt-in.

**[UPDATE 2026-06-14]** The residual is reachable WITHOUT a broad bus grant:
the `emit_event` agent tool calls `events::emit` directly (a ledger write), not
a bus publish, so it is not checked against the publish ACL at all — an agent
can mint an event of any `type` (topic) that way. So "keep agent publish grants
scoped" does not fully bound it; the real fix is a reserved-prefix guard in
`events::emit` itself (refuse agent-origin `in/dm/*`), part of the same
actor-authorization work. recall's self-sender gate still holds (a self-forged
dispatch never recalls); the residual is the multi-agent case.

## 16. [LEGS / LATENT] Exec handlers publish as the owner, not as themselves

Reacting (`mode = "exec"`) package handlers are spawned by the dispatcher
UNCAGED and WITHOUT a per-spawn token (src/dispatcher.rs spawn_handler — no
`ELANUS_BUS_TOKEN`/`ELANUS_PACKAGE`, no cage; contrast the daemon path which
injects both and cages). So when an exec handler runs `elanus bus pub`,
buscli finds no package token and — because it is uncaged — reads
`.secrets/owner` and authenticates AS THE OWNER; the broker then stamps the
event `sender = owner`. (An exec handler using `elanus emit` instead writes the
ledger directly with `sender` falling back to `kernel`.) Either way an exec
handler's emitted events are mis-attributed — never to the package that
actually produced them. This inverts the identity model's whole point
(provenance is the verified sender) for the exec-handler path, and it is
especially dangerous in a *template*: a naive egress bridge written as an exec
handler labels every outbound send as owner-originated.

This surfaced building the webhook egress exemplar (increment 5). The exemplar
was made a **daemon** instead — daemons are spawned token-authed and caged, so
webhook's receipt correctly carries `sender = webhook` (e2e section 20 asserts
it). That is the right shape for any emitting/egress bridge and the documented
recommendation (docs/actors.md). The general fix for the exec-handler path is
part of the deferred exec-handler containment work (entry 1 / leg 3): spawn
exec handlers token-authed as their package (and set `ELANUS_ACTOR` so the
`elanus emit` path attributes correctly too), so their publishes attribute to
the package and their declared publish grants are actually the thing enforced.
Until then, an emitting handler's `sender` should be read as the surface
(owner/kernel), not the package, and bridges that must attribute correctly
should be daemons.

## 17. [LATENT, found 2026-06-15] Revoking a grant does not stop a running daemon actor

Observed while building the stdlib protected kit (docs/config.md): after
`elanus revoke history --force`, the history HTTP actor kept answering on its
negotiated port — the web smoke polled `/api/history` for 30s expecting the
honest 503 of a withdrawn capability and never saw one. So revocation flips the
grant row to `revoked` in the ledger, but the supervisor does NOT tear down an
already-running daemon actor: the withdrawn capability (here, serving the
transcript crown jewels of entry 10) stays live until the actor is re-spawned or
the daemon restarts. Revocation is effective at the next (re)spawn, not at once.

In scope per the threat model: a human — or, soon, an autonomy rule — revoking a
capability reasonably expects it to stop. Not a confirmed exploit path (revoke
is rare; the actor is loopback), but "revoked" currently overstates what has
happened. Fix when the supervisor/reload seam is touched (config model
increments 2–4): on revoke, signal the supervisor to stop/restart the affected
actor so the running process matches the ledger — the same
`obs/config/changed` → restart path planned for config reload (HANDOFF D3).
Until then, treat revoke as effective on the next daemon cycle; for an immediate
stop, restart the daemon.

## 18. [LATENT, found 2026-06-15] Git config-repo ops must neutralize filters/attrs before the increment-3 untrusted checkout

Found by the adversarial review of config-model increment 2 (the config repo,
docs/config.md). `src/config_repo.rs` is the first git integration; D2 promises
every git op runs with "hooks AND filters OFF." The original `git()` builder
set `core.hooksPath=/dev/null` and removed `GIT_DIR`/`GIT_WORK_TREE` (both
verified load-bearing) but did NOT neutralize the operator's global/system
gitconfig or attribute-driven clean/smudge filters. Reproduced under the exact
kernel flags: a `[filter] smudge=…` in a hostile `~/.gitconfig`, and a repo-local
`filter.*.smudge` + tracked `.gitattributes`, BOTH executed shell on
`git checkout -- <file>`.

Not exploitable in increment 2 as shipped: no code path checks out or fetches
untrusted content (init/set_key/add/commit/status/rev-parse/read all touch
kernel-authored content only). Mitigated now anyway — `git()` exports
`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`, `GIT_ATTR_NOSYSTEM=1`
(ambient config/aliases/filters can no longer fire; behavior is deterministic
across machines), and the module doc states what is enforced.

Becomes live in increment 3, whose design REUSES this builder to fetch + diff an
UNTRUSTED agent clone. A `.gitattributes` travels in the fetched tree; if that
path ever does a working-tree *checkout*, a smudge filter is code execution in
uncaged kernel territory. Increment 3 MUST read fetched content via plumbing
(`git cat-file`/`git diff`), never a checkout that runs smudge — or explicitly
disable attribute filters for that op. Cite this entry in the increment-3 design.

## 19. [FIXED 2026-06-16] Config-proposal acceptance: tree-mode + case-fold bypasses (found by the increment-3/4 adversarial workflows)

The increment-3/4 review workflows (agent proposes config via an untrusted Git
clone; autonomy auto-accepts) found a real, reproduced **arbitrary-file
disclosure** and several siblings. All FIXED + regression-tested; recorded here
because the class (judge a proposal by NAME, act on its CONTENT) will recur.

- **[was CRITICAL] Symlink/gitlink/exec tree entry bypassed path-discipline.**
  `proposal_packages` judged changed files by NAME only (`git diff --name-only`,
  blind to the blob mode). A `packages/<pkg>.toml` committed as a symlink (git
  mode 120000) passed, merged into kernel-owned `live`, and `read_package`
  (`config list/get`, the web UI) followed it — returning the owner credential
  or any file (`/etc/passwd`). Auto-accepted at autonomous AND assisted (the
  symlink's target-path blob isn't valid TOML → empty changed-keys → Accept),
  and it defeated human accept too. FIX: path-discipline now enumerates `git diff
  --raw` and REFUSES any entry whose dst mode isn't `100644` (or `000000` =
  deletion) — rejecting 120000/160000/100755 in the always-stop gate, before any
  merge, for human and auto-accept alike. Defense in depth: `read_package`
  refuses to follow a symlink (`symlink_metadata`).
- **[was HIGH] Reap's clone `.git/config` neutralization followed an
  agent-planted symlink** → arbitrary-file OVERWRITE in uncaged kernel context.
  FIX: require `.git` to be a real dir (`symlink_metadata`), `remove_file` the
  config first (deletes the link, not its target), then a CHECKED write; on any
  failure, fail closed (don't read the clone).
- **[was HIGH] Case-folded package name bypassed the protected-stdlib stop.**
  `History.toml` ≠ `history` in the case-sensitive protected set, but collides on
  a case-insensitive fs — an autonomous agent could auto-merge over a protected
  package. FIX: `valid_pkg` now rejects non-lowercase names (one canonical
  chokepoint), and the protected check compares case-insensitively.
- **[was MEDIUM/LOW] DoS bounds.** A proposal's added objects are measured in the
  clone and refused above `MAX_PROPOSAL_BYTES` (1 MiB) BEFORE the fetch copies
  them into the live store; the changed-file count is capped (`MAX_PROPOSAL_FILES`).

DEFERRED (LOW, noted not fixed): an `ask_human` suspend (`exit 75`) bypasses the
reap, so a suspended run's proposal is harvested only when it finally terminates
and its clone leaks until then (a UX edge, not a security hole).

PROCESS NOTE: the adversarial workflow agents disregarded the "/tmp only" safety
preamble and ran `elanus`/`git` in the repo itself — leaving 501 junk
`proposal/*` branches, a stray junk commit, artifact files, and orphaned /tmp
servers (one squatted an e2e port and caused a spurious §20 failure). All cleaned
+ `/config/` gitignored. Re-run such workflows with `isolation: "worktree"` per
agent so their git side effects can't touch the main repo.

## 20. [FIXED 2026-06-19] Coding-session credential was a full-authority principal (entry-16 sibling, found by the coding-agents verifier)

The first coding-agents slice (`elanus code launch claude`, branch
`coding-agents`) minted each session's credential as a **plain fenced secret**
named `code-<session>` in `Root::secrets()`. The broker resolves a fenced secret as
a **full-authority principal** (`actor = None`), and every bus ACL gate is
`if let Some(pkg) = &actor` — so for a session credential **no gate ran**. A minted
`code-<session>` token (verifier proved it live) could publish to `in/human/owner`,
`work/agent/exec`, and other agents' mailboxes, and subscribe `obs/#` (read every
agent's telemetry). Attribution was correct and forge-resistant (`sender =
code-<session>`, the egress lesson of entry 16 was honored), but **authority was
owner-equivalent** — exactly the inverse of entry 16's other half: there an emitter
mis-attributes; here it attributes correctly but over-authorizes. A SIGKILL also
leaked the live full-authority credential (no reaper).

FIX (grant-scoped session token, src/codesession.rs + src/broker.rs): the session
credential moves to a fenced sub-store (`Root::secrets()/code-sessions/`, still
cage-fenced — the forge asymmetry is unchanged), and the broker resolves a `code-*`
principal **before** the fenced-secret path as a **grant-scoped actor** (`actor =
Some`), so the ACL gates run. The scope is **structural** (a session has no
manifest): publish ONLY the session's own `obs/agent/<agent>/<session>/#`, subscribe
nothing. This copies the webhook daemon's grant-scoped shape (entry 16's prescribed
fix) rather than inventing identity. The leaked-on-crash hazard is closed by a reaper
(`codesession::reap_orphans`, run at daemon + launcher boot, signal-0 pid probe like
the lease reaper). Proven live: the four publish attacks + the `obs/#` subscribe now
`NotAuthorized`; the session's own obs still publishes (stamped `sender =
code-<session>`); a reaped orphan is refused at CONNECT. Regression-tested at the
**authority** level (broker ACL denies a session actor outside scope), the gap the
prior shape-only suite missed. RESIDUAL: this fixes the bus-authority gap only; M0's
complete-cage criteria (read/egress denial onto the elanus OS cage) stay deferred per
the handoff sandbox stance — the tool keeps its own OS sandbox for now.

## 21. [FIXED 2026-06-20] M4-A completion routing: confused-deputy `reply_to` + cross-victim idempotency suppression (found by the M4-A verifier)

The M4-A orchestration loop (a worker's completion is routed back to the requester's
mailbox by the **daemon's own authority**, so a planner is resumed — branch
`coding-agents`) shipped two MEDIUM holes the adversarial verify turned up. Both are
in the daemon's mediated routing, not the session token (which stays emit-only — the
authority was never widened, per entry 20).

**(a) Confused-deputy `reply_to`.** `codeagent::delivery_requester` accepted an
explicit payload `reply_to` that merely `starts_with("in/")` and was wildcard-free,
then the daemon routed a **kernel-authored** completion to it **verbatim**. So a
delivery carrying `reply_to: in/human/owner` (or any `in/...` / `signal/` / `obs/` /
`work/` topic) made elanus publish a kernel-authored message to the human inbox or an
arbitrary topic — the classic confused deputy (a low-authority requester borrows the
kernel's authority to write where it cannot). FIX: an explicit `reply_to` must now
**resolve to a recognized actor's mailbox** the same safe way the sender-derived path
does (`resolve_reply_to` → `mailbox_for_actor`): a coding session (`code-*` with a
durable record) → its own session mailbox; a valid agent name → `in/agent/<agent>/
<conv>`. Both a bare actor NAME and a full `in/agent/<noun>/<conv>` mailbox topic are
accepted, but the topic is never used verbatim — the actor is extracted and the
mailbox **re-derived**. A raw/arbitrary `in/...` topic, `in/human/*`, `in/group/*`,
`signal/`, `obs/`, a wildcard, a path-unsafe name, or an unrecorded `code-*` conv all
resolve to None (no route). `mailbox_for_actor` now also requires `valid_principal`
so a path-unsafe/reserved name can't be coaxed into a non-agent topic level. Proven
live (worktree stack, broker :1893, NO model turn — fail-fast worker resume): a
delivery with `reply_to: in/human/owner` and one with `reply_to: in/totally/
arbitrary/x` both captured `reply_to: null` (route refused); the `in/human/owner`
kernel-event count was unchanged and the arbitrary topic had zero events; a
legitimate `reply_to: code-<planner>` still routed the completion to that planner's
own mailbox (sender=kernel, same correlation).

**(b) Cross-victim idempotency suppression.** `code_delivery_keys` was keyed on
`idempotency_key` alone (GLOBAL). An attacker who pre-claimed an explicit key `K`
(via a delivery to their own session A) silently **suppressed** a *different* victim's
delivery to a *different* session B that reused `K` — B was settled `done` as a bogus
duplicate and never driven (a denial of the victim's orchestration step). FIX:
namespace the dedupe by the **target session** — `PRIMARY KEY (session,
idempotency_key)`, and `claim_delivery_key`/`delivery_key_seen` are per-session. An
explicit key now only ever dedupes a delivery to the SAME session; one principal's
key can never collide with another's delivery to a different session. The default
`event:<id>` key is globally unique regardless, and the genuine same-session replay
dedupe (the at-least-once protection) still holds across a restart. Pre-release, the
table was just dropped+recreated (no migration). Proven live: with `K` pre-claimed
for session A, a victim delivery to session B reusing `K` was DRIVEN (`delivery/
accepted`, not `duplicate`), and `K` is now recorded independently for both sessions;
the genuine replay (same key, same session, re-pended `running` across a SIGKILL +
restart) was still a recognized `delivery/duplicate` no-op with zero second resume.

**(c) Lost planner wake on a crash (reliability, not a hole).** The same fix pass
closed the disclosed M4-A residual: the settle UPDATE (worker delivery → `done`) and
the routed completion emit are separate autocommit transactions, so a crash between
them settled the worker but lost the planner's wake forever (the boot sweep only
re-pends `running` events, never `done`). FIX: a boot reconciliation
`reconcile_lost_routes` (src/dispatcher.rs) walks the durable `code_delivery_keys`
rows (each marks a delivery actually driven), re-derives the requester from the
original delivery event's persisted `sender`/`payload`/`correlation`, and re-emits
the route if none was ever emitted (`route_already_emitted` guard) — idempotent and
crash-only, like the other boot sweeps. Proven live: a crafted settle→route-gap crash
state recovered exactly one route on restart, and a second restart routed nothing.

Regression-tested (`cargo test`, 134 green): the rejected-`reply_to` probes
(`explicit_reply_to_cannot_target_human_inbox_or_arbitrary_topic`), the cross-victim
non-suppression (`cross_victim_key_does_not_suppress_a_different_session_delivery` +
`delivery_key_is_namespaced_by_session_no_cross_victim_suppression`), and the
crash-recovery (`reconcile_recovers_a_route_lost_in_the_settle_route_gap`,
`reconcile_skips_deliveries_with_no_requester`). RESIDUAL: none for these; the session
token authority is unchanged (the daemon still routes with its own authority, just to
a constrained, validated destination).

## 22. [DOCTRINE + LATENT, 2026-06-20] Delegated authority is not bounded by the spawner

The recurring class in this file is "an actor ends up with more authority than its
position warrants" — entry 20 (a session credential was owner-equivalent), entry 16
(an exec handler emits as the owner), entries 13/21 (confused deputies borrow a
higher surface's standing). Underneath them is one missing rule, now stated as
doctrine: **a spawned actor's authority must be a strict subset (≤) of its
spawner's, reconstructed at spawn and enforced at mint** — `child.grants ⊆
parent.grants`, monotone down the spawn chain (Tim, 2026-06-20; design in
docs/handoffs/authority-delegation.md, doctrine in docs/identity.md "Delegation").

Why DOCTRINE: it is the line to hold — no spawn may widen authority, ever. Why also
LATENT: the invariant is **not yet enforced**, and one half of the mechanism is
already right while the other is missing:

- **Right today.** The worker-spawn path re-mints rather than inherits:
  `scrub_spawn_wrapper_identity_env` strips the parent's `ELANUS_PACKAGE`/
  `ELANUS_BUS_TOKEN`/`ELANUS_CODE_SESSION` and the wrapper mints a fresh scoped
  token (entry 20). A tool the session shells (e.g. `elanus bus pub`) *correctly*
  inherits the session's own identity (the trivial subset — it is the same actor
  using a tool). And one capability dimension is already subset-enforced:
  `lease ⊆ grant` for filesystem writes (docs/sandbox.md).
- **Missing today.** The minted child scope is a **flat per-kind constant** (entry
  20's structural `obs/agent/<agent>/<session>/#`), **independent of the spawner** —
  a worker spawned by a tightly-scoped agent gets the same bus scope as one spawned
  by the owner. There is **no enforced `child ⊆ parent` check** at mint, **no
  unified `Grants` value** that gets subsetted (authority is scattered: bus ACL in
  the broker, fs as `lease ⊆ grant`, cost as a profile `max_turns`), and **no
  fungible budget dimension** (`Σ children ≤ parent` for turns/cost/fan-out — the
  RLM "halve it to pass context down" case).

Not a confirmed exploit on its own (every code-session is already bounded to its own
obs subtree by entry 20, and re-minted not inherited) — it is the *absence of the
bounding relation* that would let a future, broader-grant spawner hand a child
authority it should not, and the absence of the monotone guarantee that makes the
whole cage reason-about-able ("no descendant exceeds any ancestor"). The fix is the
delegation handoff: a `Grants` value, `narrow(parent, request)` with `⊆`/`Σ≤`
asserted at mint (the same decidable check sandbox.md demands of `lease ⊆ grant`),
the broker/sandbox enforcing each dimension at runtime. Build the budget dimension
first (fungible, zero behavior change, ships the assert skeleton), then the
capability subsets. Cite this entry in that work.

**[M1 LANDED 2026-06-20]** The **budget dimension** is now enforced (the assert
skeleton the doctrine called for, built first as recommended). `SessionToken`
carries `turn_budget`/`remaining_budget` (`None` = unbounded; `#[serde(default)]`
so pre-M1 tokens read as unbounded). `codesession::mint` gained `spawner` +
`requested_budget`: it reads the spawner's **remaining from the fenced token store —
never from env** (the doctrine's "reconstructed at spawn, not inherited"), asserts
**`Σ children ≤ parent.remaining`** at mint, decrements-and-persists the spawner's
remaining before writing the child, and **refuses the spawn** (clear, entry-22-citing
error) if it would over-allocate. Default policy = **inherit-equal** (narrow only on
explicit request; open-decision 1). Owner path (spawner `None` / no token) stays
unbounded and lock-free → zero behavior change for every existing call site.

The interesting part is what adversarial validation caught before this shipped — two
real over-grant bugs in the code whose *whole point* is the bound:

1. **Concurrent-sibling TOCTOU.** The `Σ` decrement was a non-atomic
   read-check-write with no cross-process serialization, yet spawned workers are
   separate detached processes (`elanus code spawn` → `cmd.spawn()`), so the RLM
   fan-out the doctrine names could let N siblings each read the same stale
   remaining and all pass → `Σ > parent`. Fixed with a cross-process advisory lock
   (`libc::flock(LOCK_EX)` on `<store>/budget.lock`, RAII-released on every path
   incl. the `bail!` refusal and panic), wrapping the entire read→check→decrement→
   write-back. Owner/unbounded path never takes the lock.
2. **Torn-read fail-OPEN.** A first lock attempt still leaked: a lock-free "peek"
   classified unbounded-vs-finite *before* the lock, and `write_0600` truncated
   in place — so a sibling reading during another's write got a partial file,
   `serde` returned `None`, and an *unreadable* token was treated as *unbounded*
   and granted lock-free. Closed by (a) making `write_0600` atomic (temp +
   `rename(2)`, no reader ever sees a partial token) and (b) gating the lock-free
   path on the spawner token **file existing** (`try_exists`, a stable signal),
   not on a parse result — an existing-but-unparseable token now **fails closed**
   under the lock. A corrupt/half-written spawner token can never read as
   unlimited authority.

Both were proven load-bearing by neutralization (revert a fix → the 60-iter ×
12-thread `budget_concurrent_siblings_cannot_exceed_parent_via_race` test fails
deterministically; `budget_unparseable_spawner_token_fails_closed` covers the
fail-closed path). 7 budget regression tests; full suite 181 + 2 doctests green.

Still LATENT after M1 (not closed by this increment): the **capability** dimensions
(bus ACL, fs read, tool/command allowlist, `blocking`) are still flat per-kind
constants, not `subset(spawner)` — that is M2/M3. And the *spawner name* (which
token to charge) is still taken from `ELANUS_CODE_REPLY_TO` (env), while only the
*value* comes from the fenced store; a caged actor naming an arbitrary existing
session as its spawner is bounded (it cannot forge a fenced token) but is the
env-as-authority-key seam to close when budget becomes runtime-enforced (M2+,
marked with a `TODO` at the spawner lookup). The cross-process lock guarantee is
unix-only by construction (the whole 0600/flock model is POSIX).

**[M2 LANDED 2026-06-21]** The **bus capability dimension** now narrows by
`child ⊆ spawner`, and the scattered dimensions are unified into one **`Grants`**
value carried on `SessionToken` via `#[serde(flatten)]` (on-disk JSON unchanged —
M1 and pre-M1 tokens still deserialize). `mint` gained `requested_publish`/
`requested_subscribe`; when the spawner token file exists, under the *same* M1
`flock` it asserts:
- **subscribe (read authority): strict** — every child filter must be `covers`-ed
  by some spawner subscribe filter (`child.subscribe ⊆ spawner.subscribe`), else
  the spawn is refused. (Sessions get empty subscribe today; this is the
  forward-looking guard.)
- **publish:** the child may **always** emit its own structural self-telemetry
  subtree `obs/agent/<agent>/<session>/#` (its own audit trail — disjoint,
  write-only-own, *not* a widening), plus any filter `covers`-ed by the spawner's
  publish grants; anything else is refused.

The decision was the design fork M2 raised: entry-20 gives each session a
*disjoint* obs subtree, so a literal `child ⊆ parent` on publish would forbid a
child emitting its own telemetry. Resolved as above — read authority narrows
strictly; self-telemetry is always allowed because it is own-data-only and
structurally disjoint from everything else (the child's `own_obs` is built from the
*child's* launcher-set principal/agent, so it cannot name another session's
subtree). Owner-spawned sessions (no spawner token) get the exact entry-20
structural scope unconditionally — **zero behavior change** for the common case.
The broker is unchanged: it already gates `code-*` actors from the token's
publish/subscribe, and runtime subscribe (exact-match) is *stricter* than the
mint-time `covers` containment, so no widening.

Soundness rests on **`topic::covers(wide, narrow)`** — a decidable filter-
containment with a conservative "deny when unsure" bias, including the MQTT
`$`-topic rule (a root wildcard does not cover a `$`-anchored filter — without it
`covers("#", "$x/#")` falsely reports ⊆, an overstated guarantee = a defect). It
is proven by a brute-force soundness oracle (`covers(w,n) ⟹ ∀ topic: matches(n,t)
⟹ matches(w,t)`) over a generated filter/topic alphabet incl. `$`-cases, plus
adversarial review with an *extended* oracle outside the shipped alphabet — no
widening counterexample found. 198 + 2 doctests green; clippy clean on the changed
files.

Still LATENT after M2 (M3): fs read/write, the tool/command allowlist, and
`blocking` are still flat per-kind, not `subset(spawner)`. The env-keyed
spawner-name residual (above) is unchanged — still M2+ follow-up.

**[M3 LANDED 2026-06-21]** The delegation contract is now **complete across every
named capability dimension** — `Grants` carries `fs_write`, `fs_read`,
`tool_allowlist`, and `blocking` (each `Option<Vec<String>>`, `#[serde(default)]`,
`None` = unbounded), and `mint` asserts `child ⊆ spawner` for ALL of them under the
*same* M1 flock, via one uniform `narrow` contract: budget keeps its fungible
`Σ ≤ parent`; bus keeps its M2 `covers`; the four capability dims use two decidable,
deny-when-unsure primitives — **path containment** (`topic::path_covered`:
component-wise `Path::starts_with` over lexically-normalized absolute paths — no
`canonicalize`, since grant prefixes need not exist and symlink resolution stays a
runtime cage concern; `/a/b` covers `/a/b/c` but not `/a/bc`; `..`-escape and
relative/empty prefixes deny) and **exact set-membership** (tool/blocking). Every
child entry is checked (loop, not first-only); a child can never flip a bounded dim
to unbounded. The request side is unified into `RequestedGrants` (retiring the
`mint` arg-count smell). Scope was deliberately **mint-bound** (Tim's call): the
contract is recorded and enforced at spawn for all four; *runtime* enforcement is
unchanged — `fs_write` keeps its existing cage/`acquire_lease` `lease ⊆ grant`
(profile-driven, untouched), and `fs_read`/`tool_allowlist`/`blocking` are
mint-bound only, runtime-enforced later (mirroring sandbox.md's read-scoping
deferral). Owner-spawned sessions get all four dims `None` (unbounded), lock-free —
zero behavior change. 217 + 2 doctests green; clippy clean on the touched modules.

Adversarial validation (committed-tree, no git-mutation) found one **latent HIGH**
before finalize: an empty-string `wide` prefix made `path_covered` a silent root
wildcard (`"/etc".starts_with("")` is true in Rust) — not reachable in shipped M3
(no production path mints `Some([""])`; sessions get `None`) but a root-wildcard
escalation the moment fs grants get populated (M4 / deferred runtime). Fixed by
requiring `wide` prefixes be absolute (rejects empty *and* relative degenerate
prefixes) + a regression test. Lesson for M4: when the `--grants` CLI populates fs
grants, **validate prefixes at construction** (absolute, non-empty) too.

Still LATENT after M3: runtime enforcement for `fs_read`/`tool_allowlist`/`blocking`
(mint-bound only today); and the env-keyed spawner-name residual (unchanged) — the
`ELANUS_CODE_REPLY_TO` lookup should move onto a capability reference. M4 (optional)
is the `--grants`/budget CLI surface for deliberate narrowing.

**[M4 LANDED 2026-06-21 — handoff COMPLETE]** Narrowing is now first-class in the
UX. `elanus code <tool>` accepts `--budget <N>` and repeatable
`--grant-{publish,subscribe,fs-write,fs-read,tool,blocking}`; `take_grants_flags`
(src/codeagent.rs) strips them from argv (the rest is forwarded to the tool
verbatim), **validates at construction** (numeric budget; `topic::valid_filter` for
bus filters; fs paths must be absolute, non-empty, whitespace-free, contain no `..`,
and name a real directory below root — `/`, `//`, `/../..` are refused, closing the
M3 root-wildcard footgun *at the door*; tool/blocking non-empty), and `launch`
threads the resulting `RequestedGrants` into `mint`. Absent flags ⇒
`RequestedGrants::default()` ⇒ byte-identical prior behavior. The CLI only checks
well-formedness; the *bound* is still mint's `child ⊆ spawner` / `Σ ≤ parent`
(unchanged) — e.g. `--grant-publish '#'` parses fine but a child is refused it
unless its spawner holds it. Acceptance met (e2e tests asserting refusals): an owner
`--budget 4` session has remaining 4, and a child requesting >4 (or an fs_write /
publish outside an owner-set grant) is refused. Adversarial validation found **no
bypass** (every requested grant is re-checked at mint); residuals were LOW and
addressed (degenerate-absolute hardening above; a vacuous-pass test made
unconditional). 241 + 2 doctests green; clippy clean on the touched module.

The `--budget`/`--grant-*` names are a **reserved elanus flag namespace** — they are
stripped from anywhere in argv before the tool sees them (no `--` end-of-options
sentinel today), so a future tool adopting one of these names would have it captured
by elanus. Acceptable now (claude/codex use none of them); revisit if it bites.

Residuals carried past the handoff (all recorded, none blocking): (1) runtime
enforcement for `fs_read`/`tool_allowlist`/`blocking` is still deferred (mint-bound
only) — when it lands, the token grant becomes the cage's source instead of the
profile; (2) the env-keyed spawner-name lookup (`ELANUS_CODE_REPLY_TO`) should move
onto a capability reference before budget/grants become load-bearing at runtime;
(3) async `spawn` does not yet forward `--grant-*` flags to detached workers (mint
still prevents widening — it just can't pass an explicit narrowing request through
the async path); (4) the cross-process lock is unix-only by construction.

## 23. [DESIGN 2026-06-25] The credential vault: encrypted-at-rest, accidental-disclosure scope

Model providers (docs/handoffs/model-providers.md, M1) store a real secret — an
API key, plus any secret extra-header values (LiteLLM/OpenRouter) — in the ledger.
A provider is **a resource, not an identity or an authority** (entry 2 / Tim's
"just a resource"): no `child ⊆ parent` narrowing, no phonebook entry; choosing one
for a child is **audited on the session-start obs, not gated**. The vault is the
`providers` table (src/provider.rs): **clear** columns for non-secret metadata
(name, kind, wire, base_url, header *names*, tool) so `list`/`get`/`test`/the UI are
plain queries, and **one sealed blob** (`secret` + its random 24-byte `nonce`)
holding the serialized secret material. A `NativeLogin` row carries no blob.

**Crypto.** XChaCha20-Poly1305 (`chacha20poly1305`), random per-row nonce stored
beside the ciphertext, sealed under a 32-byte master key at `<root>/secret.key`
(`0600`, generated on first use — sibling of the `.secrets` store, entry 6). Reads
are **fail-closed**: a wrong-size/missing key, a tampered nonce, or a tampered
ciphertext yields an error, never garbage plaintext (the AEAD tag is checked). The
secret is decrypted **only transiently in daemon/CLI memory** at `materialize`/`test`
time; it is never written to obs, the config git, logs, or printed — the CLI shows
`••• (encrypted)`, and the `Secret` newtype redacts its own `Debug`, so an
accidental `{:?}` cannot leak it. `materialize(credential, consumer)` is a partial
function over the validity matrix; the literal key it returns lives in a `Secret`
the caller must `.expose()` deliberately. For the harness shapes the key never
touches the command line: claude takes it via `ANTHROPIC_AUTH_TOKEN` env, codex via
`env_key`/`env_http_headers` (env, referenced from `-c` config — never a `-c` value),
opencode inside the `OPENCODE_CONFIG_CONTENT` env JSON.

**What this defends — and what it does NOT.** This is "safety = audit, not
restriction" (entry 0 / the doctrine note): the real threat is **accidental
disclosure** — a key landing in git, a `.db` backup, an obs stream, a `SELECT *` an
agent runs, a screen-share. Encryption-at-rest closes all of those: a raw
`SELECT * FROM providers` reveals no key (verified). It does **NOT** defend against
an attacker who already has full filesystem read as the elanus user — they can read
`secret.key` and the blob together — and chasing that would be theater. The
**file-key tradeoff** is deliberate (handoff Open Decision 1): the master key on
disk keeps the headless daemon auto-starting (no keyring on a systemd box, no
passphrase prompt). The honest upgrade path, deferred: a keyring-backed master key
*when a keyring is actually present and unlocked*, or `ELANUS_SECRET_KEY`/passphrase
at daemon start for a key that never touches disk (at the cost of unattended
auto-start). LATENT until then: anyone with FS read as the elanus user reads every
stored credential.

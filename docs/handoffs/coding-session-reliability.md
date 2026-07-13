---
status: done
author: Terra/high planner (Codex)
last-updated: 2026-07-11
---

# Handoff: coding-session reliability

Make coding-session coordination say only what its evidence supports, make a
claim disappear everywhere when it is released, and let a second `lanius dev`
choose usable browser ports without extra flags. This is a small reliability
sprint, not a redesign of coding-agent supervision or dispatch.

## Read These First

- [sibling-awareness.md](sibling-awareness.md) -- the shipped default-room and
  auto-claim design. This handoff tightens its honesty and lifecycle behavior; it
  does not replace it.
- [read-provenance.md](read-provenance.md) -- why harness tool events are useful
  but advisory evidence, and why an authoritative filesystem camera is separate
  work.
- [../../src/codeagent.rs](../../src/codeagent.rs) -- `whose_cmd` / `whose_line`
  / `whose_json` (~2597), `claim_cmd` (~2811),
  `canonicalize_claim_path` (~2889), `auto_claim_write` (~2915), and
  `unclaim_cmd` (~2961).
- [../../src/codesession.rs](../../src/codesession.rs) -- the durable
  `code_claims` operations (`add_claim`, `remove_claim`, `peer_claims`,
  `own_claims`) (~1180-1230) and `whose_path` (~1350).
- [../../src/mailcli.rs](../../src/mailcli.rs) -- `room_claims` and
  `rooms_cmd` (~350-410), the human-facing reader of the same claim rows.
- [../../src/dev.rs](../../src/dev.rs) -- `run` (~19) owns the isolated dev root,
  bus selection, and web/Vite port resolution; `first_free_port` (~601) is its
  deliberately best-effort probe.
- [../../src/main.rs](../../src/main.rs) -- the `Dev` CLI flags (~170) and the
  call into `dev::run` (~924). This handoff identifies the required change but
  does not edit that file in this planning pass.
- [../../AGENTS.md](../../AGENTS.md) -- local development rules. The skill-link
  invariant below belongs here, not in the Lanius capability model.

## Wonky Bits And Decisions

1. **A claim is evidence, not ownership.** `code_claims` records a present
   manual claim or a harness write-tool signal. It does not prove who made a
   filesystem change, and `last_active`, a task, a matching workdir, or an
   untracked path must never fill that gap. `lanius code whose` may say **yours**
   only when its viewer session has a present claim for that canonical path. With
   no such signal, it must say `unattributed/unknown`; remove the current
   `likely yours` guess. A different live session's present claim may be shown as
   `claimed by <session>` with its evidence, never upgraded to proof.

2. **The unclaim failure is a normalization seam, not a delayed projection.**
   `claim_cmd` canonicalizes `src/foo.rs` against the recorded workdir before
   inserting it, while `unclaim_cmd` sends the raw spelling to `remove_claim`.
   The delete therefore misses the stored absolute path and honestly reports no
   row removed, even though the row remains visible. `claims`, `whose`, peer
   injection, and `code rooms` all read `code_claims` directly; there is no
   separate claim projection to wait for. Both commands must use one resolver.

3. **Port shifting becomes the default, including the isolated dev bus.** The
   requested web relay and Vite ports remain 7180 and 5173, and the dev broker's
   requested port remains 11883. By default `lanius dev` probes upward for free
   values. `--fixed-ports` is the explicit opt-in to bind those requested values
   exactly and fail normally on a conflict. The banner must continue to say that
   the broker is in `target/lanius-dev`, separate from production's
   `~/.lanius/root` and port 1883. That is isolation from production, not a claim
   that two concurrent dev supervisors have fully independent state.

4. **The skill link is project tooling.** Add one narrow `AGENTS.md` rule:
   `.codex/skills` must remain a symlink to `../.claude/skills`; do not replace
   it with a copied directory. This is project-tool configuration outside the
   Lanius capability model, so do not describe it as a package, grant, or skill
   installation.

## Milestones

### M1 -- Evidence-based `whose`

Keep the existing `code_claims` table as the evidence source. Refactor the
attribution lookup and rendering in `src/codesession.rs` / `src/codeagent.rs` so
the caller's session is considered explicitly instead of deriving "mine" after a
generic freshest-claim lookup.

- Preserve canonical path lookup and the newest active claim selection.
- Give the text and JSON outputs an explicit evidence/state shape: current viewer
  claim, other session's current claim, or unattributed/unknown. `mine` is true
  only for the first state.
- Render `yours` only for a current viewer claim. Do not use `likely yours`,
  "probably", worktree membership, or recency as a fallback.
- Keep the advisory boundary visible: a write-tool auto-claim from Claude,
  Codex, or OpenCode is useful coordination evidence, not an authoritative
  filesystem history.

**Acceptance:**

- A human-created `.codex/` directory with no claim reports
  `unattributed/unknown`, in both single-path and `--dirty` output; it is never
  labelled yours.
- A path with a present viewer claim reports `yours` and identifies the claim as
  the evidence.
- A path with only another session's present claim identifies that session as a
  current claimant but does not call the change yours.
- JSON callers can distinguish all three states without parsing human prose.

### M2 -- One claim lifecycle for every reader

Make `claim` and `unclaim` pass the same canonical path to the same room/session
key. Keep the durable CRUD in `codesession.rs`; the command layer should share a
small resolver that obtains the current session's recorded workdir and invokes
`canonicalize_claim_path` for both verbs. Do not add a second claim cache or
projection.

- Preserve idempotent behavior: unclaiming an absent canonical row is still a
  no-op with an honest message.
- On a successful release, report the normalized path that was actually removed.
- Verify the same delete is immediately reflected by the four existing readers:
  `own_claims`/`claims`, `peer_claims`/turn injection, `whose_path`, and
  `mailcli::room_claims`/`lanius code rooms`.
- Retain crash cleanup in `reap_dead_members`; it is a separate lifecycle exit
  that must use the same table semantics.
- **Phantom-claim expiry at the advertise boundary (field-evidence addition, see
  Log 2026-07-11).** `reap_dead_members` (`codesession.rs` ~1463) already deletes
  a *confirmed-dead* session's claims, but it only runs on a new `lanius code`
  launch (`codeagent.rs` ~4074) and the daemon tick (`dispatcher.rs` ~208). The
  turn-injection builder (`codeagent.rs` ~3465) reads `peer_claims` **directly,
  with no reap first**, so a crashed session's advisory claims keep getting
  injected into its roommates' turns until the next launch/tick — the observed
  30+-minute phantom-claim window for `code-d94cac68`. Call `reap_dead_members`
  once immediately before the `peer_claims` read in that injection path so a
  confirmed-dead session's claims are never advertised, even before a launch or
  tick fires. Reuse the existing `reap_dead_members` (no new table, no new cache);
  keep its split-brain safety intact — it reaps only on a confirmed same-host pid
  death and leaves a disconnected-but-alive (split-brain) session's claims alone.
  This is the smallest coherent addition; the credential/adapter defects in
  `docs/bugs/claude-code-adapter-summary-credential-crash.md` are the larger,
  out-of-scope fix.

**Acceptance:**

- In a temporary workdir, `claim src/foo.rs` followed by `unclaim src/foo.rs`
  removes the canonical stored row and says it was released.
- After that release, `claims --json` has no own entry, a roommate's next-turn
  injection has no peer entry, `code rooms --json` has no room claim, and
  `whose src/foo.rs` is unattributed/unknown.
- The same behavior holds when claim and unclaim use different equivalent
  spellings (relative, absolute, or a resolved symlink where the platform test
  permits it).
- A claim made by another live session remains untouched by this session's
  unclaim attempt.
- **Phantom-claim expiry.** Seed a room where a peer session holds claims but its
  `code_room_members.owner_pid` is a confirmed-dead pid; the very next turn
  injection for a live roommate advertises **no** peer claim from that dead
  session (it was reaped at the advertise boundary). A disconnected peer whose pid
  is still alive (split brain) keeps its claims advertised — liveness, not mere
  disconnection, is the reap trigger.

### M3 -- Conflict-tolerant dev ports by default

Invert the `shift_ports` CLI contract in `src/main.rs` and `src/dev.rs`.
`lanius dev` should choose the first free web, Vite, and isolated-bus ports;
`lanius dev --fixed-ports` should retain exact requested values. Update help and
the startup banner together so the command's behavior and printed URLs agree.

- Keep `--web-port` and `--vite-port` as the preferred starting values.
- Factor the selection decision enough to test it without launching the long-lived
  supervisor. Keep the existing bounded probe and document its TOCTOU limit; do
  not promise race-free reservation.
- When ports move, print the requested and selected web/Vite values and the
  selected dev-bus value. When they do not move, still print the resolved URLs.
- Do not alter the `target/lanius-dev` root or its production-isolation rule in
  this milestone.

**Acceptance:**

- With the requested web and Vite ports occupied, plain `lanius dev` selects a
  distinct free pair and prints the usable Vite URL.
- With 11883 occupied, plain `lanius dev` selects another dev-bus port and its
  banner still makes clear that this is not production port 1883.
- `lanius dev --fixed-ports` does not probe for substitutes; a busy requested
  listener produces the normal bind failure.
- `lanius dev --help` describes default shifting and the fixed-port opt-in; it
  no longer presents shifting as opt-in.

### M4 -- Preserve the Codex skill link

Add the single `AGENTS.md` invariant from decision 4 near the existing local
development instructions. Do not modify the link itself, create a copy of the
skills tree, or connect it to packages, profiles, grants, or runtime discovery.

**Acceptance:**

- `test -L .codex/skills` succeeds and `readlink .codex/skills` is exactly
  `../.claude/skills`.
- `AGENTS.md` names the invariant and its project-tooling boundary in one narrow
  instruction.

## Test Strategy

- Add focused Rust tests beside the existing claim tests in `src/codeagent.rs`
  and `src/codesession.rs`. Seed a temporary root with two recorded sessions and
  a real temporary workdir so normalization, viewer evidence, and peer isolation
  are tested through the same helpers as the CLI.
- Add output/JSON assertions for unknown, viewer-claimed, and peer-claimed paths.
  Include the regression where `.codex/` exists without a Lanius claim.
- Test a complete claim -> unclaim -> all-readers sequence. Check the database
  row and `peer_claims`, `own_claims`, `whose_path`, and `recent_rooms`; do not
  settle for a single command's success message.
- Unit-test extracted dev-port selection with held loopback listeners. Cover
  default shift, distinct web/Vite selection, dev-bus shifting, and fixed mode.
  A short manual smoke test may start the existing dev loop, read its banner, and
  stop it; do not turn supervisor lifecycle behavior into this sprint's test
  target.
- Run focused Rust tests, then the relevant full crate test command. Validate the
  documentation invariant with `test -L` and `readlink`; no runtime test is
  needed for the `AGENTS.md` wording itself.

## Deferrals And Boundaries

- **Dev-supervisor lifecycle investigation is out of scope.** A parallel spike
  should collect evidence about restarts, teardown, orphan processes, and shared
  dev-root behavior. Its findings can become a separately scoped milestone; this
  handoff changes only port-selection defaults and truthful logging.
- **Async `lanius code spawn` model/effort controls are separate infrastructure
  debt.** They belong to a later handoff because another live session owns
  `src/codeagent.rs`; do not mix those launch controls with ownership wording or
  claim lifecycle work.
- **Authoritative filesystem attribution is not claimed here.** The current
  harness write-tool signals are advisory. Cage/OS-level evidence remains gated
  by the read-provenance and coding-agent cage work.
- Do not edit [worker-dm-unification.md](worker-dm-unification.md), change worker
  DM behavior, or expand the web UI in this sprint.

## Log

- 2026-07-12 -- **Phase B (M1 + M2) IMPLEMENTED + VERIFIED; changes unstaged for
  Fable's commit.** Impl worker (sonnet, clean context) built M1
  (`whose_path_for_viewer` + `AttributionState{Viewer,Other,Unknown}`, three-state
  `whose_line`/`whose_json` with a JSON `state` key, `likely yours` removed) and M2
  (shared `resolve_own_claim_path` for both `claim`/`unclaim`, normalized-path
  release message, and the phantom-claim reap: `reap_dead_members` before the
  `peer_claims` read in `turn_injection`). Adversarial verifier (opus/high)
  returned **pass=true, build_ok=true, tests_ok=true**; every M1+M2 acceptance
  clause holds and the new tests assert real DB state (own/peer/whose
  before+after). The headline attack — a canonicalization mismatch mislabeling the
  viewer's OWN claim as `claimed by <itself>` — was REFUTED (`session_holds_claim`
  and the `whose_path` fallback scan the identical canon+raw pair, so a miss
  degrades to Unknown, never Other-by-self). One med test-quality defect (the new
  M2 tests mutated global `ENV_SESSION` unguarded → deterministic failure as a
  filtered subset) was fixed in one round with a test-only shared `ENV_SESSION_LOCK`
  Mutex covering all four ENV_SESSION-mutating tests. Planner re-ran the checks
  directly: the previously failing filtered subset now passes 3/3, full suite
  **620 passed / 0 failed**, clean build. Files changed (unstaged): `src/codeagent.rs`,
  `src/codesession.rs` (+ this doc). Accepted-as-scoped residuals: `whose` itself
  never reaps, so it can still cite a confirmed-dead session as a current claimant
  (M1 scopes reap to the advertise boundary only); JSON `mine` is now `false` (was
  `null`) for the unattributed case — callers should key off `attributed`/`state`;
  `turn_injection`'s reap is per-turn and cross-room (bounded, advisory). The
  credential/adapter defects in the bug doc remain the larger out-of-scope fix.
- 2026-07-11 -- **Phase B planner (Fable's planner) pickup — anchors re-verified
  post-keystone, M2 extended.** Phase A (M3+M4) is committed (`2d3eaaa`) and the
  worker-dm-unification keystone is committed (`107c332`), so `src/codeagent.rs` /
  `src/codesession.rs` / `src/main.rs` are cold; tree is clean (only `.chainlink/`
  + `.codex/` untracked). Re-checked every M1/M2 line anchor after the keystone:
  `codeagent.rs` all hold exactly — `whose_cmd` 2597, `whose_line` 2671,
  `whose_json` 2692, `claim_cmd` 2811, `canonicalize_claim_path` 2889,
  `auto_claim_write` 2915, `unclaim_cmd` 2961. `codesession.rs`: `add_claim` 1182,
  `remove_claim` 1206, `peer_claims` 1223, `whose_path` 1350 hold; only
  `own_claims` drifted to **1433** (was cited ~1230). M1/M2 acceptance clauses are
  concrete; no sharpening needed beyond the phantom addition below.
- 2026-07-11 -- **Field-evidence fold-in: phantom claims from dead sessions (M2
  extension).** `docs/bugs/claude-code-adapter-summary-credential-crash.md` records
  `code-d94cac68` crashing with 12 live edit claims still advertised to peers 30+
  minutes later. Read the code: `reap_dead_members` already deletes a
  confirmed-dead session's claims and runs on new-launch (`codeagent.rs` ~4074) +
  daemon tick (`dispatcher.rs` ~208), but the turn-injection builder
  (`codeagent.rs` ~3465) reads `peer_claims` with **no reap first**, so phantom
  claims persist until the next launch/tick. M2 previously only said "retain
  `reap_dead_members`" — it did **not** cover the advertise boundary. Extended M2
  with the smallest coherent addition: reap once before the `peer_claims`
  injection read, reusing existing `reap_dead_members` (no new table/cache),
  preserving its confirmed-pid-death-only / split-brain-safe semantics. Added a
  matching acceptance clause. The credential/adapter defects in the same bug doc
  are the larger out-of-scope fix and are NOT part of this sprint.
- 2026-07-11 -- **Sol stop state / Fable pickup.** Durable sprint state is in
  Chainlink milestone `#1`: dev-lifecycle spike `#1`, this planning task `#2`,
  and cross-harness async model/effort controls `#3`. Terra/high planner Dirac
  wrote this handoff and its `docs/handoffs/README.md` entry, then hit the Codex
  usage limit before returning a final message. No implementation or verification
  worker was launched and no sprint code was changed. Terra/high spike Sagan
  completed issue `#1`: the orderly `[dev] shutting down` log proves
  SIGINT/SIGTERM/SIGHUP, rules out watcher restarts and the scoped `pkill`, but
  cannot identify the sender; it recommends signal/process-context logging and
  persistent-terminal guidance, not a behavioral fix yet. Fable session
  `code-79985d39` is actively orchestrating worker-DM unification; treat its hot
  files as off-limits, especially `src/codeagent.rs`, `src/main.rs`, `src/web.rs`,
  the comms projection, and converse UI. `src/dev.rs` was explicitly reported
  cold. Sol's earlier trusted-host work is already committed as `5f1bc8a`; stale
  room claims on those files are evidence for M2, not pending edits.

- 2026-07-10 -- Grounded the plan in the live code. `whose_line` currently falls
  back to `unattributed (no session claims it -- likely yours)`, which explains a
  human-created `.codex/` being misreported. `code_claims` is the only current
  evidence source; `whose_path` chooses its freshest row and the renderer adds
  `mine` afterwards.
- 2026-07-10 -- Confirmed the claim contradiction: `claim_cmd` stores a canonical
  workdir-relative path; `unclaim_cmd` deletes the raw argument. The direct
  readers are `claims`, peer injection, `whose`, and `code rooms`, so a failed
  delete naturally leaves all of them showing the old row.
- 2026-07-10 -- Confirmed `dev::run` already supports port shifting, but only when
  `--shift-ports` is passed. It applies to the web relay, Vite, and the dedicated
  development broker. The proposed change is an inversion with a fixed-port
  escape hatch, not a new dev topology.
- 2026-07-10 -- Verified `.codex/skills -> ../.claude/skills`. The requested
  invariant is documented as local project tooling, not a Lanius capability.

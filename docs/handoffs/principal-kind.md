---
status: done
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-08
---
# Handoff A: a principal's kind is a stored field, not a name prefix

Today the system decides *what kind of principal this is* — a grant-scoped
coding worker vs. a full-authority identity (owner / kernel / human) — by looking
at the **spelling of its name**: `name.starts_with("code-")`. That test sits in
the security core: it is the branch at `src/broker.rs:440` that chooses, at
CONNECT, whether every bus ACL gate runs (`actor = Some`, scoped) or is skipped
(`actor = None`, owner-equivalent). The same guess is then re-derived,
independently, in two more places that render the UI.

This handoff replaces the magic prefix with an explicit **`kind`** carried on the
principal's stored record, and switches every classifier to read that field
(falling back to the prefix only for legacy rows, so nothing breaks). It is the
capability-from-a-stored-attribute rule the audit keeps asking for: authority
must come from what the kernel recorded about a principal, never from how its
name happens to be spelled.

## Why this matters (and the nuance that keeps it honest)

`elanus` already learned this lesson the expensive way. **security.md entry 20**:
the first coding-session credential was a plain fenced secret named
`code-<session>`, the broker resolved it as full authority, and *no ACL gate
ran* — a worker token could publish to the owner's mailbox and read every
agent's telemetry. The fix put the session token in a fenced sub-store and made
the broker resolve `code-*` **before** the fenced-secret path as a grant-scoped
actor. That fix is correct and stays. But it wired the whole distinction to a
string prefix, and then two UI projections copied the same string test. Three
independent guesses at one fact is a bug waiting to drift.

**The nuance — do not paper over it.** The broker's *real* authority boundary is
**which fenced store holds the credential**, not the name:

- a credential in `.secrets/code-sessions/<name>.json` → grant-scoped session
  (only the uncaged launcher can place it there);
- a credential in `.secrets/<name>` → full authority (owner / kernel / human;
  only the human/kernel can place it there — the cage fences the store);
- a supervisor-minted token in the in-memory actors map → grant-scoped package.

The store placement is the ground truth. `kind` must **agree with** the store
the credential resolves in — it is a *label on* that ground truth so downstream
readers (and humans) don't have to re-derive it, not a second, competing source
of authority. The migration below derives `kind` *from* the store/prefix so the
two can never disagree.

## Decisions to confirm / wonky bits

1. **`kind` is a label, authority stays store-driven.** The broker keeps
   resolving authority by store placement (session-store → scoped, fenced-secret
   → full, actors-map → package). `kind` is set *from* that resolution and
   carried alongside `(actor, sender)`. We do **not** invert it (read `kind` to
   decide the store). If you ever find code deciding *authority* from `kind`
   alone, that is the bug this handoff is trying not to create. **Recommend: yes,
   keep authority store-driven; `kind` is descriptive.**

2. **The value set.** `session | human | kernel | package`. `owner` is a `human`
   (the default human). Confirm we don't need to distinguish `owner` from other
   humans *at this layer* — the broker already treats every fenced-secret
   principal identically (`actor = None`); "which human" is the *name*, `kind` is
   the class. **Recommend: four values; "owner-ness" stays a name/config fact,
   not a kind.**

3. **Where `kind` lives for the UI projections.** The broker sees live
   credentials; the web/mailcli projections see only **ledger event rows** and a
   session-id string — they never open a token file. Two honest options:
   - **(a)** materialize `kind` where a durable record already exists: the
     `SessionToken` JSON (for the broker/launcher) and the `code_sessions` table
     (`src/db.rs:384`, which every coding run already rows into). The projections
     consult `code_sessions` for an authoritative `kind`, prefix-fallback for
     pre-migration rows. No ledger schema change.
   - **(b)** stamp `principal_kind` onto every event at emit time from the
     verified connection, so a projection reads it straight off the row. More
     invasive (touches the hot emit path and every event) and redundant with the
     verified `sender` already on the row.
   **Recommend (a):** the record already exists, it is a read-time join, and it
   keeps the ledger shape stable (back-compat). (b) is a bigger blast radius for
   no extra truth.

4. **Prefix stays as the fallback, forever-ish.** Legacy tokens and pre-migration
   `code_sessions` rows have no `kind`. `is_session_principal` keeps its current
   prefix+valid-principal test as the fallback when `kind` is absent. This is the
   same back-compat discipline `Grants` already uses (`#[serde(default)]` →
   `None` on old tokens, `src/codesession.rs:1722`). We are *adding* a
   faster/clearer signal, not removing the old one.

5. **Do not touch the internal `is_session_principal` guards that are really
   store-routing.** Many `is_session_principal` calls inside `codesession.rs`
   (mint, read, retire, budget/room ops — e.g. `:2161`, `:2392`, `:893`) ask "is
   this a name I own a token file for?", which is legitimately a name test (the
   name *is* the file stem). Those stay. This handoff changes the **authority
   classifier** (broker CONNECT) and the two **UI worker classifiers**, not the
   store-routing predicate.

## The three classifiers to switch (and what each really asks)

Grounded in the worktree:

| site | file:line | question it asks | after this handoff |
|---|---|---|---|
| CONNECT trust anchor | `src/broker.rs:440` | grant-scoped vs full authority | resolve by store as today, then set `kind` from the store it matched; carry `kind` next to `(actor, sender)` |
| comms-list eviction | `src/web.rs:2381` `is_worker_session` (used `:2544`, `:2604`) | "is this a coding run, drop it from the human comms list" | read `kind == session` from `code_sessions`, prefix-fallback |
| session-mail filter | `src/mailcli.rs:131` | "only thread mail addressed to a coding session" | same: `kind == session` via `code_sessions`, prefix-fallback |

`src/code_projection.rs:143` also prefix-tests, but only *after* it has already
matched the coding-noun obs subtree (`obs/agent/{codex,claude-code}/...`,
`:139`) — it is a shape check on an already-coding topic, not an authority
decision. Fold it in for consistency (read `kind`, prefix-fallback) but it is not
load-bearing; call it out and let the implementer decide.

## Milestones

### M1 — add `kind` to the stored records, derived on write

Add a `kind` field to `SessionToken` (`src/codesession.rs:1956`), serialized
`#[serde(default)]` so **every existing token on disk deserializes byte-for-byte**
(the same invariant `Grants` documents at `:1722`). Default, when absent, derives
from the prefix (`session`). Add a `kind` column to `code_sessions`
(`src/db.rs:384`) with a default that back-fills existing rows as `session` (they
are all coding runs by construction). `mint` (`:2153`) stamps `kind = session`
explicitly going forward.

- **Acceptance:** a `SessionToken` JSON written before this change (no `kind`
  key) round-trips through `read` (`:2392`) and reports `kind == session`; a
  freshly minted token has `"kind":"session"` on disk; `cargo test` in
  `codesession` green including a new "legacy token has no kind key, resolves as
  session" case next to the existing `is_session_principal` tests (`:2535`).

### M2 — broker computes `kind` from the store it resolved, not the name

At `src/broker.rs:440`, keep the resolution order exactly as entry 20 left it
(session-store first, then fenced secret, then actors map). Set a `kind` value
from the branch that matched: session-store → `session`, fenced-secret →
`human`/`kernel` (by name/config — kernel is the kernel's own principal name,
everything else fenced is `human`), actors map → `package`. Carry `kind`
alongside `(actor, sender)` so any later code (and the ledger `sender`
attribution) can see the class without re-guessing.

- **Acceptance:** an existing e2e that connects a `code-*` session and asserts
  its scoped ACL (the entry-20 regression: worker denied on `in/human/owner`,
  `obs/#`) still passes, and the resolved principal now reports `kind ==
  session`; a fenced-secret owner connects and reports `kind == human` with
  `actor == None` (authority unchanged). No behavior change — the *branch taken*
  is identical to today; only the label is new.

### M3 — the two UI classifiers read `kind`, prefix as fallback

Replace `web.rs:2381 is_worker_session` and `mailcli.rs:131`'s
`starts_with("code-")` with a shared helper that: looks up the session in
`code_sessions`, returns `kind == session` if present, and falls back to the
prefix+valid-principal test when the row (or its `kind`) is absent. Put the
helper where both can call it (e.g. next to `is_session_principal` in
`codesession.rs`, taking a `&Connection`), so there is **one** definition of
"is this a worker," not three copies.

- **Acceptance:** the existing web comms-list e2e (a coding run is evicted, a
  curated non-`code-*` conversation is preserved — `chat-rendering.md` M2, cited
  in the `web.rs:2372` comment) still passes; add a case where a `code_sessions`
  row with `kind = session` but a **non-`code-` id** (a forward-looking id shape)
  is correctly evicted — proving the decision now comes from the field, not the
  spelling; and a legacy row with no `kind` and a `code-` id is still evicted via
  fallback.

### M4 (optional, fold if cheap) — `code_projection.rs` reads `kind` too

Switch `src/code_projection.rs:143` to the same helper for consistency. Purely
cosmetic (it is already inside a coding-noun-gated path); skip if it adds a
`&Connection` plumb that isn't otherwise there.

- **Acceptance:** `code_projection` tests unchanged and green.

## Read these first

- `src/broker.rs:411-470` — the CONNECT identity resolution (the trust anchor).
- `src/codesession.rs:1678-1990` — `PREFIX`, `Grants` (the back-compat
  `#[serde(default)]` template), `SessionToken`, `is_session_principal`, `mint`,
  `read`.
- `src/web.rs:2372-2422` — `is_worker_session` + the comment explaining why the
  decision must be derivable from ledger shape (a third-party UI reproduces it).
- `src/mailcli.rs:123-160` — the session-mail filter.
- `src/db.rs:384-394` — `code_sessions`.
- `docs/security.md` entry 20 (the credential-authority fix this builds on),
  entry 16 (the attribution half), and the identity model (`docs/identity.md`).

## Residuals / gating

- **No new authority; label only.** This handoff deliberately changes *nothing*
  about who may do what. If a reviewer wants `kind` to *gate* anything, that is a
  separate decision — flag it, don't sneak it in here.
- **`kernel` vs `human` disambiguation** at the fenced-secret branch relies on
  the kernel's principal name being known to the broker. If that name is not
  currently distinguishable from a human's, ship M2 collapsing both to a single
  "full-authority" label with a `TODO` and split later — the authority is
  identical either way, so the UI can live without the split for now.
- Handoffs B and C build on the *concept* here but do not block on it; B (the
  `dm` grammar) is independent, and C's projection cleanup is cleaner once M3
  lands but does not require it.

## Log

- 2026-07-08 — planner drafted from the worktree. Confirmed: `topic.rs` does not
  validate categories, `is_session_principal` is the sole authority classifier at
  CONNECT, and the two UI copies are `web.rs:2381` / `mailcli.rs:131`.
</content>
</invoke>

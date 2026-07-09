# Configuration

Decided 2026-06-15. This describes the direction we are moving toward. It is
not yet how the code is organized; the migration state and the parts still
open live in HANDOFF.md. It builds on docs/layering.md (the interface is a
product with its own language), docs/actors.md (everything is an actor), and
docs/identity.md (the broker is the only thing that says who sent a message).

## Two things, and one process

There are only two kinds of thing in this part of the system, and one thing
that happens to them.

The two things are **packages** and **configuration**. A package is the unit
you install. Configuration is the set of choices a package — or an agent —
needs before it can do its job. That is the whole inventory. There is no
separate category of "service" or "feeder" sitting beside packages; those are
just packages doing what packages do (see "Skills and actors are roles", in
docs/actors.md).

The one thing that happens is **accepting a change**. Installing a package,
turning one on, editing a setting — these are all the same event: a change to
configuration that someone has to accept. The change can be made by a person
or proposed by an agent. Either way it goes through the same acceptance step,
and the system records who accepted what. "A proposal" is therefore not a
third kind of thing. It is just the agent being the one who starts the
accept-a-change process instead of a person.

## What configuration is

An agent has configuration: which model it thinks with, how many run steps one
activation may take before yielding, which context program builds its provider
request, and which packages it is allowed to see. That kind already has a home
in the interface today — it is the "configure" view, backed by the agent's
profile. The current storage key for the run-step budget is still
`model.max_turns`; that is a compatibility name, not the product model.

A package has configuration too, and this is the part with no home today:
which accounts a watcher should follow, how often it should poll, whose inbox
it should deliver into. Installing such a package is not enough to make it
useful; it cannot do anything until it has been told how to behave. Treating
package configuration as a first-class thing — something you can see, set, and
hand to an agent to propose changes against — is the gap this document closes.

Configuration is kept as **files**. The agent's profile is already a file;
package configuration becomes files in the same spirit. Keeping configuration
as files is what lets the rest of this design work, because files are
something both a person and an agent already know how to edit, and something
the system already knows how to fence, watch, and diff (docs/sandbox.md: the
cage restricts where writes may land, and the camera records what changed).

## A change is a proposal until it is accepted

An agent never writes live configuration directly. It proposes. Its edits land
somewhere that is not yet in effect, the system turns them into a plain diff,
and that diff is what a person (or a rule, below) accepts or declines.
Acceptance is the act that makes the change real.

We deliberately do not give the agent a special "propose a configuration
change" tool. A tool would be more machinery for no gain: the agent is already
fluent at editing files, so it edits the configuration files the way it edits
anything else, and the system observes the result. The proposal is the diff,
not a message in a bespoke protocol.

## Proposals are Git branches

The mechanism for "held aside until accepted" is Git. This is not a different
model from the one above; it is the most natural way to implement it, and it
happens to dissolve two problems we would otherwise have to solve by hand.

The mapping is the pull-request workflow:

- The **live** configuration is a branch the kernel owns — `live`. Actors read
  the checked-out files exactly as they read any config today.
- A **proposal** — a person's or an agent's — is a branch, `proposal/<id>`.
- The **diff** the interface shows is the difference between the proposal
  branch and `live`.
- **Acceptance** is a merge of the proposal into `live`. Whether that merge
  happens immediately or waits for a person is the autonomy question below.
- The **history** of accepted changes is the commit history: what changed,
  against what parent state, and — paired with the ledger — by whom.

Two problems this dissolves:

- **Platform differences stop mattering.** The other way to hold a change
  aside is a layered filesystem, and a true layered mount is only clean on
  Linux (OverlayFS); macOS and Windows would need heavyweight virtualization.
  A Git branch is the layer, and it behaves identically everywhere.
- **Silent overwrites become impossible.** When a person and an agent both
  change configuration, Git produces a visible conflict rather than letting one
  silently clobber the other. (That clobber is a real bug we have already hit
  in the configure form; making it structurally impossible is worth a lot.)

It also sits on the project's grain — lanius is already a "git hooks and
sqlite" kind of system — and a Git history is a content-addressed hash chain,
which is exactly the tamper-evident provenance trail we have wanted for
configuration anyway.

## Keeping the agent fenced

"Sandbox the .git and the branch pointers" is the right instinct, but the
robust way to do it is by **isolation, not by fencing parts of one repository**.
Fencing pieces of a single `.git` is brittle: Git must write the index, the
object store, and the agent's own branch refs just to let it commit at all, and
threading "may commit, may not move `live`" through ref permissions is fragile.

So the agent works in its **own clone**, inside its cage. It may do anything to
that clone — move its branches, rewrite its history — because the clone is
disposable. The **live** repository lives in kernel-only territory, the same
fenced area that already holds the secret store and the ledger and that the
cage already keeps agents out of (docs/sandbox.md). The kernel fetches just the
named proposal branch back, reads the diff, gates it, and merges. The agent
never touches the live `.git`.

Three sharp edges inside that boundary:

- **Git hooks are code execution.** A repository's hooks run arbitrary code on
  commit, checkout, and merge. The kernel must run every Git operation with
  hooks and content filters disabled, and treat the agent's clone as untrusted
  input — reading its commits and diffs, never executing its machinery.
- **A commit's author is not provenance.** An agent can set the author name to
  anything. Git's author field is decoration. The trustworthy answer to "who
  did this" is the broker-stamped sender on the acceptance event in the ledger
  (docs/identity.md) — an identity the agent cannot forge.
- **Two records, composed not duplicated.** Git holds the content and the diff;
  the ledger holds the acceptance event — the broker-verified identity and the
  autonomy rule that allowed it — and references the commit. One pointer between
  them, no overlap. This also answers "where did this come from" cleanly: the
  commit that introduced it, accepted by that identity.

## Autonomy: how much you have to confirm

How much confirmation an agent's proposal needs is the person's comfort to set,
not ours to fix. There are roughly **three levels of autonomy**, and the
interface shows the rules in plain language so they are easy to understand and
change. In every level the agent only ever *proposes*; the levels differ only
in what gets accepted without asking you.

The rules judge the **diff**. A rule can be deterministic ("a change that only
touches which accounts are watched") and may be backed by a classifier for the
fuzzier cases; either way the unit it looks at is a proposed diff. A change that
only adjusts a watched-accounts list might auto-accept at a middle autonomy
level, while a change that installs a new package or alters the cage always
stops for a person. The point is that the same proposal mechanism carries all
of it; autonomy is a policy layered on top, not a different code path.

## Stdlib: the configuration that is always there

Some packages are not optional — the product itself depends on them. The
transcript view (`history`) is the first example: the interface reads it, it
needs no configuration, and until recently it could not be installed at all.

These live in a **stdlib kit**: always installed, and protected against
removal. Trying to uninstall it sets off a loud alarm and refuses by default,
the way a shell refuses to delete the root of the filesystem without a fight.
This is, as the saying went when we decided it, "just permissions" — a
protected flag on the kit and a guard on the removal path — and needs no change
to the model above. It also settles the "should the product depend on something
a person has to discover and turn on" tension: product-critical packages are in
stdlib, present, and protected; everything else is opt-in.

## What the interface shows

Following docs/layering.md, the configuration surface is three things and no
internal vocabulary: **what you can add** (a catalog of packages), **what is
installed and on**, and acceptance woven in as a single quiet confirmation —
not a separate, intimidating queue, and never the words "stage", "grant", or
"pending". Package configuration gets a home right next to the package it
belongs to, so setting up a watcher has somewhere to ask "which accounts?".
When an agent is the one proposing, the person sees a plain sentence — "the
scout agent would like to start watching @example" — and the diff behind it if
they want it, not a technical approval to triage.

## Profile fields: how an agent is presented (`[ui]`)

Most profile fields decide what an agent *is* — its model, its sandbox, its
autonomy. One small table decides only how it is **presented** in the web UI,
and it stays deliberately generic so presentation policy never becomes a
per-name special case in the kernel (the `is_worker_session` anti-pattern the
simple-core doctrine forbids — a magic string prefix that leaks product policy
into core).

```toml
[ui]
# Where this profile appears. "panel" = presented ONLY in a dedicated surface
# (the assistant side-panel) and hidden from the left-hand agent list. Absent
# or any other value = an ordinary agent, listed like the rest.
surface = "panel"
```

The `helper` profile ships with `surface = "panel"`: it is the one agent that
lives entirely in the assistant panel, so it must not also appear as a row in
the agent list. The list filters on **this property**, not on the name
"helper" — so any future panel-only profile is hidden for free, and nothing in
the interface matches a hard-coded agent name.

## Still open

- **Repository layout.** One configuration repository with subtrees for the
  agent and for each package, or a repository per scope. Leaning toward one,
  for a single coherent history.
- **Conflict policy.** Git surfaces a conflict when a person and an agent edit
  the same configuration; the interface needs a plain way to resolve it. Rare,
  but it needs an answer.
- **Reload.** How an actor learns its configuration changed after a merge, and
  whether that is a restart or a live signal.
- **The exact three autonomy levels** — their names and precise boundaries.

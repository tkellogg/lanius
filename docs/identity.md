# Identity

Design, 2026-06-13. This is the missing center of the security model. It is
not built yet — it is the plan. It does not replace docs/security.md; it
gives the three deferred "legs" recorded there a single purpose and a shape,
and it is the thing the actor direction (docs/actors.md) has been waiting on.

## Why identity is genuinely hard here

Every actor on one machine runs as the same operating-system user. The agent
thinking with a language model, the script watching a folder, the command
line you type into, and the web interface are, as far as the operating
system is concerned, all the same person. So the usual trick — ask the
operating system who a process belongs to — gives every actor the same
answer and tells us nothing. We cannot lean on the machine to tell our
actors apart, because to the machine they are one user.

There is a second, deeper problem that has to be named or the rest is
theater. The place where we record who is allowed to do what — the approvals
ledger — currently lives inside the same folder tree that actors can write
to. An actor that wanted more authority does not need to defeat any check;
it can edit the ledger directly and grant itself whatever it likes. So today
authority is really decided by *being local code*, and every actor is local
code standing exactly where the human stands.

That is the actual problem. It is not "we have no passwords." It is that
locality is currently the same thing as authority.

## The shape of the answer

Identity rests on one trusted thing and one rule.

The trusted thing is the kernel, which includes the message broker. If the
kernel is compromised the game is over, and that is acceptable — it is the
one part we decide to trust completely, and we keep it small for exactly
that reason.

The rule is this: **the broker is the only thing that says who sent a
message, and the ledger can only change by going through the broker.** If
both halves hold, then trusting the kernel is enough to trust every identity
in the system, and there is no way to sidestep the check, because there is
no second door into authority.

In concrete pieces:

1. **Every actor carries the identity its launcher gave it.** The important
   refinement (Tim, 2026-06-13): a thing the kernel *launches* does not have
   to authenticate from scratch, because the launcher already knows who it
   started. The kernel mints a per-spawn secret for the actor when it
   launches it — this already happens for background package processes — and
   the actor presents that secret once, when it connects; the broker maps it
   back to the identity the kernel assigned, for the life of the connection.
   The secret does not ride on every message; the connection carries the
   established identity. This means almost nothing has to "log in" — the
   kernel vouches for everything it starts.

   The exceptions are the entry points a *person* starts by hand. The
   human's command line is the clearest: it is not launched by the kernel,
   so it reads the human's credential from the fenced store (a secret the
   kernel wrote that agents cannot reach) and presents that. The web
   interface should move to being kernel-launched — a supervised service the
   kernel brings up — so that it, too, gets a vouched identity automatically
   instead of being hand-started; that is both safer (the kernel knows it
   started it) and simpler to run day to day. The person then authenticates
   to that interface separately (see "the human is the hard case").

2. **The broker stamps every forwarded message with the sender it verified,
   and it ignores any sender a message claims for itself.** A subscriber
   never sees anyone's secret; it sees a sender field the broker vouches for
   — "this came from the scout, I checked." The key word is *ignores*: the
   broker overwrites whatever sender a message tried to claim, so an actor
   cannot forge a different one. (elanus already does exactly this shape of
   thing with the correlation field on inbound messages, so the machinery is
   familiar.)

3. **No credential means no authority, not "the human."** Today, connecting
   with no credentials is treated as full human authority. That has to
   invert: present nothing, get nothing.

4. **Only the kernel writes the ledger.** Actors lose direct write access to
   the approvals ledger and the database file that holds it. When an actor
   wants to change what it is allowed to do, it asks over the bus, where it
   is authenticated, and the kernel records the request. This is the half
   that makes the broker's identity check actually load-bearing. Without it,
   perfect identity on the bus is a locked front door standing next to an
   open window.

5. **No actor can read another actor's secret, or the human's.** Secrets,
   and the configuration that confers authority, live where each actor's
   sandbox cannot reach them. A stolen secret is a stolen identity, so this
   is not a nicety — it is part of the definition working at all.

These five are not five features. They are one property — *you are who the
broker says you are, and there is no way around the broker* — held across
three surfaces: the broker, the ledger, and the sandbox. They are also,
exactly, the three deferred legs in docs/security.md. Identity is not a new
project bolted on beside those legs; it is the name for finishing them, and
the reason to.

## Why minted secrets, and not something lighter

It is worth saying why we cannot use something simpler than a kernel-minted
secret. Because every actor is the same operating-system user, the broker
cannot ask the operating system "who is connecting" and get a useful answer
— peer-credential checks would say "the user" for all of them. And a token
sitting in a file is only as private as the sandbox around it. So the secret
has to be something the kernel hands out per actor, and the sandbox has to
keep each actor away from every other actor's copy. The two mechanisms are a
matched pair; neither works alone.

## The ledger is SQL, and protecting it has two layers

When we say "the ledger," the thing that has to be protected is the approvals
table in the database — SQL rows, not a configuration file. A file-based
ledger may exist too, downstream, and it benefits from exactly the same
protection, but the authoritative thing is the SQL. There are two ways to
keep an actor from granting itself authority by writing those rows, and they
stack.

The first layer is to stop the writing outright: an actor's sandbox denies it
access to the database, the kernel is the only thing that writes it, and the
only way an actor can ask for more authority is over the authenticated bus.
This is the primary defense, and it is appealing precisely because it needs
no secret at all — it rests on the sandbox, which we already depend on, and
on the kernel being the sole writer.

The second layer makes tampering *detectable* even if the first is somehow
bypassed: each authoritative row carries a hash keyed by a secret only the
kernel holds, so a row an actor wrote by hand has no valid hash and the
kernel ignores it. This is the salt-and-hash idea, and it is a real
strengthening — with two honest cautions. It only works if that key stays
secret for as long as the ledger lives, which is exactly the persistent-
secret problem below. And a naive per-row hash catches forged and modified
rows but not deleted ones, nor an old, still-valid row replayed after it was
revoked; doing it properly means chaining the rows, each row's hash folding
in the one before it, so the kernel can check the whole chain against a single
head value it keeps. Worth doing, but it is a small tamper-evident log, not a
one-line hash.

Recommendation: build the first layer now, because it is robust and needs no
persistent kernel secret. Treat the keyed-hash chain as defense in depth that
becomes available once we have somewhere to keep a secret, and build it as a
proper chain when we do.

## Persistent secrets: the foundation, and what actually needs them

Where secrets live is the deepest thing in this design, so it is worth being
precise about which secrets even need to persist — the answer is narrower
than it first looks.

A non-human actor's connection secret does not need to outlive the broker. We
can mint it fresh every time we launch the actor, because the actor's
identity is its stable *name*, not a long-lived secret; a restart just hands
out new secrets. That is already how the background actors work, and it means
the everyday case needs no persistent storage at all.

Two things genuinely do need to persist. The first is the human's
authentication — you should not have to re-enroll every time the daemon
restarts. The second is the keyed-hash chain's key, if we build that second
ledger layer, because the ledger outlives any single run and the key has to
match across runs. So persistent secret storage is required for human
authentication and for the optional ledger hardening, and not for routine
actor identity. There is a quiet win hiding in that: if we take the
physical-fencing layer for the ledger and skip the keyed hash for now, the
kernel needs no persistent secret of its own at all, and the only persistent
secret in the whole system is the human's — which the operating system is
already built to hold.

For the human, that persistent, agent-proof store already exists: a passkey.
Its private half never leaves a secure enclave, an agent on the machine
cannot extract it, and it cannot be used without the person's own gesture —
which makes it the one secret in the system that survives even an agent
breaking out of its sandbox. (Practical caution: browser support is uneven,
and Firefox in particular handles passkeys poorly, so the implementation has
to degrade gracefully rather than assume them.)

For anything on the kernel side that does end up needing to persist, the
baseline store is a location the kernel owns and every actor's sandbox is
fenced away from. It depends on the sandbox holding, which is the same
dependency as the rest of this design, so it adds no new article of faith.
The operating system's keychain is a possible later hardening, with the
caveat that on a single-user machine it tends to hand any process running as
that user the same access, so it is not automatically stronger than a
sandbox-fenced file.

## The human is the hard case

A non-human actor is the easy half: the kernel made it, the kernel gave it a
secret, and the kernel can keep that secret out of everyone else's reach.

The human is harder for one specific reason: **agents can reach the same
surfaces the human uses.** An agent with a shell can talk to the web
interface's local port directly, without ever opening a browser. So
protecting the human is not really about the browser at all — storing
something in browser memory does not help, because the agent can go around
the browser and speak to the server itself. The real task is to make sure
the thing that proves "a real person did this" is something an agent cannot
obtain or replay.

Human proof is a configurable spectrum, not one mandatory mechanism, because
most people do not want a heavy lock on their own door — and for many, none
at all is the right setting. The mechanism is pluggable; an installation
picks where on the spectrum it sits:

- **None.** Trust the local human implicitly. This is a first-class, common
  choice, not a shameful fallback — on a single-user machine the person may
  simply not care, and the threat it accepts (an agent could, in principle,
  act as the human on that box) is one they are entitled to accept. It
  should be honestly labeled, but it is a perfectly good default for a lot of
  people.

- **A light out-of-band tap.** When an action wants confirmation, send a
  desktop notification — "elanus: approve installing X?" with a yes — to the
  person's logged-in session. This is out-of-band in the way that matters
  here: it lands in the human's interactive desktop, where a headless agent
  is not, so the agent cannot answer it without driving the GUI (which the
  sandbox should deny). It is cheap, pleasant, and good enough for most
  people who want *some* check. (Desktop notifications are easy on macOS;
  other platforms vary, so this degrades to the next option or to none.)

- **A passkey.** The strong case: a gesture whose secret never leaves a
  secure enclave and that answers a fresh challenge each time, so no amount
  of reading files on the machine reproduces it. For the security-conscious,
  or for the highest-stakes actions, this is the only thing that survives an
  agent breaking out of its sandbox.

Underneath all three, the credential a surface holds to act for the human
still lives where agents cannot read it — that is what keeps "none" from
being worse than it has to be, and what every surface relies on. The point
of the spectrum is that the *extra* gesture on top is the human's choice,
from nothing to a hardware key, and the design must not hardcode one rung.

We do not need to build the passkey rung first. None and the notification tap
are the easy, high-value paths; passkeys can follow for the people who want
them.

## "On behalf of," stated plainly

The web server and the command line are themselves actors. When they act for
the human they are not the human; they are trusted surfaces carrying the
human's authority. That distinction earns its keep: it means each surface's
own credential is a high-value target — whoever holds it can act as the
human — so it must be guarded as carefully as the human's own, and the most
sensitive actions should still demand the out-of-band gesture rather than
resting on a surface's stored credential. This is the honest reading of the
"web interface acts on behalf of the human" idea: the delegation is right;
the security lives in where the root credential sits and what the
high-stakes actions additionally require, not in which storage slot the
delegated token rides in.

## Delegation: authority is a subset of the spawner

Identity above settles *who you are*. It does not, by itself, settle *what you
may do relative to who started you* — and that is a second rule, just as load-
bearing. Stated plainly (Tim, 2026-06-20):

**A thing the kernel launches gets authority that is a strict subset (≤) of
whoever launched it — reconstructed and re-authenticated by the harness at spawn,
never blindly inherited.** `child.grants ⊆ parent.grants`, asserted at mint, so no
descendant can out-authorize an ancestor. Authority narrows *monotonically* down a
spawn chain.

This completes the launcher-vouched idea (point 1 above). The launcher already
vouches for *who* a child is — it minted the child's secret, so it knows. The same
launcher is therefore the right place to bound *what* the child may do: it can only
hand down authority it holds, and it hands down a slice ≤ its own. "On behalf of"
is the human→surface case of this; spawn delegation is the general case.

Two flavors, and the distinction matters:

- **Capability dimensions are subsetted.** Bus topic patterns, fs roots, tool
  allowlist: the child gets a subset; siblings may overlap. This is exactly the
  `lease ⊆ grant` rule docs/sandbox.md already enforces for filesystem writes —
  generalized to every dimension and across the spawn boundary, using the same
  "decidable, boring function" (canonicalized prefixes, not glob soup).
- **Budget dimensions are partitioned.** Turn/cost budget, wall-clock, spawn
  fan-out: the child gets an allocation carved from the parent's *remaining*, and
  siblings partition it (`Σ children ≤ parent`). Cutting a child's budget to a half
  or a quarter as a way of passing context down (an RLM-style sub-call) is this
  flavor — divisible authority split at a spawn, not a subset.

The everyday default may be *equal* — a sibling session the human spawns directly
often gets the human's own broad slice, which is the equal case of `⊆`. That is
fine. The invariant is `⊆`, not equality, and the moment one actor spawns another,
the subset rule (not "they're peers") is what holds. This is the reconciliation of
the "homogeneous authority among the user's own agents" language in the coding-agent
handoffs: homogeneous is the *default equal case*, not a competing model — see
docs/security.md entry 22 and docs/handoffs/authority-delegation.md for the contract
and its (not-yet-built) enforcement. Today the mechanism is half-present: a spawned
worker is re-minted rather than inherited (the launch wrapper scrubs the parent's
token), but the minted scope is a flat per-kind constant (entry 20's structural
code-session scope), not yet a function of — or bounded by — the spawner's grants.

## Why this also settles the agent-versus-package question

Once the broker stamps a verified sender on every change, the question that
was blocking the actor unification answers itself. A change to an agent's
definition that arrives carrying a human's verified identity is trusted and
takes effect immediately. The same change carrying an agent's identity
re-enters review. We do not have to fingerprint and re-approve an agent's
configuration the way we do a package's code; we need to know who made the
change, and a verified identity is exactly that knowledge.

For configuration that is edited as plain files rather than sent as messages
— a profile on disk, for example — the equivalent guarantee comes from the
sandbox rather than the broker: the human's configuration lives where agents
cannot write it, so a file that changed was changed by the human. Same
principle, enforced by the other surface.

## Where identity has to live

Identity is not a module you add in one corner. It is a property that has to
hold in five places at once, which is why it has felt large:

- **The broker** authenticates each connection and stamps the verified
  sender, overwriting any claimed one.
- **Credential issuance and storage**: the kernel mints per-actor secrets;
  they are stored where only that actor and the kernel can read them.
- **The ledger** accepts writes only from the kernel, so the bus is the one
  and only path to changing authority.
- **The sandbox** fences each actor away from the secrets, the
  configuration, and the ledger that it is not entitled to read or write —
  including the human's.
- **The interface** carries the human's delegated authority and asks for an
  out-of-band gesture on the actions where being a real person is the point.

## What an identity is: a name, not a role

A correction to how the first increment shipped, decided with Tim 2026-06-13.
The credential work above authenticates a *principal* — and it used the word
"human" as that principal. That was a role wearing an identity's clothes.
"human" is a *kind* of actor, not a *who*. The principal is an identity with a
name — `owner`, or `tim`, or `alice` — and "human" survives as an attribute of
it (its kind), not as its name.

This is the same point the actor model already makes (docs/actors.md): a human
is an actor like any other, named like any other. An agent's principal is
`kestrel`, not "agent"; a person's principal should be `tim`, not "human". The
mechanism already allows it — the secret store is keyed by name, and the broker
matches a presented name against `.secrets/<name>` — so making the principal an
identity is mostly letting the rest of the system catch up to where the topic
grammar already pointed (the human mailbox has always had an `<owner>` slot).

The default owner identity is named `owner` (it is the first identity, and on a
fresh single-person install that reads right). It is only a default: the system
should nudge the person to set their real name, after which they are `tim`
everywhere. Multiple humans fall straight out of this — they are just more
named identities of kind human — but provisioning and managing several of them
waits on a stronger human-authentication story (the spectrum above); the
*model* is multi-ready now, the *management* is deferred.

## Identities, channels, and names

An identity is **identifiable but not singularly addressable.** A person is one
stable identity reachable many ways — the elanus interface, a phone, Bluesky, a
front door — and called by many names. Agents have this shape too. So an
identity is not an address; it sits above its addresses. Three pieces:

- **Identity** — the stable entity. `tim`. Has a kind (human / agent / script /
  external) and a canonical display name. The *who*. An identity's
  authenticated elanus principal (the credential work above) is simply one of
  its channels: the *elanus channel*.
- **Channel** — an addressable endpoint, a `(kind, address)` pair: the elanus
  channel, `(bluesky, @handle)`, `(discord, id)`, `(sms, +1…)`, `(email, …)`.
  Many per identity. The *where to reach them*.
- **Name / alias** — a name others use for the identity. Many per identity, and
  **not unique** — two people can both be "Sam", and one person can be "Tim",
  "tk", and "dad". A name is a label, never an address.

Two confidences live here and must not be confused. *Did this message really
come from this channel* is channel authentication — for the elanus channel that
is exactly the broker-verified sender above; for an external channel it is only
as strong as the bridge that carried it. *Is this channel really this identity*
is a separate, fuzzier judgement — the linkage — and it is the heart of the
phonebook.

## The phonebook

The phonebook is the record of which channels belong to which identity. It is
**SQL, and shared** (Tim's call): one agent that works out a Bluesky handle and
a Discord handle are the same person writes that down once, and the whole fleet
sees it. This is the master-data / record-linkage problem that fintech's
three-way match and healthcare patient-matching also face (the formal name is
probabilistic record linkage, Fellegi–Sunter; the everyday shape is a vCard —
one card, many numbers and emails; the modern identity shape is a DID or
ActivityPub actor's `alsoKnownAs` — one subject, many endpoints). **We do not
set out to solve the matching. We ship the data model that makes it solvable**,
and leave the matching policy to agents, to people, and to later work. A
readable `phonebook.md` for an agent to consult is then just a rendered view of
the table; the SQL is the truth, so things other than the agent can use it too.

Four properties keep the hard part open instead of frozen:

1. **A channel can be recorded before it is resolved.** A message from an
   unknown handle is logged as a channel with no identity yet — seen, but
   unmatched. You capture faithfully first and decide who it is later. (This is
   exactly what has been hard for agents talking across Bluesky and Discord:
   forced to decide "same person?" the moment a message arrives, with the least
   information. Logging the channel unresolved lets the judgement happen later,
   with more.)
2. **Each link carries a confidence and a provenance.** Not "this channel is
   tim" but "0.6, proposed by agent kestrel from a fuzzy match" versus "1.0,
   confirmed by tim himself." A matcher proposes; policy decides. We ship the
   columns, not the threshold — and because the writes that set provenance
   arrive over the authenticated bus, the provenance *is* the broker-verified
   sender, so an agent can only ever propose as itself, never confirm as the
   human.
3. **Resolution is revisable and retroactive.** Unifying an identity's messages
   is a query-time join — channel to identity — so correcting a link re-unifies
   all of history at once. Resolving at the moment a message arrives, by
   contrast, would freeze each guess into the immutable record; that is why
   messages are addressed by *channel* on the wire and identity is resolved at
   recall, not baked into the topic.
4. **Merge re-points; it never collapses, so a split can undo it.** When two
   identities turn out to be one, their channels and names are re-pointed to a
   single identity; the rows are not destroyed. Being wrong later — they were
   two people after all — is then a cheap re-point back. Split is the operation
   everyone forgets until they need it; non-destructive merge is how you keep it
   available.

Sketch of the shape (final column names settle when it is built):

```
identity( id, kind, canonical, … )
channel ( channel_kind, address, identity_id NULL, confidence, provenance, … )
alias   ( identity_id, name, context NULL )      -- name is non-unique
```

A null `identity_id` on a channel is the unresolved state in (1); confidence
and provenance are (2); the join is (3); re-pointing rather than deleting is
(4). Because the phonebook must accept writes from agents but the approvals
ledger must not, the phonebook is its own store, written by the phonebook
service over the authenticated bus — never the kernel-only-writable elanus.db.

## Recall: the unified frame, made easy but not forced

Because topics stay channel-faithful, an identity's conversation is spread
across several channels' worth of messages. Pulling them into one linear frame
— "everything to and from Tim, in order, as if it were a single chat" — is a
join over the phonebook and the ledger, and we make it a stock context-pipeline
stage (the same shape as recent-history): hand it an identity, get back the
merged timeline. The harness is never *required* to unify channels — the raw
per-channel threads stay faithful and usable — it is simply made trivial to.

A trust rule the recall stage must obey, because the correspondent decides
*whose* history loads and is therefore authority-bearing: the correspondent is
taken only from the broker-verified, kernel-stamped event topic
(`in/dm/<kind>/<addr>`), never from a self-claimed body field, and never from an
event the running agent emitted itself (its verified sender equals the agent).
Otherwise a prompt-injected agent could name a correspondent — in a payload, or
by forging its own dispatch — and pull another person's confidential messages
into its own prompt. This is the same doctrine the phonebook follows for
writes: provenance is the verified sender, never a field the writer chose. The
residual to close as the agent-authorization work matures: an agent holding a
broad publish grant could forge an `in/dm/...` event for *another* agent that
is dispatched on the channel plane — the deeper fix is reserving the ingress
prefix so only bridges, never agents, can publish `in/dm/...` (security ledger).

## Implementation notes (increment 1, as built)

The verified-sender foundation is in. The broker derives the sender from the
authenticated connection and records it on every ledgered event; it also
rides on the forwarded observation envelopes, so bus subscribers (the web
interface among them) and the handler that the dispatcher hands an event to
all see who the kernel holds responsible. The sender is set from the
session, never read from the message, so it cannot be forged by a payload
field. Events the kernel mints itself are "kernel"; events an agent's run
emits are attributed to that agent (self-reported for now, since the run
writes the ledger directly — the broker-verified path is the unforgeable
one, and the later increments close the gap by making the ledger
kernel-only-writable). Rows written before this existed have no sender;
absent should be read as "unknown", never silently treated as trusted.

## Implementation notes (increment 3, as built — 2026-06-14)

The principal became a name, not a role. The broker handshake authenticates
any fenced secret `.secrets/<name>` as a full-authority identity (the owner,
the kernel, another human), checked before any package token; a package token
is grant-scoped; no credential is refused (deny-by-default, shipped). "human"
is no longer a keyword — the default owner identity is named "owner" (configur-
able), and `kernel` and `owner` are just two fenced-secret names, so dropping
`.secrets/alice` makes "alice" authenticate too (multi-human is model-ready;
provisioning/UX is deferred). The owner's name has one source of truth — the
default profile's `owner` field — and `.secrets/.owner-name` is a cache of it
the surfaces read; `ensure` keeps the cache in sync and, on a rename or an
upgrade from a pre-rename `.secrets/human`, *moves* the existing secret to the
new name so the auth identity, the `in/human/<owner>` mailbox, and the
credential always agree and nothing is orphaned. `ELANUS_OWNER` is a runtime
override. The stock human-proxy packages (notify, escalation) match
`in/human/#` so a renamed owner still receives asks. The cage fences
`.secrets` read+write, so only the human/kernel can place a full-authority
secret; a caged agent cannot mint or read one (macOS — the Linux read-fence
gap remains the deferred limitation in section 0).

## Implementation notes (increments 2, 4, 5, as built — 2026-06-14)

- **Phonebook (increment 2)** — `packages/phonebook`, a daemon owning its own
  sqlite in its scratch (never elanus.db). Reads over HTTP (resolve / identity
  / identities / channels / whois); writes over the authenticated bus
  (`in/package/phonebook/<op>`), so each link's provenance is the broker-
  verified sender — an agent proposes only as itself. Merge is non-destructive
  (a `merged_into` pointer), so split reverts; a channel can be recorded before
  it is resolved; resolution is a query-time join. The who-is-who graph also
  lands in elanus.db as the ledgered write events (security.md entry 14
  update).
- **Recall (increment 4)** — `packages/recall`, a resident context stage
  (order 25). Given an incoming channel, it assembles the conversation with
  that person across every channel the phonebook knows, as one frame. The
  correspondent is taken ONLY from the broker-verified topic and never on a
  self-emitted event (the provenance gate; security.md entry 15) — because who
  you are talking to decides whose history loads.
- **Egress (increment 5)** — `packages/webhook`, the egress exemplar, built as
  a daemon bridge so its send attributes to it (security.md entry 16). Direct
  delivery off the bus + an `obs/channel/<kind>/sent` record; no `out/` plane.
- **Owner not auto-registered in the phonebook (decided to defer).** The owner
  is a first-class *principal* (a fenced secret + the `in/human/<owner>`
  mailbox), but nothing writes an `identity {id:owner, kind:human}` + an
  `(elanus, owner)` channel into the phonebook, so the directory and the
  principal namespace are not yet stitched together (recall does not work for
  the owner out of the box; the owner is reached as the agent's human via
  `in/human/<owner>`, not as a recalled correspondent). Seeding it (at
  phonebook startup, or init) is a small, clean follow-up; recorded here so the
  gap between "principal" and "phonebook identity" is a known, deliberate v1
  edge, not an oversight.

## Settled in this round (2026-06-13)

- **Scope of the first pass.** The sandbox-protected credential everywhere,
  and human proof as a configurable spectrum — none (a fine, common
  default), a desktop-notification tap (the easy middle), or a passkey (the
  strong case, which we do not have to build first). The mechanism is
  pluggable; nothing hardcodes passkeys.
- **Launcher-vouched identity.** Anything the kernel launches carries the
  identity the kernel gave it (a per-spawn secret), so it does not log in
  from scratch. Only human-typed entry points read the human credential from
  the fenced store, and the web interface should become kernel-launched so it
  is vouched-for automatically (and simpler to run).
- **One push.** Broker authentication, only-the-kernel-writes for the
  approvals ledger, and the sandbox read-scoping that keeps secrets out of
  actors' reach all ship together, because the latter two are what make the
  first actually hold. The thing being secured is the SQL approvals ledger; a
  file ledger is downstream and inherits the same protection.
- **The ledger gets the physical-fencing layer first** (kernel-only writes,
  sandbox-fenced database, the bus as the only path to more authority). The
  keyed-hash chain is defense in depth for later, once there is a persistent
  place to keep its key; choosing fencing-first means the kernel needs no
  persistent secret of its own yet.

## Still open (small, for the build)

- **Bootstrapping the human.** The command-line case is straightforward: the
  kernel generates a secret for the human's command-line surface and stores
  it fenced. The browser case is the real question — most likely passkey
  enrollment on first use, with the operating system holding the key. Worth a
  short concrete proposal at the start of the build.
- **Telling the sandbox what to fence.** The cage today exempts the whole
  harness root so the kernel can write its own records; the fix is to stop
  exempting the database and the secret store for actor processes while the
  kernel (which is not caged) still writes them freely. The exact mechanism —
  move those files outside the actor-writable tree, or fence them by path —
  is a build-time choice.
- **The default authentication posture** for a fresh install (authenticate
  with an easy opt-out, per the recommendation above) — confirm when wiring
  the first-run experience.

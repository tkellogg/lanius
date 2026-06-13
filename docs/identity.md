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

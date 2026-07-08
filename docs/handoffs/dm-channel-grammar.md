---
status: done
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-08
---
# Handoff B: add `dm` as a channel category and address the conversation, not the person

`docs/channels.md` and `docs/identity.md` both assume a topic shape —
`in/dm/<kind>/<addr>` — that **does not exist in the grammar.** The closed
category set in `docs/topics.md` #1 is `{agent, human, group, package, fs,
harness, channel}`; `dm` is not in it. And the one shipped ingress scaffold,
`packages/discord/lanius.toml`, actually publishes `in/package/discord/triage` —
not the `in/dm/...` form recall is designed to key on. So the convention is
documented but not realized, and the two docs disagree with the code.

Tim has approved **adding `dm` as a category** and **conflating DMs with group
chats**. This handoff makes both real at the grammar level, reserves the `in/dm/`
prefix so agents can't forge it, and reconciles the Discord scaffold. It does
**not** build the bridge or the relay — that is Handoff C. And it does **not**
build recall or the phonebook, because **they already exist** as packages:
`packages/recall/scripts/main` is a resident context stage that already parses
the correspondent from `in/dm/<channel_kind>/<address>` (line ~72), already
enforces the trust rule (correspondent taken ONLY from the broker-verified topic,
never a body field, never a self-emitted dispatch — lines ~12, ~195), and already
calls `in/package/phonebook {kind:"resolve", channel_kind, address}` to unify a
person's channels (`packages/phonebook/scripts/main`, the `identity`/`channel`/
`alias` directory with `merged_into` merge/split). **The wrinkle this handoff
fixes:** recall subscribes to `in/dm/<kind>/<addr>` but *nothing publishes it
today* — the Discord scaffold emits `in/package/discord/triage`. So recall is a
built-but-starved consumer; this handoff makes the grammar real so the existing
recall stage finally receives ingress.

## The one idea that resolves the DM/group question

**Address the conversation, not the person.**

A 1:1 DM is the *degenerate case of a group chat*. Telegram already models it
this way: every message carries a `chat.id` whether the chat is private or a
group. So the wire address is the **conversation**, and the participant set is a
lookup:

```
in/dm/<kind>/<chat-addr>     a conversation on platform <kind>, identified by
                             the platform's own conversation id <chat-addr>.
                             1:1 and group are the SAME shape — the phonebook
                             says how many participants and who.
```

This is the conflation Tim wants, and it lands *inside* `dm` — we do **not** need
a separate "group DM" concept on the wire. A private Telegram chat and a Telegram
group are both `in/dm/telegram/<chat.id>`; the difference is a phonebook fact
(one resolved participant vs. several), not a different topic.

### Addressing is uniform; membership is a security layer, not a category

**Tim's decision (this supersedes an earlier "keep `dm` and `group` distinct"
draft).** A conversation is addressed by an **ID**; its participant set is a
*resolved fact*, never a structural property of the topic. A 1:1 DM and a group
chat are the same shape — a conversation with participants — and so, at the
addressing layer, is any other room. "If a DM isn't addressed as the participants,
then it's just an ID. That's good — structurally the same." The broker/MQTT does
**not** structurally enforce who is in a conversation anyway.

So the category is **not** the authority boundary. Enforcing "only these
participants may publish / be recalled" is a **layered security measure**, applied
per conversation, not a reason to carve conversations into privileged vs.
unprivileged wire categories:

- **the reserved `in/dm/` prefix (M2)** — only a package that declares the
  ingress-bridge capability may publish external ingress; a prompt-injected agent
  can't forge `in/dm/telegram/<victim>`. This is the security measure that stops
  an agent widening a conversation's inputs.
- **publish/subscribe grants** — who may write to or read a given conversation is
  an explicit grant, checked in the broker ACL.
- **the recall trust rule (already coded in `packages/recall`)** — the
  correspondent is taken only from the broker-verified topic, so which threads
  load can't be widened from a payload.

**Embrace multi-channel reachability.** A person or an agent is reached through a
DM *and* several group chats at once — that is simply how it works, and the model
should embrace it rather than fight it: uniform conversation addressing, identities
and participants resolved through the phonebook, security applied as an explicit
layer where a boundary must hold.

**Scope for this handoff.** External-channel conversations (1:1 and group) use
`dm`, addressed by the platform's conversation id. The existing internal `group`
category (`in/group/<id>`, `code_room_members` — `src/db.rs:482`,
`src/codesession.rs:1159`) is **not refactored here** — but we stop justifying its
separateness as an "authority boundary"; its membership rules are just its own
security layer. Folding internal rooms into the one uniform conversation model is a
plausible future direction, out of scope for now. The security that matters for
`dm` is M2's reserved prefix plus grants — build that, not a category wall.

## Decisions to confirm / wonky bits

1. **Adding a category is "a design event, not a convention"** (`topics.md` #1).
   This handoff *is* that event: `dm` joins the closed set. Confirm the final set
   is `{agent, human, group, package, fs, harness, channel, dm}`. (Note `channel`
   already exists as a category but is only used today for the *receipt* plane
   `obs/channel/<kind>/{sent,acked}` — it is an obs noun, not an ingress verb.
   `channels.md` considered reusing it for ingress (`in/channel/...`) and rejected
   it; `dm` is the chosen name because it reads as "a conversation," which is what
   the address now means.) **Recommend: add `dm`; leave `channel` as the receipt
   noun.**

2. **Categories are not currently validated in code.** `src/topic.rs`
   `valid_name` (`:49`) only checks the topic is non-empty and wildcard-free;
   there is **no** enum of categories enforced anywhere. So "adding `dm` to the
   category set" is, mechanically: (a) a docs change in `topics.md` + `channels.md`,
   (b) the reserved-prefix guard in M2, (c) reconciling the Discord manifest in
   M3. There is no category-validation function to extend — confirm we are *not*
   introducing one as part of this (it would be a larger change touching every
   publish path). **Recommend: no category enforcement enum now; scope is the
   `in/dm/` reservation only.**

3. **Locator shape.** `topics.md` #2 says actor categories have single-segment
   nouns and the first `in/` locator is the conversation id. `dm` breaks that mold
   slightly: `<kind>` then `<chat-addr>` — two segments before any conversation
   locator, because the platform *is* part of the address (recall must see
   `(kind, addr)` on the topic). Confirm `dm`'s documented locator rule:
   `in/dm/<kind>/<chat-addr>`, where `<chat-addr>` is `encode_segment`-ed
   (`src/topic.rs:57`) so a platform id containing `/` can't add levels.
   **Recommend: document `dm` as a two-segment-noun category (kind + addr),
   explicitly, in `topics.md` #2.**

4. **What "reserve `in/dm/`" actually enforces.** security.md entry 15 has two
   halves: the tool path (`emit_event`) is **already closed** — `src/exec.rs:2121`
   `emit_event_in_plane_refused` refuses *any* agent-origin `in/` type except the
   agent's own mailbox (`:2025`). The **open** half is the raw bus-grant path: an
   agent with a broad `publish` grant can still forge `in/dm/...` straight on the
   bus. Reserving the prefix means: the **broker** refuses a *grant-scoped actor*
   (`actor = Some`, i.e. a session or a non-bridge package) publishing `in/dm/...`,
   allowing only full-authority principals and packages that declare a bridge
   capability. Confirm the reservation lives in the broker ACL, next to
   `actor_may_publish` (`src/broker.rs:237`). **Recommend: yes — this is the
   entry-15 residual's real fix and it belongs with the actor-authorization core,
   not in a package.**

## Milestones

### M1 — put `dm` in the grammar docs, define the conversation address

Update `docs/topics.md` #1 (add `dm` to the category set, with the "design event"
note) and #2 (document `dm`'s locator: `in/dm/<kind>/<chat-addr>`, two-segment
noun, `encode_segment`-ed addr, the conversation *is* the address). Update
`docs/channels.md` to (a) mark the "one grammar decision to make" section as
DECIDED = option (a), and (b) add the DM-is-a-degenerate-group model AND Tim's
"addressing is uniform; membership is a security layer, not a category" principle
from this handoff (do **not** frame the category as an authority boundary). Note that the
**existing** recall stage (`packages/recall/scripts/main`) already keys the
correspondent on `(kind, chat-addr)` from the broker-verified topic (never a
payload field) — this handoff makes its subscribed topic finally get published;
the multi-party resolution wrinkle is settled in M4.

- **Acceptance:** `docs/topics.md` and `docs/channels.md` agree with
  `docs/identity.md`'s `in/dm/<kind>/<addr>` usage; a reader can point to one
  canonical definition of the `dm` address shape. (Docs-only milestone — no code;
  its acceptance is internal consistency, checkable by grep for `in/dm/` across
  `docs/` all describing the same shape.)

### M2 — reserve the `in/dm/` ingress prefix to bridges (close the entry-15 residual)

In the broker publish ACL (`src/broker.rs`, at/near `actor_may_publish` `:237`),
refuse a publish to `in/dm/...` from a **grant-scoped actor** (`actor = Some`)
unless that actor is a package that declares an ingress-bridge capability in its
manifest (a manifest flag — e.g. `[request] ingress = true` or reuse the existing
`publish` grant list containing an `in/dm/#`-shaped filter that only a bridge
package's approved manifest can hold). Full-authority principals (owner/kernel —
`actor = None`) are unaffected. This is the same shape as the existing structural
scope for sessions (`:238`): the decision is made from the *stored* capability,
not the name.

- **Acceptance:** a live regression like entry 20's: a `code-*` session token
  (or a non-bridge package actor) attempting `elanus bus pub in/dm/telegram/x`
  gets `NotAuthorized`; a package whose approved manifest declares the ingress
  capability publishes `in/dm/telegram/x` successfully; the owner still can. A
  unit/integration test asserts the ACL branch. Cross-check: recall's trust rule
  (already coded in `packages/recall`) now rests on an *enforced* invariant, so
  update security.md entry 15 from "[LATENT] residual … still open" to
  fixed-for-the-bus-path.

### M3 — reconcile the Discord scaffold to the canonical form

Change `packages/discord/lanius.toml` so ingress publishes `in/dm/discord/<addr>`
(the canonical, recall-keyable form) instead of `in/package/discord/triage`, and
declare the ingress-bridge capability M2 checks for. Keep its egress inbox
(`in/package/discord/send`) and receipt plane (`obs/channel/discord/#`) as they
are — those are correct. Update any in-repo dispatcher/subscribe filters that
matched the old `in/package/discord/triage` to `in/dm/discord/#` (or
`in/dm/#` for a handler that wants all platforms).

- **Acceptance:** the Discord package manifest is internally consistent (its
  declared publish topic matches what a bridge is now permitted to publish under
  M2), and any package-manifest lint / e2e that loads the in-repo packages still
  passes. (The scaffold is UNTESTED by its own header, so acceptance is
  manifest-level, not a live Discord connection.)

### M4 — settle multi-party resolution (the one genuine open design point)

This is the real wrinkle, and it must be decided, not hand-waved. The existing
recall stage resolves **one correspondent per event**, keyed on `(kind,
address)`: `resolve(kind, addr)` (`packages/recall/scripts/main:108`) returns
`(identity, channels)`, and `gather(channels, …)` (`:144`) then pulls the message
history across **every channel of that identity** — the cross-channel unified
inbox. For a 1:1 that is exactly right. For a **group** chat the address *is* the
conversation (`in/dm/telegram/<group-chat-id>`), so a naive resolve would treat
the whole group as one "identity" — and, worse, if that group resolved to a
person, `gather` would pull that person's *private* other-channel history into a
group prompt. A group is not a person; a group member's private history must not
leak into the room.

**Decision (recommended).** A group conversation resolves to its **own
non-human identity** in the phonebook — the phonebook `identity` table already
carries a `kind` column (`packages/phonebook/scripts/main:150`), so a group is
`identity{kind:"group"}` (or `"conversation"`), distinct from any human
participant. Its channel set is **only its own conversation address(es)**, never
its members'. Then the existing recall code needs **zero change**: `resolve`
returns the group identity, `gather` loads only that conversation's thread
(`in/dm/telegram/<group-chat-id>`), and no member's cross-channel history is
pulled. The safe default — "a group's recall unit is the group's own thread" —
falls out of the phonebook *data model*, not new recall logic.

- The **per-message sender** (who in the room spoke) is a separate axis from the
  correspondent (the room). Recall keys threads on the correspondent, so the
  group's participant set never decides *which* threads load — which is precisely
  what stops a prompt-injected agent from widening it. Optionally annotating each
  recalled line with the resolved sender identity is a later enhancement; if
  added, the sender is the **broker-verified `sender`**, resolved per line, and is
  **never** used to widen which threads load (same trust rule, applied to the
  sender axis).
- **1:1 stays a person.** `in/dm/telegram/<private-chat-id>` resolves to a human
  identity whose channel set spans their platforms → the cross-channel unified
  inbox Tim wants. Only *group* addresses get the group-entity treatment.

- **Acceptance:** a documented rule (in `docs/identity.md`/`docs/channels.md`) —
  "a group-chat address is a `kind:group` phonebook identity whose channels are
  its own conversation address(es) only; a 1:1 address resolves to the human." A
  worked check against `packages/recall/scripts/main`: given a `kind:group`
  identity, `resolve`→`gather` loads only the group thread (assert the channel set
  contains just the conversation address); given a human 1:1, it loads the human's
  cross-channel set. No recall-code change is required for the safe default — if
  the reviewer finds one is, that is a finding to surface, not silently patch.

## Read these first

- `docs/topics.md` (#1 category set, #2 locator conventions, the `group` and
  conversation `[DECIDED]` blocks at "Conversations and rooms").
- `docs/channels.md` — the transport-is-a-package design; the "one grammar
  decision to make" section this handoff decides.
- `docs/identity.md` — the phonebook (`identity`/`channel`/`alias`) and recall's
  provenance rule ("the correspondent decides *whose* history loads").
- `docs/security.md` entry 15 (the `in/dm/` forge residual this closes) and entry
  16 (why a bridge must be a daemon — Handoff C's problem, but read it).
- `src/topic.rs:35-69` — `valid_filter` / `valid_name` / `encode_segment` (there
  is no category enum today).
- `src/broker.rs:227-271` — `actor_may_publish` / `actor_may_subscribe` (where
  the reservation goes).
- `src/exec.rs:2025, 2101-2127` — the already-closed tool half of entry 15.
- `packages/discord/lanius.toml` — the scaffold to reconcile.

## Residuals / gating

- **Recall and the phonebook already exist** (as packages, not core `src/` — a
  `src/` grep misses them): `packages/recall/scripts/main` (the resident stage,
  parsing `in/dm/<kind>/<addr>` and enforcing the single-correspondent trust rule)
  and `packages/phonebook/scripts/main` (the `identity`/`channel`/`alias`
  directory with merge/split and a `resolve` API). This handoff does not build
  them; it makes the grammar real so the **existing** recall stage finally
  receives ingress (today nothing publishes the topic it subscribes to). Both are
  inert until approved (`elanus approve recall` / `phonebook`) and are in no
  default kit — **wiring and exercising them is Handoff C's job**, not deferred.
- **Seeding `owner` in the phonebook** (channels.md gap 3) is deferred to Handoff
  C (it is part of exercising the phonebook end-to-end). The
  **EA/channel-selection policy** (gap 4) is genuinely out of scope for the whole
  arc — that one really is unbuilt.
- **M2 depends on a way to mark a package as a bridge.** If the manifest has no
  natural flag, the smallest honest option is: a bridge is a package whose
  *approved* manifest holds a publish grant covering `in/dm/#` — and only an
  owner-approved manifest can hold that. Confirm with the packages/grants owner
  before inventing a new manifest key.
- Independent of Handoff A. Handoff C (the Telegram bridge + agent-DM relay)
  depends on **this** handoff (the `dm` grammar + reserved prefix + reconciled
  scaffold).

## Log

- 2026-07-08 — planner drafted from the worktree. Confirmed: no category enum in
  `topic.rs`; the Discord scaffold really does publish `in/package/discord/triage`
  (`packages/discord/lanius.toml`); entry-15 tool path already closed at
  `exec.rs:2121`, bus path still open. **Corrected** an earlier `src/`-only-grep
  error: recall (`packages/recall/scripts/main`) and phonebook
  (`packages/phonebook/scripts/main`) are **built as packages** — recall already
  parses `in/dm/<kind>/<addr>` and calls phonebook `resolve`; this handoff feeds
  its starved subscription. Decided the DM/group conflation as: unify
  1:1-and-group *inside* `dm`; and (M4) a group resolves to a `kind:group`
  phonebook identity whose channel set is its own conversation only, so recall
  loads the room's thread and never a member's private history — zero recall-code
  change.
- 2026-07-08 (Tim, tech-lead override) — **the category is NOT the authority
  boundary.** An earlier draft kept `dm` and `group` distinct because "the
  category boundary is the membership-authority boundary"; Tim rejected that.
  Addressing is uniform (a conversation is an ID; participants are resolved, not
  structural); enforcing participants is a layered security measure (the reserved
  `in/dm/` prefix + grants + the recall trust rule), because the broker/MQTT does
  not enforce membership structurally anyway. Embrace multi-channel reachability
  (a person is reached via a DM and several groups at once). The code scope
  (M2/M3) is unchanged — only the rationale: build the security layer, not a
  category wall. M4's group-resolution rule stands, reframed as the security
  measure that keeps a group's recall to its own thread.
</content>
</invoke>

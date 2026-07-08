---
status: done
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-08
---
# Handoff C: agent DMs and a Telegram bridge — one publish, many renderers

The payoff of Handoffs A and B: an agent can DM you, and you can DM it back, on an
external messaging app — built entirely as a **package + grants + phonebook
rows**, editing **zero kernel files** for the new channel. `docs/channels.md`
argues the whole case ("a transport is just a package"); this handoff turns that
doc into a shippable Telegram bridge and, in passing, removes the last two places
where the kernel still hard-codes the channel taxonomy.

The through-line (channels.md): **the web dashboard is already a relay of the
`in/human/<owner>` plane.** An agent DMing you in the browser and an agent DMing
you on Telegram are the *same publish, different renderer*. That is why agent-DMs
and the Telegram bridge are one arc, not two.

**Tim's explicit goal for this whole exercise:** "get me on Telegram and start
**load-testing the phonebook** so you can talk to me seamlessly across many
platforms." The phonebook and recall are **already built** as packages
(`packages/phonebook/scripts/main`, `packages/recall/scripts/main`) — so wiring
and *exercising* them is **in scope here**, not a residual. The payoff this
handoff must reach is "routes, renders, **and unifies**": an inbound Telegram
message resolves to identity `tim` via the phonebook and recall pulls his
elanus-channel history into the frame. Only "routes and renders" would fall short
of the goal.

**Depends on Handoff B** (the `dm` grammar, the reserved `in/dm/` prefix, the
reconciled scaffold). **Soft-depends on Handoff A** (the source/worker
classification is cleaner once `kind` is a stored field; the `source_for` cleanup
in M3 is easier but not blocked). Do B first.

## Decisions to confirm / wonky bits

1. **The bridge is a daemon, not an exec handler — non-negotiable.**
   security.md entry 16: an exec-mode handler runs uncaged and tokenless, so when
   it publishes it authenticates as the **owner**, mislabeling every send. A
   daemon is spawned token-authed and caged, so the broker stamps
   `sender = telegram`. The `packages/webhook` egress exemplar is a daemon for
   exactly this reason (entry 16, e2e §20 asserts `sender = webhook`). Copy that
   shape. **Recommend: daemon; this is settled, restated only so nobody
   "simplifies" it into an exec handler.**

2. **The bridge holds the bot token; no agent ever sees it.** Same as the Discord
   scaffold — the daemon reads `TELEGRAM_TOKEN` from its own environment,
   authenticates to the bus via supervisor-injected `ELANUS_PACKAGE` /
   `ELANUS_BUS_TOKEN`, and **parks (sleeps), does not crash-loop**, when
   unconfigured. **Recommend: mirror `packages/discord` park-not-crash behavior.**

3. **No "send to Tim, system picks the platform" primitive.** Egress is
   command-shaped: an agent publishes to a *specific* bridge inbox
   (`in/package/telegram/send`). *Which* channel to reach the owner on is a
   **policy** owned by an EA/human-proxy actor reading `obs/channel/+/acked`
   latency — out of scope here (channels.md gap 4). **Recommend: build the
   mechanism (per-bridge inbox), not the policy.**

4. **Ingress addresses the conversation (Handoff B).** The daemon publishes each
   arrival **once** to `in/dm/telegram/<chat.id>` — 1:1 and group chats are the
   same shape; the phonebook resolves participants. No twin-publish; the arrival's
   observation is the delivery echo (`topics.md` #3). **Recommend: single publish
   to the `dm` plane.**

5. **The `source_for` cleanup: how far?** `src/web.rs:2385` `source_for`
   hard-codes the channel taxonomy (`web-`, github/jira/linear, cron/timer) into
   the core conversation projection — adding a channel edits the kernel. The right
   seam already exists: `source_for` *already* prefers an explicit
   `payload.source` (`:2386`) before falling to the hard-coded guesses. The clean
   move is to make the **bridge/handler stamp `payload.source`** (the package
   name / channel kind) so the fallbacks wither. Confirm scope: this handoff
   *adds* the stamp on the new Telegram path and *moves* the existing hard-coded
   cases to be package/descriptor-declared where cheap, but does **not** have to
   delete every legacy fallback in one go (github/jira/linear may have no package
   yet). **Recommend: stamp source at the source; leave legacy fallbacks as a
   shrinking safety net with a `TODO`, don't block the bridge on deleting them.**

## Milestones

### M1 — the Telegram egress path (agent → owner on Telegram)

Add `packages/telegram/` as a **daemon** package, copying `packages/webhook`
(egress) and `packages/discord` (daemon lifecycle, park-not-crash). Manifest:
`[request] subscribe = ["in/package/telegram/send"]`,
`publish = ["obs/channel/telegram/#", "in/dm/telegram/#"]`, `[process] mode =
"daemon"`. The daemon consumes `in/package/telegram/send {recipient, text}`,
calls the Bot API `sendMessage` (off the bus), and emits
`obs/channel/telegram/sent` (and `obs/channel/telegram/acked` if the platform
gives a delivery signal).

- **Acceptance:** with a test/stub Bot API (or a recorded transport), publishing
  `in/package/telegram/send {recipient, text}` produces an
  `obs/channel/telegram/sent` receipt **stamped `sender = telegram`** (the entry-16
  invariant — assert the sender, mirroring webhook e2e §20). Unconfigured (no
  `TELEGRAM_TOKEN`), the daemon parks and does not crash the supervisor.

### M2 — the Telegram ingress path (owner → agent on Telegram)

The daemon long-polls `getUpdates` holding the bot token, and on each message
publishes **once** to `in/dm/telegram/<chat.id>` (`encode_segment` the id). The
dispatcher routes it to whatever handler/agent holds an approved
`subscribe in/dm/#` (or `in/dm/telegram/#`) grant (`packages::matching_exec_handlers`, called from
`src/dispatcher.rs:1713`, → `topic::matches`). This ingress is
permitted by Handoff B M2 because the Telegram package declares the bridge
capability; a non-bridge actor forging `in/dm/telegram/...` is refused.

- **Acceptance:** a simulated inbound update causes exactly one
  `in/dm/telegram/<chat.id>` ledger event with `sender = telegram`; a subscribed
  test handler receives it; **no** second/twin publish exists. Negative: a
  `code-*` session attempting the same publish is `NotAuthorized` (Handoff B M2
  regression, re-asserted here on the live Telegram plane).

### M3 — de-magic the web conversation projection (`source_for` / `is_worker_session`)

In `src/web.rs`, make `source` come from data, not spelling: ensure the
send_message/ask_human/bridge paths stamp `payload.source` (channel kind or
package name) so `source_for` (`:2385`) resolves via its existing
`payload.source` branch (`:2386`) for anything new, and a Telegram-originated
conversation renders with `source = "telegram"` with no kernel edit. Fold
`is_worker_session` (`:2381`) onto the Handoff-A `kind` helper if A has landed
(otherwise leave it and note the dependency). Leave the legacy github/jira/linear/
cron fallbacks in place as a shrinking net with a `TODO` pointing at
"channel/package-declared source" (channels.md, closing section).

- **Acceptance:** a conversation seeded from an `in/dm/telegram/<chat.id>` inbound
  (or its correlated `in/human/<owner>` reply) renders in the web comms list with
  `source = "telegram"`, derived from stamped `payload.source`, **without** adding
  a `telegram` branch to `source_for`. Existing web comms e2e (coding runs
  evicted, curated conversations preserved, `web-`/github/cron sources still
  labeled) stays green.

### M4 — record the correspondent's channel in the phonebook on ingress

On each inbound Telegram message, record the sighting in the phonebook so it can
be resolved: publish `in/package/phonebook/channel {channel_kind:"telegram",
address:<chat.id>}` (an unresolved sighting — `op_channel`,
`packages/phonebook/scripts/main:249`, leaves `identity_id` NULL until linked).
**Provenance is the broker-verified `sender` of that phonebook write, never a
chosen field** (the phonebook's own rule, `:18` — every link's provenance is the
verified sender). Who writes it: either the bridge daemon itself, or the
ingress-handling agent — pick the one whose verified `sender` you want on the
audit trail; recommend the bridge (it is the party that actually saw the wire
address). **Linking** the sighting to identity `tim` (`in/package/phonebook/link`)
is owner/EA territory (a human vouches "this Telegram chat is me") and is done
once in M5's seeding, not on every message.

- **Acceptance:** after a simulated inbound, a `channel` row exists for
  `(telegram, <chat.id>)` with `provenance` = the broker-verified sender of the
  phonebook write (assert it is the bridge/handler identity, not a payload field);
  a second identical inbound does not duplicate or corrupt the row (upsert is
  idempotent, `:231`).

### M5 — approve `recall` + `phonebook` and seed `owner` (make unification live)

Exercise the built-but-inert packages: approve `phonebook` and `recall` for the
test profile (both park until approved, and neither is in a default kit), and
seed the owner so recall can unify Tim's own channels (channels.md gap 3): create
`identity{id:"owner", kind:"human"}` (`in/package/phonebook/identity`) and link
his known elanus channel (`in/package/phonebook/link {channel_kind:"elanus",
address:"owner", identity:"owner"}`), plus link the Telegram chat from M4
(`link {channel_kind:"telegram", address:<chat.id>, identity:"owner"}`). Now
`resolve("telegram", <chat.id>)` returns identity `owner` with a channel set
spanning elanus + telegram.

- **Acceptance:** with the packages approved and the owner seeded, a phonebook
  `resolve {channel_kind:"telegram", address:<chat.id>}` returns `resolved:true`
  with identity `owner`; and a recall run over an inbound Telegram event produces
  a frame that includes at least one prior **elanus-channel** message for `owner`
  (proving cross-channel unification, not just the single Telegram thread).

### M6 — the round trip, end to end: routes, renders, AND unifies

Wire it all into one conversation: an owner message on Telegram lands as
`in/dm/telegram/<chat.id>`, the phonebook resolves it to identity `owner` and
recall pulls his cross-channel history into the prompt, an agent runs and replies
via `in/package/telegram/send`, the reply is delivered and observed on
`obs/channel/telegram/sent`, and the web UI shows the same exchange as a
`source = telegram` conversation. This is the payoff — the "same publish,
different renderer" claim *and* "seamless across many platforms" made real. Add
an in-repo e2e (stubbed transport) that drives the full loop.

- **Acceptance:** the e2e drives inbound → **phonebook resolve → recall
  enrichment** → agent turn → outbound → receipt, and asserts: one ingress event
  (`sender = telegram`), the recalled frame contains Tim's prior elanus-channel
  message(s) for the resolved identity (the load-test-the-phonebook payoff), a
  correct `obs/channel/telegram/sent` receipt (`sender = telegram`), and the web
  projection rendering the exchange as one `telegram` conversation. The kernel
  diff for adding Telegram is **zero files** outside `packages/telegram/` and the
  (generic, channel-agnostic) `source_for` stamp from M3.

## Read these first

- `docs/channels.md` — the whole design; the Telegram worked example (inbound
  steps 1-4, outbound), the "two templates already exist" section, and the "the
  kernel should never learn a new channel's name" close.
- `packages/webhook/lanius.toml` + its `scripts/` — the egress daemon template
  (entry-16-correct).
- `packages/discord/lanius.toml` + its `scripts/` — the ingress daemon lifecycle
  (park-not-crash, supervisor-injected token).
- `src/web.rs:2360-2422` — `session_for_event`, `is_worker_session`, `source_for`
  (the hard-codes to retire; note `payload.source` is already preferred at
  `:2386`), and `:2535-2611` (how inbound + ambient rows seed conversations).
- `packages::matching_exec_handlers` (called from `src/dispatcher.rs:1713`) — how
  an ingress topic routes to a subscribed handler.
- `packages/phonebook/scripts/main` — the identity directory: the bus write ops
  (`op_channel:249`, `op_link:262`, `op_identity:212`), provenance = verified
  sender (`:18`), and the `resolve` query recall calls.
- `packages/recall/scripts/main` — the resident stage this bridge feeds:
  `parse_correspondent`/`dm_topic` (`:72`), `resolve` (`:108`), `gather` (`:144`),
  and the self-sender trust gate (`:195`).
- `docs/security.md` entry 16 (daemon-not-exec) and entry 15 (the reserved `dm`
  prefix that makes ingress trustworthy).
- Handoffs **A** (`principal-kind.md`) and **B** (`dm-channel-grammar.md`) — this
  handoff consumes both.

## Residuals / gating

- **Cross-channel unification is IN scope** (M4-M6), because recall and the
  phonebook are already built (`packages/recall`, `packages/phonebook`) — they
  were only starved of ingress. This handoff wires and exercises them; that is
  Tim's stated goal (load-test the phonebook).
- **EA / channel-selection policy** (which platform to reach the owner on;
  escalation A→B→C) is **genuinely** out of scope (channels.md gap 4) — that one
  really is unbuilt. This handoff builds the per-bridge inbox *mechanism* the
  policy will later choose among; it does not build the chooser.
- **Auto-linking a sighting to an identity is deferred.** M4 records the sighting;
  M5 links it once via an owner-vouched seed. A general "which human is this new
  chat" auto-linker is identity-model work (the phonebook notes per-sender
  restriction is still open, `packages/phonebook/scripts/main:32`), not this
  handoff.
- **The legacy `source_for` fallbacks** (github/jira/linear/cron) stay until
  those sources become packages that stamp their own `payload.source`; M3 shrinks
  the net, it does not empty it.
- **Live Telegram** requires a real bot token; CI acceptance uses a stub/recorded
  transport (same posture the Discord scaffold documents for itself).

## Log

- 2026-07-08 — planner drafted from the worktree. Confirmed the two daemon
  templates exist (`packages/webhook`, `packages/discord`), `source_for` already
  prefers `payload.source` (`web.rs:2386`) so the de-magic seam is real, and the
  ingress-routing path is `packages::matching_exec_handlers`
  (`src/dispatcher.rs:1713`) → `topic::matches`.
- 2026-07-08 (revision) — **corrected a `src/`-only-grep error**: recall and the
  phonebook are **built as packages** (`packages/recall/scripts/main`,
  `packages/phonebook/scripts/main`), not unbuilt. Cross-channel unification moved
  from a residual into scope as M4 (record the `(telegram, chat.id)` sighting,
  provenance = verified sender), M5 (approve `recall`+`phonebook`, seed `owner`),
  and M6 (round-trip now asserts recall pulls Tim's cross-channel history — the
  "load-test the phonebook" payoff). EA/channel-selection (gap 4) stays out.
  Sequenced after B (needs the `dm` grammar + reserved prefix); soft-after A (the
  worker/source classification is a stored `kind` once A lands).
</content>
</invoke>

---
status: implemented
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-13
---
# Telegram bridge — close the loop: chat with your agents from your phone

The Telegram bridge **daemon already exists and is committed** (`packages/telegram/`,
commit `eeaf712` "handoffs A/B/C"): a 255-line two-way daemon that long-polls
`getUpdates` into `in/dm/telegram/<chat.id>`, serves egress on
`in/package/telegram/send` with `sender = telegram` receipts, records phonebook
sightings, suppresses bot echoes, acks the update offset, and parks (does not
crash-loop) when unconfigured. `packages/webhook` (egress exemplar) and
`packages/discord` (ingress scaffold) are its parents; the design law it embodies
is `docs/channels.md` — "a transport is just a package," zero kernel edits.

So this handoff is **not** "build the bridge." It is the honest gap between *a
bridge that publishes to the bus* and *Tim texting his agents from a train and it
working* (`docs/journeys/chat-from-anywhere.md`). Four things stand between the two,
and every one of them is verifiable against code read on 2026-07-13:

1. **A reply an agent writes never reaches the phone.** `send_message`/`ask_human`
   emit to `in/human/<owner>` (`src/exec.rs:986`, `:1685`). `reply_source`
   (`src/exec.rs:2039`, called `:2234`/`:2298`) stamps that reply so the *web*
   renders it `source = "telegram"` — but **nothing forwards `in/human/<owner>` to
   `in/package/telegram/send`.** The web shows the reply; Tim's phone never buzzes.
   The loop is open on the outbound-return side.
2. **An inbound is never authenticated as the owner, and an unknown sender is not
   fenced.** The daemon publishes `in/dm/telegram/<chat.id>` for *any* sender and
   records an *unresolved* phonebook sighting (`packages/telegram/scripts/main:174-206`).
   Nothing maps a chat id to identity `owner`; nothing stops a stranger's message
   from being dispatched. And the journey's core demand — "it's the same
   conversation" — needs a verified-owner inbound to land in the owner's converse
   plane, not a parallel `in/dm/telegram/*` silo.
3. **The bot token is not in the vault, and in fact never reaches the daemon at
   all today.** The manifest declares `[[config.keys]] TELEGRAM_TOKEN` and the
   script reads it from its own env or a **plaintext** `config/packages/telegram.toml`
   (`packages/telegram/scripts/main:54-73`). But the daemon-spawn seam
   (`src/dispatcher.rs:495-521`) injects `ROOT/DB/PACKAGE/SCRATCH/BUS_ADDR/BUS_TOKEN`
   and **never materializes package config keys into the child env** (verified: no
   config-key→env wiring at the spawn site or in `config_repo`). So today the token
   arrives only if the operator exports it into the supervisor's own environment,
   or writes it plaintext to disk. Both are wrong for a secret.
4. **There is no end-to-end proof and no operator on-ramp.** Unit tests exist
   (`reply_source_derives_channel_from_ingress` `src/exec.rs:3174`;
   `telegram_conversation_renders_source_from_stamp` `src/web.rs:3459`) but nothing
   drives inbound → resolve → agent turn → reply → phone → receipt as one loop, and
   there's no runnable "BotFather → approve → talk to it" path.

**Depends on** Handoff A (`principal-kind.md`, done) and Handoff B
(`dm-channel-grammar.md`, done) — the stored `kind` field and the reserved,
broker-enforced `in/dm/` prefix. Both are landed; this consumes them. **Continues**
Handoff C (`agent-dm-relay.md`, done — it built the bridge and the `reply_source`
render stamp). This is C's unfinished second half: C made a telegram conversation
*render*; this makes it *round-trip*.

## Decisions to confirm / wonky bits

1. **Outbound transport = long-poll `getUpdates`. RULED, keep it.** The committed
   daemon already long-polls (`inbound_loop`, `scripts/main:209`). Do not switch to
   webhooks. Reasoning, in the journey's own terms ("I don't want to babysit an
   endpoint"): a webhook requires Telegram to reach *in* to a public HTTPS URL with
   a valid cert — which a laptop behind a home NAT does not have, forcing a tunnel
   or a rented relay. That is more ops and a **new inbound attack surface** (an
   internet-reachable endpoint) for zero benefit on a personal machine. Long-poll
   needs no inbound port, no cert, no public DNS; it is already written and it is
   the safer default. Webhook is a **residual** for a future always-on hosted
   deployment, not this handoff.

2. **Reply routing is deterministic correlation-follow, NOT the deferred EA
   policy.** Handoff C explicitly deferred "which platform to reach the owner on"
   as EA/channel-selection policy (`channels.md` gap 4). That deferral still holds
   for *unprompted* egress. But replying to a Telegram-*originated* conversation
   needs **no policy**: the inbound was `in/dm/telegram/<chat.id>`, the agent's
   reply threads on that same correlation (`OutboundMessage.correlation`,
   `src/exec.rs:991-994`), so the target chat id is *carried by the correlation*,
   not chosen. The forwarder is the routing twin of `reply_source`'s *rendering*
   logic (`src/exec.rs:2039`): where `reply_source` reads the correlated ingress to
   stamp a label, the forwarder reads it to pick a `recipient`. **Build the
   deterministic reply-follow; leave "agent picks a platform unprompted" deferred.**

3. **Where does the reply-forwarder live?** Options: (a) a new small daemon package
   subscribing the reply plane, generic over *all* channels; (b) fold it into the
   telegram bridge (it already owns the send path and the chat-id knowledge);
   (c) a kernel seam in `emit_message`. **Recommend (b) for this handoff, factored
   so (a) is a later generalization:** the telegram daemon gains a third loop that
   subscribes replies correlated to *its own* `in/dm/telegram/*` ingress and
   forwards them to its own `sendMessage`. It stays **stateless** — the chat id is
   re-derived from the correlated ingress event in the ledger, not held in memory —
   so a crash-only restart loses nothing. No kernel edit; honors the design law.
   (If Tim wants the generic channel-return router now, that's option (a) as its own
   package; say so and it becomes M1's shape instead.)

4. **How does a verified-owner inbound become "the same conversation" — promote to
   `in/human/<owner>`, or subscribe the `dm` plane directly?** The journey's
   keystone ("it's the same conversation") and the existing render test
   (`telegram_conversation_renders_source_from_stamp` seeds an **`in/human/<owner>`**
   row stamped `source:"telegram"`, `src/web.rs:3477`) both point at **promotion**:
   a verified-owner `in/dm/telegram/<chat.id>` is re-emitted onto `in/human/<owner>`
   with `source:"telegram"` and the chat id retained, so the *same* converse agent
   and the *same* web pane handle it. The alternative — the owner's agent subscribes
   `in/dm/telegram/#` directly and recall unifies — is less code but leaves the
   telegram thread in a separate plane the current projection does not fold into the
   owner conversation. **Recommend promotion**, with the hard security caveat in
   wonky-bit 5. (Impl must verify the web projection actually renders the promoted
   row as one continuous conversation; the render test proves the *shape* is right.)

5. **Who is trusted to promote onto owner mail?** `in/human/<owner>` is the owner's
   mailbox; forging it is owner-impersonation (the exact class the reserved `in/dm/`
   prefix and `emit_event`'s `in/`-plane refusal exist to prevent — `security.md`
   entry 15, `src/exec.rs:2135-2157`). So the resolve-and-promote step must be done
   by a **broker-trusted, token-authed actor** whose verified `sender` the broker
   stamps, and it must be **fail-closed**: only a chat id the phonebook resolves to
   `owner` is promoted; an unresolved/unknown sender is recorded as a sighting and
   **stops there** (no dispatch as owner mail). This is M2 and it is the security
   spine of the whole feature — the journey's "it knows it's me / a stranger doesn't
   get to be me." Confirm the promoter's home: recommend a small resident step in
   the phonebook/recall-adjacent path (both already daemons with verified senders),
   NOT the raw bridge (whose verified sender is `telegram`, not a trust authority
   over the owner mailbox) and NEVER an agent.

6. **Bot token: encrypted vault credential, materialized into daemon env at spawn.**
   The provider vault (`src/provider.rs`: `Credential` enum `:114`,
   XChaCha20-Poly1305 `:26`/`:510`, `providers` table `:553`, `lanius provider add`
   `src/providercli.rs:33`) is the right home — it is exactly "held the way my
   model-provider keys are held." Wonky sub-decision: the vault's `Consumer` is
   `dispatcher | harness` and its `Credential` variants are `ApiKey`/`NativeLogin`
   — a bot token for a *daemon package* is a third consumer. **Recommend: reuse the
   vault's crypto (`seal`/`open`, the sealed `secret` blob + `nonce` columns) and
   add a minimal package-secret binding**, rather than contorting the token into an
   `ApiKey`. Then materialize it into the child env at the spawn seam
   (`src/dispatcher.rs:495-521`) — decrypted transiently, alongside `BUS_TOKEN`,
   never written to `config/packages/telegram.toml`. This closes **both** the
   absent config-key→env wiring **and** the plaintext exposure in one seam. Keep
   the `TELEGRAM_TOKEN` env / `TELEGRAM_API_BASE` overrides for CI/stub.

## Milestones

Each milestone is independently landable and testable, with a stub Bot API
(`TELEGRAM_API_BASE` → recorded transport, `scripts/main:74`) so no live token is
needed in CI. Ordered by dependency.

### M1 — Reply routing: an agent's reply reaches the phone

Add the reply-follow (wonky-bit 3, recommended shape: a third loop in the telegram
daemon). It subscribes the reply plane, and for a reply **correlated to a
`in/dm/telegram/<chat.id>` ingress** (or to a `source:"telegram"` promoted owner
event, M2), re-derives the chat id from the correlated ingress event in the ledger
and forwards `in/package/telegram/send {recipient:<chat.id>, text:<reply text>}`
— which the existing egress loop turns into a `sendMessage` + `obs/channel/telegram/sent`
receipt (`sender = telegram`). Stateless: no in-memory correlation map; a restart
loses nothing. A reply with **no** channel-correlated ingress produces **no**
telegram send (no accidental egress).

- **Acceptance:** an in-repo test (stub transport) drives: seed an
  `in/dm/telegram/<chat.id>` ingress on correlation `C` → an agent reply
  (`send_message`, correlation `C`, targeting `in/human/<owner>`) → the forwarder
  emits `in/package/telegram/send {recipient:<chat.id>, ...}` → exactly one
  `sendMessage` to `<chat.id>` and one `obs/channel/telegram/sent` stamped
  `sender = telegram`. Negative: a reply correlated to a *non-channel* ingress
  (e.g. a `web-` conversation) yields **zero** telegram sends. Idempotence: a
  duplicate reply on `C` does not double-send (assert one receipt).

### M2 — Owner-sender authentication + promotion (the security gate)

Seed `identity{id:"owner", kind:"human"}` and link Tim's Telegram chat id once,
owner-vouched (`in/package/phonebook/link {channel_kind:"telegram", address:<chat.id>,
identity:"owner"}`; phonebook `op_link` `packages/phonebook/scripts/main:262`,
provenance = broker-verified sender). Add the trusted **resolve-and-promote** step
(wonky-bits 4–5): on a `in/dm/telegram/<chat.id>` ingress, a token-authed actor
calls phonebook `resolve` (`q_resolve` `:327`, via recall's `resolve` `scripts/main:108`);
**if** it resolves to `owner`, re-emit onto `in/human/<owner>` with
`source:"telegram"` and the chat id retained (so M1's forwarder and the web
projection both find it); **else** the message stays an unresolved sighting and is
**not** dispatched as owner mail. Fail-closed.

- **Acceptance:** an inbound from the **linked** chat id resolves to `owner`,
  appears on `in/human/<owner>` stamped `source:"telegram"` with the chat id
  retained, and reaches the owner's agent. An inbound from an **unlinked** chat id
  does **not** produce an `in/human/<owner>` event (asserted) and does **not** reach
  an agent as owner — only an unresolved `(telegram, chat.id)` phonebook sighting.
  A replayed inbound (same `update_id`) does not double-publish (the existing offset
  ack, `scripts/main:227`, is the mechanism — assert it). The promoter's verified
  `sender` is the trusted actor, never a payload field.

### M3 — Bot token as an encrypted vault credential, injected at spawn

Store the token in the vault's crypto (`src/provider.rs` `seal`/`open`, sealed
`secret` blob + `nonce`) via a minimal package-secret binding (wonky-bit 6), set by
a CLI in the `lanius provider`/`lanius config` family (secret read off stdin, never
argv — mirror `providercli::resolve_key` `:72`). Extend the daemon-spawn seam
(`src/dispatcher.rs:495-521`) to materialize a package's declared secret into the
child env (decrypted transiently) alongside `BUS_TOKEN`. Delete the plaintext
`token =` fallback from `config/packages/telegram.toml` / `scripts/main:_config`;
keep the `TELEGRAM_TOKEN` env + `TELEGRAM_API_BASE` overrides for CI.

- **Acceptance:** setting the token writes an **encrypted** blob (nonce + sealed
  secret, no plaintext column, `REDACTED` on read — `provider.rs:109`); a spawned
  telegram daemon receives the decrypted token in its env with **no plaintext token
  anywhere at rest** (`git grep`/config scan shows none); unconfigured (no
  credential, no env) the daemon still **parks**, and `lanius packages check`
  reports the missing-token fix. The token is never logged or printed.

### M4 — End-to-end round trip + operator on-ramp (chat-from-anywhere, real)

One stub-transport e2e drives the whole loop and the journey's one-line test:
inbound(linked owner) → phonebook resolve → promote to `in/human/<owner>`
(`source:"telegram"`) → agent turn → `send_message` reply on the correlation →
reply-forwarder → `sendMessage` → `obs/channel/telegram/sent` → the web projection
(`comms_view.py source_for`, `kits/stdlib/packages/comms/scripts/comms_view.py:193`,
prefers `payload.source`) shows it as **one** `source = "telegram"` conversation.
Plus a runnable setup section in `packages/telegram/SKILL.md`: BotFather token →
`lanius provider add` (M3) → `lanius approve telegram` → message the bot → seed +
link the owner chat id (M2). Manually validate once against a live BotFather token
from a phone.

- **Acceptance:** the e2e asserts the full loop with `sender = telegram` on **both**
  the ingress-derived owner event and the egress receipt, and the web projection
  rendering the exchange as a single continuous `telegram`-sourced conversation
  (not two threads). The kernel diff for the whole feature is bounded to: the
  spawn-seam secret injection (M3) and the trusted-promoter seam (M2) — everything
  else lives in `packages/telegram/` and the phonebook/recall packages, per the
  "zero kernel edits for a new channel" law (`channels.md`).

## Read these first

- `docs/journeys/chat-from-anywhere.md` — the intent; the "same conversation,"
  "knows it's me / a stranger doesn't," "secret stays secret," and "don't babysit
  an endpoint" tests to reason against.
- `docs/channels.md` — the design law (a transport is a package; zero kernel edits;
  inbound → `in/dm/<kind>/<addr>`, outbound receipt → `obs/channel/<kind>/{sent,acked}`).
- `docs/handoffs/agent-dm-relay.md` — **Handoff C** (done), which built this bridge;
  this is its unfinished second half. Also `principal-kind.md` (Handoff **A**, done)
  and `dm-channel-grammar.md` (Handoff **B**, done). *Note: the A/B labels are
  crossed relative to filenames — `principal-kind.md` calls itself "A,"
  `dm-channel-grammar.md` calls itself "B."*
- `packages/telegram/{lanius.toml,SKILL.md,scripts/main}` — the committed daemon:
  ingress `inbound_loop:209` / `publish_inbound:174`, egress `outbound_loop:130`,
  token read `_config:54`/`:73`, offset ack `:227`, bot-echo skip `:234`, park `:88`.
- `src/exec.rs` — `reply_source:2039` (the render stamp; the routing twin M1 adds),
  its call sites `:2234`/`:2298`, `OutboundMessage:985` (correlation threading),
  and the `in/`-plane / owner-mailbox refusal `:2135-2157` (why promotion must be
  trusted).
- `src/dispatcher.rs:413-545` — the daemon-spawn seam: token mint `:452`, env
  injection `:495-521` (the M3 hook — note config keys are **not** injected today),
  and `matching_exec_handlers` ingress routing `:1719` (in `dispatch_pending:1707`).
- `src/provider.rs` — the vault M3 reuses: `Credential:114`, XChaCha20 `:26`/`:510`,
  `providers` schema `:553`, `add:599`/`get:680`; CLI `src/providercli.rs:33`.
- `packages/phonebook/scripts/main` — `op_identity:212`, `op_channel:249`,
  `op_link:262`, `q_resolve:327`; `packages/recall/scripts/main` — `resolve:108`.
- `kits/stdlib/packages/comms/scripts/comms_view.py:193` — `source_for`, now in the
  comms package, prefers `payload.source` ("adding a new channel must NOT add a
  branch here"). The render tests: `src/web.rs:3459`, `src/exec.rs:3174`.
- `docs/security.md` entry 15 (reserved `in/dm/`, provenance = verified sender) and
  entry 16 (daemon-not-exec: why the bridge is a daemon, `sender = telegram`).

## Residuals / gating (be honest)

- **Webhook transport** — deferred to a future always-on hosted deployment
  (wonky-bit 1). Long-poll is the ruling for a personal machine.
- **Unprompted, pick-a-platform egress** (the EA/channel-selection policy,
  `channels.md` gap 4) — still deferred. M1 does only deterministic reply-follow to
  the platform a conversation already lives on. The journey explicitly accepts this
  ("I can live without it for now. Replying where I already am is the floor.").
- **Multi-human** — only `owner` is linked. A second human is a new phonebook
  identity + link; the promotion step generalizes to "resolve → that human's plane"
  but this handoff seeds and proves owner only.
- **Group chats** — Telegram's `chat.id` is the same shape (the bridge already
  handles it), but *who is the owner in a group* and recall's multi-party resolution
  (`security.md` entry 15 close: a group is a `kind:group` identity) are deferred;
  M2 authenticates 1:1 owner DMs.
- **Media / attachments** — text-only (`msg.get("text")`, `scripts/main:189`).
  Photos/files/voice are a follow-up.
- **Auto-linking an unknown chat id to a human** — deferred; linking is
  owner-vouched only (M2 seeds it once). A "which human is this new chat" auto-linker
  is identity-model work, not this handoff.
- **Live Telegram** needs a real BotFather token; CI acceptance uses the stub/
  recorded transport, the posture the bridge already documents for itself.

## Log

- 2026-07-13 — Planner drafted from main, grounding every anchor in code read
  today. Key correction to the naive framing: the bridge is **already built and
  committed** (`eeaf712`); this handoff is the loop-closer, not a rebuild. Four
  verified gaps drive the milestones: (1) no `in/human/<owner>` → `in/package/telegram/send`
  forwarder exists (grep-confirmed) so replies never reach the phone; (2) no
  chat-id→owner authentication and no fence on unknown senders; (3) the daemon-spawn
  seam (`dispatcher.rs:495-521`) **never materializes package config keys into the
  child env** — so the token today only arrives via inherited supervisor env or a
  plaintext config file, making the vault milestone close an *absent wiring* as well
  as a plaintext exposure; (4) no end-to-end proof or operator on-ramp. Outbound
  transport ruled long-poll (no public endpoint for a laptop). Reply routing ruled
  deterministic correlation-follow, distinct from the still-deferred EA policy.
  Depends on A+B (both done), continues C (done).
- 2026-07-13 — M1-M4 IMPLEMENTED (planner-driven, 3 sonnet impl workers + 1 e2e
  worker + 1 opus verifier + 1 fix round; left unstaged for Fable). Shapes as
  ruled: M1 = a third loop (`reply_forward_loop`) in the telegram daemon —
  stateless correlation-follow, chat id re-derived read-only from the ledger,
  loop-avoidance discriminator (skips sender ∈ {telegram, dm-promoter} and
  bodies carrying `prompt`/`promoted`), plaintext `_config("token")` fallback
  deleted. M2 = NEW `packages/dm-promoter/` (wonky-bit 5's "trusted actor"
  landed as its own broker-trusted daemon, not inside phonebook/recall):
  subscribes `in/dm/telegram/#`, resolves via phonebook HTTP, promotes onto
  `in/human/owner` `{prompt, source:"telegram", chat_id, promoted:true,
  session:"tg-<id>"}` on the ingress correlation ONLY for resolved==owner;
  anything else — including any resolve error — publishes nothing (fail-closed;
  opus verifier probed 13 adversarial resolution shapes, all rejected). M3 =
  `package_secrets` vault table reusing seal/open, `provider set-secret`
  (stdin-only) / `list-secrets` (REDACTED), manifest `ConfigKeyDecl.secret`,
  spawn-seam `secret_env_for` injecting the decrypted token transiently into
  the child env; security.md entry 26; `packages check` is vault-aware for
  secret keys. M4 = e2e section 20b rewritten to drive the whole loop with the
  stub transport (vault token, no-plaintext-at-rest asserts, unknown-sender
  fail-closed case, owner promotion, AUTOMATIC reply-forward round trip).
  Verification found + the loop fixed: (HIGH) telegram's manifest lacked the
  `in/package/telegram/send` self-publish grant — the broker NACKed the
  forward, replies silently dropped; (real bugs caught by the e2e worker)
  dm-promoter shipped without its exec bit, and ingress published with a NULL
  correlation which silently broke the whole promote→reply chain (fixed with a
  deterministic `tg-<chat_id>` ingress correlation); (MEDIUM) `packages check`
  pointed secret keys at a plaintext `config set` fix. Also repaired
  pre-existing e2e rot (hardcoded `elanus.db` vs the post-rename `lanius.db`).
  Final: cargo test 663/0, full e2e 253 ok / 0 fail. RESIDUAL (new, needs a
  decision): recall unification does NOT fire for promoted turns — recall keys
  the correspondent off the `in/dm/<kind>/<addr>` topic, which promotion
  discards; a promoted `in/human/owner` turn gets no cross-channel history.
  Candidate fix: recall trusts `payload.chat_id`+`source` only when the
  broker-verified sender is `dm-promoter`. Documented in the e2e notes; NOT
  patched. Still pending from the milestone list: the one-time live BotFather
  validation from a phone (M4's manual step — operator on-ramp is written in
  packages/telegram/SKILL.md).

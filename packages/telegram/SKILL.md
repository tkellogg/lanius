---
name: telegram
description: Telegram bridge (daemon) — getUpdates messages become in/dm/telegram/<chat.id> conversation ingress; in/package/telegram/send delivers replies with sender=telegram receipts. A transport is just a package (zero kernel edits).
---

# telegram

A two-way external-channel **daemon** — the payoff of the "a transport is just a
package" design (docs/channels.md). It edits **zero kernel files**: it rides the
built-in `in/dm/<kind>/<addr>` grammar (Handoff B) and the already-built
`phonebook` + `recall` packages to become a live Telegram relay. Paired with
`dm-promoter` (the fail-closed security gate), an inbound message from the
*linked owner* becomes a real agent turn, and the agent's reply is relayed back
to the phone automatically.

## Setup (operator on-ramp)

1. **Get a bot token from BotFather.** Message `@BotFather` on Telegram,
   `/newbot`, follow the prompts — you get back a token that looks like
   `123456:ABC-...`.

2. **Store the token as a vault secret — never plaintext, never argv/history.**
   The plaintext config-key fallback has been removed; the token lives ONLY in
   the encrypted `package_secrets` vault and is injected into the daemon's
   environment at spawn. Pipe it in on stdin, never pass it as an argument and
   never leave it in a config file:
   ```sh
   read -s TOKEN   # prompts, doesn't echo, doesn't touch shell history
   printf '%s' "$TOKEN" | lanius provider set-secret telegram TELEGRAM_TOKEN
   unset TOKEN
   ```
   (Pulling it from a password manager's CLI into the pipe — e.g.
   `op read op://.../token | lanius provider set-secret telegram TELEGRAM_TOKEN`
   — works the same way and keeps it out of your own shell history too.)

3. **Approve the package chain.** Telegram depends on `phonebook` + `recall`
   (identity + cross-channel unification); the promotion gate depends on
   `phonebook` too:
   ```sh
   lanius approve phonebook
   lanius approve recall
   lanius approve telegram
   lanius approve dm-promoter
   ```
   Each ships PENDING (it can reach the outside world / write owner mail) and
   parks until approved. `elanus packages check` reports telegram invalid
   until its declared deps are approved AND `TELEGRAM_TOKEN` is set, each
   paired with its exact fix.

4. **Message the bot once, then find and link your chat id.** Send the bot any
   message on Telegram. The bridge records an UNRESOLVED phonebook sighting for
   that chat — nobody is promoted to owner mail yet (dm-promoter is
   fail-closed by design). Find the chat id from that sighting:
   ```sh
   curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"channels","resolved":false}'
   ```
   (`$PBPORT` is the port in `run/pkg-phonebook/http.json`.) Then seed your
   owner identity and vouch that this Telegram chat is you — the same identity
   as your primary `elanus` channel, so recall unifies your history across
   both:
   ```sh
   lanius bus pub in/package/phonebook/identity '{"id":"owner","kind":"human","canonical":"Tim"}' --qos 1
   lanius bus pub in/package/phonebook/link '{"channel_kind":"elanus","address":"owner","identity":"owner","confidence":1.0}' --qos 1
   lanius bus pub in/package/phonebook/link '{"channel_kind":"telegram","address":"<your chat id>","identity":"owner","confidence":1.0}' --qos 1
   ```

5. **What you should now observe.** Message the bot again: dm-promoter
   resolves the chat to `owner` and promotes the message onto `in/human/owner`
   — a real agent turn runs, its context includes your history from every
   linked channel, and its reply is automatically forwarded back to the same
   Telegram chat (the reply-forwarder derives the chat id from the ledger; no
   command from the agent is needed to route it). A stranger who finds the bot
   and messages it gets an unresolved sighting and nothing more — no agent
   turn, no owner mail, ever, until you explicitly link them.

For CI/offline work, point `TELEGRAM_API_BASE` at a stub/recorded Bot API — no
live token needed (see `tests/e2e.sh` section 20b for the exact stub + drive
pattern).

## Flow

- **Ingress:** the daemon long-polls `getUpdates` holding the bot token; each
  message → **one** `in/dm/telegram/<chat.id>` publish (the canonical,
  recall-keyable conversation address; the chat id *is* the address, 1:1 and
  group alike). Any handler subscribing `in/dm/#` (or `in/dm/telegram/#`)
  receives it. The `in/dm/telegram/#` publish grant IS the ingress-bridge
  capability the broker ACL requires — only an owner-approved manifest may hold
  a dm-scoped grant, so a prompt-injected agent cannot forge ingress. On each
  inbound the bridge also records a phonebook channel **sighting**
  (`in/package/phonebook/channel {channel_kind:"telegram", address:<chat.id>}`),
  provenance = the bridge's broker-verified sender.
- **Promotion (dm-promoter):** a separate, narrowly-scoped daemon resolves the
  sighted chat id against the phonebook and, ONLY when it resolves to identity
  `owner`, republishes the message onto `in/human/owner` (same correlation id)
  — the one thing that turns "a message arrived on this wire" into "this is
  the owner talking." Fail-closed: any failure (phonebook down, unlinked,
  unknown, malformed) produces no publish at all.
- **Reply-forward:** the bridge also subscribes `in/human/owner`. When an
  agent's reply lands there (not a promoted/inbound message — it distinguishes
  by sender and by the `promoted`/`prompt` fields dm-promoter stamps), it
  derives the Telegram chat id from the ledger-correlated ingress event
  (read-only sqlite query) and publishes `in/package/telegram/send`, closing
  the loop with no hand-off command required from the agent.
- **Egress:** an agent (or the reply-forwarder) commands a send by publishing
  `in/package/telegram/send {recipient, text}`; the bridge calls the Bot API
  `sendMessage` directly (off the bus) and publishes an
  `obs/channel/telegram/sent` receipt **stamped `sender = telegram`** (the
  daemon authenticates as itself — docs/security.md entry 16).

It is a **daemon, not an exec handler** (entry 16): only a caged, token-authed
daemon gets `sender = telegram` on its records; an uncaged exec handler would
authenticate as the owner and mislabel every send. Unconfigured (no vault
secret set), it parks instead of crash-looping.

## Security posture

The bot token is an **encrypted vault secret** (`package_secrets`, sealed with
the same XChaCha20 vault crypto as an API-key provider), never a plaintext
config value — the old `config/packages/telegram.toml` token fallback has been
removed entirely. It is injected into the daemon's own environment only at
spawn time, by the supervisor, from the declared `secret = true` manifest key;
no agent, no config file, and no ledger row ever holds it in the clear.

Unification (the "load-test the phonebook" payoff): once the owner is seeded and
the Telegram chat linked to identity `owner`, `recall` resolves an inbound
Telegram message to `owner` and pulls his cross-channel history (elanus +
telegram) into the frame — the same person, seamless across platforms.

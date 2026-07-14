---
name: dm-promoter
description: Resolve-and-promote security gate — verifies a Telegram (or other in/dm/*) chat id against the phonebook and, only when it resolves to owner, promotes it onto in/human/owner. Fail-closed; a stranger's message stops at an unresolved sighting.
---

# dm-promoter

The security spine of "chat from anywhere" (docs/handoffs/telegram-bridge.md
M2, docs/security.md entry 15). A channel bridge (e.g. packages/telegram) only
proves a message arrived on some wire; it does not prove the sender is the
owner. This daemon is the one thing trusted to make that call and write into
`in/human/owner`, the owner's mailbox.

**What it does:** subscribes `in/dm/telegram/#`. For each inbound, asks the
phonebook (packages/phonebook, over its HTTP read plane) whether the chat id
resolves to identity `owner`. If, and only if, it does, it re-publishes the
message onto `in/human/owner` (retaining the ingress correlation id, so the
owner's agent turn and its eventual reply thread back exactly like any other
owner conversation) with `source:"telegram"`, `chat_id`, `promoted:true`, and
`prompt` carrying the text. If the chat id is unlinked, unknown, or the
phonebook can't be reached, it does nothing — the message stays an unresolved
phonebook sighting (already recorded by the bridge) and never becomes owner
mail.

**Security posture:** fail-closed and broker-trusted. Its authority to write
`in/human/owner` is its broker-verified `sender` (`dm-promoter`, stamped
because it is spawned token-authed under its own manifest grant) plus that
manifest's narrow `publish = ["in/human/owner"]` — never a payload field, and
never something an agent can do itself (agents hold no grant on
`in/human/#`). Any failure degrades to "not owner"; nothing promotes on an
error or a guess.

**Depends on:** `phonebook` (approved, reachable — resolution decisions come
from there) and an owner link already seeded there
(`in/package/phonebook/identity {id:"owner",...}` +
`in/package/phonebook/link {channel_kind:"telegram", address:<chat.id>,
identity:"owner"}`, done once, by hand or by an onboarding flow — not by this
package). Ships PENDING; approve with `lanius approve dm-promoter`.

**Profile:** the agent profile a promoted message runs under is
`DM_PROMOTER_PROFILE` (env) or this package's own `profile` config key
(`lanius config set dm-promoter profile '"main"'`), defaulting to `"main"` —
either change restarts the daemon (the supervisor watches this package's
config-repo fingerprint), so a fresh process re-reads it before the next
promotion.

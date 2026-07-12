---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus (session code-543b576e)
last-updated: 2026-07-11
---

# Handoff: unify worker-session DMs into the chat plane

A coding session that messages its owner is invisible in the web UI. Live repro
(2026-07-09): session `code-543b576e` emitted `in/human/owner` (event 5411,
correlation `code-deliver-92da…`) replying to an owner delivery. The OS
notification fired (the notifier package watches the bus and doesn't care who
the sender is), but the message appears in **no** web-UI conversation. The
owner's question — "where would it even be?" — has no good answer today.

## Why it's invisible

The conversation-list projection (relocated to the comms package,
`kits/stdlib/packages/comms/scripts/comms_view.py`) drops worker sessions in
both of its paths:

- `comms_view.py:311` (inbound seeds): *"Worker (coding-run) sessions stay in
  the trace view, not chat"* → `if is_worker_session(conn, session): continue`
- `comms_view.py:347` (ambient agent-first sends): same filter.

So agent-DM-in-the-web-UI exists **only for native agents** — exactly the gap
the 2026-07-08 channels/routing audit flagged. The trace/session view shows the
coding run's *activity*, but the human-facing DM exchange (owner delivers a
message, worker replies on the correlation) never assembles into a thread
anywhere.

This is also the reference violation of the simple-core law
(`docs/channels.md`, closing section): `is_worker_session` /
`WORKER_PREFIX = "code-"` hard-code the channel taxonomy into the projection.
Adding or reclassifying a channel edits code, not a descriptor.

## Goal

A coding session's exchange with the owner threads as a first-class
conversation in the web UI, like any agent DM — and the projection stops
learning channel names from string prefixes.

**Explicitly NOT in scope — the security core.** The `code-` prefix is
load-bearing in the broker ACL (`broker.rs:440`); worker authority, grants, and
mailbox scoping do not change here. This is a **read-model + UI** unification.
If a milestone finds itself editing broker/ACL code, stop — wrong seam.

## What a worker conversation IS (and isn't)

The thread is the **DM exchange only**, not the coding trace:

- owner → worker: `code-deliver` events (`lanius code deliver`, web
  `/api/code/deliver`, correlation `code-deliver-<uuid>`)
- worker → owner: `in/human/<owner>` events whose broker-verified sender is
  that worker session (ambient), or whose correlation joins a delivery.

Tool calls, file edits, sub-worker spawns stay in the trace/session view. The
session detail page and the chat thread should cross-link (chat header → trace
view; trace view → conversation panel), not duplicate each other.

## Milestones

### M0 — a first-class worker→owner send verb

Today a coding session has no way to message its owner except hand-rolling
`lanius emit` and guessing at the payload shape (a real fumble happened
2026-07-09). Native agents get `send_message`/`ask_human` as exec built-ins
gated on the comms-etiquette package; coding sessions get nothing. Close the
gap:

- **`lanius code send "<message>"`** — non-blocking, always addressed to the
  owner. New arm in the `lanius code` verb match (`src/main.rs`, next to
  `deliver` ~1663), handler in `src/codeagent.rs`.
- **Identity is derived, never claimed**: read `LANIUS_CODE_SESSION`
  (`codeagent::ENV_SESSION`) exactly as `deliver` does; bail cleanly without
  it. A session can only speak as itself.
- Emit `in/human/<owner>` (owner from `secrets::owner_name`) via
  `events::emit` with explicit `sender = <session>` — the same
  ledger-write path `record_delivery` uses; the session's emit-only bus token
  is never used or widened. Zero broker/ACL surface.
- Payload: `{"text": <msg>, "session": <session>, "source": "code"}` — the
  shape the projection reads, with the M3 source stamp from day one.
  `source` is a stable machine token (a fact), not display text; the UI owns
  the pretty label.
- Optional `--corr <id>`: a worker replying to a delivery threads on that
  delivery's `code-deliver-*` correlation; bare `send` is an ambient new
  thread.
- Structure as an env-free testable core (env-reading `send` wraps a
  `record_send(root, sender, message, corr)`), mirroring
  `deliver`/`record_delivery`.
- **Teach it**: a `lanius code send` bullet in the comms-etiquette SKILL.md
  "Coding-session dispatch" list (`kits/core/packages/comms-etiquette/`,
  the coding-session cheatsheet — the one kit file documenting
  `lanius code deliver`), plus a short "from a coding session" note in that
  SKILL.md's "Talking to the human" section, plus the `codeagent` printed
  help.
- **Deferred: a blocking human-ask.** `lanius code ask <session>` already
  exists for sibling-asks; the future verb for asking the HUMAN is named
  **`lanius code ask-human`** (no collision) but requires the
  checkpoint-and-exit suspend machinery coding sessions don't have. Out of
  scope; leave a code comment at the `send` handler naming it.

### M1 — projection: worker sessions become conversations

In `comms_view.py`, replace the two `continue`s with a fold that builds a
conversation from the DM exchange:

- Seed from `code-deliver` traffic and from `in/human/<owner>` rows whose
  sender is the worker session (the ambient path already requires
  broker-verified `sender == agent`; reuse that discipline — sender is
  broker-verified, never payload-claimed).
- Correlation join: a `code-deliver-*` correlation groups the owner's delivery
  and the worker's reply into one thread, same as the existing prompted-thread
  join.
- The conversation row carries the honest source token `"code"` and the
  session's note/title where one exists (`lanius code note`). The chip label
  ("coding session") is presentation: the UI keeps a tiny token→label map in
  ONE place, which must not grow branches beyond label lookup.
- **List placement — DECIDED (Tim, 2026-07-09): converse pane, same
  conversation list as agent DMs, distinguishable by chip — NOT a separate
  silo** (a silo recreates the taxonomy split one level up). Concretely:
  worker DM threads surface under the coding tool-noun's (`claude-code` /
  `codex`) converse bucket — `App.tsx` stops skipping `loadConversations`
  for worker nouns (~line 552) — same list shape, chip, and a cross-link to
  the trace/session view. Minimal change against the existing per-agent
  machinery; Tim validates the intermix at demo.

### M2 — reply-from-chat routes to the worker

Replying inside a worker conversation must route via the deliver path
(`/api/code/deliver`, which the walkthrough sprint already built), not the
native-agent exec path. The UI seam exists (`ui/web/src/lib/conversation.ts`
knows deliver is a worker-only affordance); wire it so one compose box does the
right thing per conversation kind, driven by data the projection returns — not
by the client re-deriving "is this a worker" from the session id.

Note (verified 2026-07-09): the server half is already DONE, not a stub —
`web.rs code_deliver` is fully implemented (human-proof gate, `code-*` shape
validation, relay via `cli_deliver`). And it routes with the **owner as
requester**, so `delivery_requester` deliberately routes NO reply back — a
chat reply is a "say something" note, not a peer round-trip; the worker's
answer reaches the owner via its own `in/human/<owner>` emission (M0/M1), not
via completion routing. Don't wire a phantom reply route. M2 is therefore UI
wiring + round-trip verification only.

Tie-in: `inbox-provenance` has MERGED to main (2026-07-09, commits `1523ba9`,
`c7a85eb`) — delivered messages render inside the worker's context fenced,
full-verbatim, with harness-asserted provenance. A chat-sent reply must arrive
through that merged rendering. Verify the round-trip end-to-end: web chat →
deliver → worker inbox → worker reply (`lanius code send --corr …`) → web
chat.

### M3 — kill the taxonomy special-case (the simple-core payoff)

Move the hard-coded channel taxonomy out of the projection:

- `source_for` already prefers an explicit stamped `payload.source`; make the
  worker emit paths stamp `"source": "code"` (M0's `send`, `codeagent.rs`
  completion/reply emission) so the `code-` display fallback becomes dead
  weight and is removed.
- The projection's worker fold keys on the **broker-verified `sender`
  column**, never on a string prefix and never on `payload.session`.
- `is_worker_session` survives only where it reflects a real durable fact (the
  stored `kind` in `code_sessions`), not as a display-routing switch. The two
  drop-`continue`s die.
- **Deferred (honest note)**: relocating the remaining fallback taxonomy
  (`web-`, github/jira/linear, cron) into channel descriptors per
  `docs/channels.md` ("the kernel should never learn a new channel's name";
  `withheld_builtin_tools` in `src/packages.rs` is the pattern done right).
  Those channels are not packages yet, so their `source_for` fallbacks stay
  as the existing shrinking-net TODO — building a descriptor registry with no
  second consumer would add a parallel taxonomy, not remove one. Clause 3
  reads as "no prefix-based display *routing* remains" (ruled 2026-07-09).

### M4 — tests + e2e

- Python-side: projection tests for the worker-conversation fold, the
  correlation join, and the sender-verification rule (a payload claiming
  `session: code-x` from a non-matching sender must NOT create/join a thread —
  that's a spoof vector).
- `ui.spec.mjs`: a worker DM thread renders; reply-from-chat delivers; the
  chip and cross-links are present.
- `cargo test` for the Rust-side seams: the M0 env-free `record_send` core
  (identity, payload shape + `"source":"code"` stamp, empty-message bail,
  correlation), completion-reply stamping, conversation-detail reads in
  `web.rs`.
- Build ritual: `npm run build` → cargo build (build.rs embed-freshness now
  handles staleness) → run e2e against the Rust server.

## Acceptance

1. From a coding session: `lanius code send "<msg>"` (or a correlated reply
   to a deliver via `--corr`) → the message appears in a web-UI conversation,
   live over SSE, attributed to that session with an honest chip. Attribution
   comes from the broker-verified sender, never the payload claim.
2. Replying in that conversation lands in the worker's inbox with full
   provenance rendering; the worker's next reply threads back. One
   conversation, both directions.
3. `grep -n 'WORKER_PREFIX\|is_worker_session' kits/stdlib/packages/comms/` —
   no display-routing use remains (durable-`kind` fact reads may survive);
   the `code-` display fallback in `source_for` is gone.
4. Broker ACL (`broker.rs`) untouched — diff shows zero security-core changes.
5. Full `cargo test` + `ui.spec.mjs` green; report counts.

### M5 — sender-verify the single-thread DETAIL feed (blocker)

M0–M4 landed, but the e2e worker surfaced a real hole the M-work never touched:
the **conversation-LIST** projection (`comms_view.py conversation_rows`) is
sender-verified against the spoof, but the **single-thread DETAIL feed** in the
core web server is not. Opening a worker thread reconstructs its feed via
`web.rs conversation_messages` (`src/web.rs:2724`), whose ambient fold calls
`query_human_by_session` (`src/web.rs:2862`). That helper matches `in/human/%`
rows by a **payload** `LIKE '%"session":"<id>"%'` and **never consults the
broker-verified `sender` column** (`src/web.rs:2867-2872`). So any writer to the
owner mailbox can put `"session":"code-<victim>"` in its payload and its message
renders inside that worker's opened thread — the exact spoof the list projection
already rejects.

Root cause, confirmed by reading the code: `EventRow` **dropped its `sender`
field** when the list projection moved to the comms package
(`src/web.rs:2580-2588` — the comment says "the surviving in-core
conversation-detail readers don't use it"), and `map_event` (`src/web.rs:2600`)
therefore never reads column 5. The detail feed cannot sender-verify until the
column is carried again. Every `SELECT` feeding `map_event` already lists
`sender` at column index 5 (`src/web.rs:2599`), so no query text changes.

**The rule to mirror — do NOT invent a new one.** The list projection's spoof
guard is `comms_view.py:436`:
`if session != row["sender"] and is_worker_session(conn, session): continue`.
Read literally: a payload `session:` claim is honored only when the claimed
session is *not* a worker, OR the broker-verified sender equals that session (a
worker speaks only as itself). Mirror exactly this in the detail feed.

The Rust classifier already exists: `codesession::is_worker_session(&conn, session)`
(`src/codesession.rs:2096`) — durable-`kind` read with a `code-` prefix
fallback, the same logic `comms_view.is_worker_session` ports. Use it; do not
reimplement.

**Secondary gap (in scope — decided).** The detail feed also does not fold the
owner's **delivery prompt** into the opened worker thread. The owner→worker
delivery lands on topic `in/agent/<noun>/<session>` with a payload carrying
`prompt` and **no** `payload.session` (see the e2e seed,
`ui/web/test/ui.spec.mjs:1280`). The existing in/agent pass matches by
`payload LIKE '%"session"…'` (`src/web.rs:2783-2791`) and `session_for_event`
(`src/web.rs:2795`), so it misses the delivery entirely — an opened worker
thread shows the worker's reply with **no owner prompt above it**, an incoherent
half-conversation that contradicts this handoff's "one conversation, both
directions" goal. In scope because it is the *same seam*, small, and the list
already seeds it (`comms_view.py:374-408`: match the delivery by **topic
segment**, fold `prompt`/`text` as the owner's "you" turn). Why bounded: mirror
`comms_view`'s delivery pass semantics — a `code-deliver`-shaped topic
`in/agent/<noun>/<session>` whose **raw trailing segment equals `session`**
(reject a deeper `/`-bearing topic, `comms_view.py:396`), gated on
`is_worker_session(&conn, session)` so native threads are untouched. Fold the
payload prompt as a `who:"you"` turn exactly like the existing in/agent prompt
fold (`src/web.rs:2809-2822`); `add_message`'s content-key dedup collapses any
overlap. Recommended: also push the delivery row's `correlation_id` into `corrs`
so a correlated reply that omits `payload.session` still threads via the
existing correlation join (`src/web.rs:2826-2831`).

**Files in scope:** `src/web.rs` ONLY (the fix + a Rust unit test in the
existing `#[cfg(test)] mod tests`). Optionally `ui/web/test/ui.spec.mjs` — but a
Rust unit test is preferred and sufficient (the e2e flow already asserts the
LIST defense at `ui.spec.mjs:1392-1394` and documents the detail-feed gap at
`ui.spec.mjs:1375-1380`; that comment should be updated to reflect the closed
gap if e2e is touched). **`src/broker.rs` MUST NOT be touched — any diff there is
an automatic FAIL** (this is a read-model change; broker/ACL is the wrong seam,
per the handoff's scope fence).

**Implementation shape (minimal, mirror the list — no refactor):**

1. Re-add `sender: Option<String>` to `EventRow` (`src/web.rs:2580`) and read
   `col_string(row, 5)` in `map_event` (`src/web.rs:2600`). Load-bearing.
2. In `conversation_messages`, sender-verify the ambient fold: when folding a
   `query_human_by_session` row, skip it if
   `is_worker_session(&conn, session) && row.sender.as_deref() != Some(session)`.
   (Equivalent to filtering inside `query_human_by_session`, which already has
   `conn` and `session`.) Leave the correlation-join path
   (`query_human_by_corr`) unchanged — the list mirror does not sender-check it
   either (`comms_view.py:467-487`); do not invent a stricter rule.
3. Add the delivery-prompt fold (secondary gap) as specified above.

**Acceptance (M5):**

a. **Sender-verified detail feed.** A worker thread's `conversation_messages`
   feed folds an ambient `in/human/<owner>` row claiming that `code-*` session
   **only** when the broker-verified `sender` equals the session. A row with the
   same `payload.session` but a **foreign sender** (e.g. `eve`) does NOT appear
   in the feed. The worker's own reply (sender == session) still renders.
b. **Non-worker threads unchanged.** For a non-worker session (`evt-*`, a native
   agent-noun run session), the ambient fold behaves exactly as before — no
   sender gate applied (regression-guard the existing `run-amb-1` feed test,
   `src/web.rs:3196`).
c. **Delivery-prompt fold.** Opening a worker thread whose owner delivered on
   `in/agent/<noun>/<session>` renders the owner's delivered prompt as a "you"
   turn *and* the worker's reply — a coherent two-sided thread.
d. **Green + untouched core.** Full `cargo test` passes; `ui.spec.mjs` passes
   (415/0 baseline, or higher if a detail-feed assertion is added). `git diff
   --stat src/broker.rs` is **empty**.

Note the Rust `is_worker_session` prefix fallback means a `code-*` session with
no `code_sessions` row is still classified a worker — so a unit test can seed a
spoof purely from the events table (via the existing `insert_event` test helper,
`sender` at param 5) without standing up a `code_sessions` record.

## Context for the implementer

- `docs/channels.md` — the conversation model + the closing "principle, made
  concrete" section this handoff executes.
- 2026-07-08 audit conclusion: no transport concept needed; a bridge is a
  package on the topic protocol. This handoff makes the web UI itself behave
  like just another channel consumer.
- Downstream benefactor: the planned Telegram/Signal bridge — once worker DMs
  thread uniformly, "attach lanius to Signal so an agent can DM me"
  (`_questions.md`) is the same projection with a different egress package.

## Log

- 2026-07-09 (planner, session c185ae6f): status → in-progress. Amendments
  ruled by Tim/Fable: added M0 (`lanius code send`, ask-verb deferred as
  `ask-human`); source stamp is the machine token `"code"` (UI owns the
  label); M1 placement decided — converse pane under the tool-noun bucket,
  chip, no silo; M3 scoped to stamp-at-emit + sender-keyed fold, descriptor
  registry deferred; inbox-provenance noted MERGED (`1523ba9`, `c7a85eb`);
  recorded that `/api/code/deliver` routes owner-as-requester with no reply
  route (by design). Fixture-shaped live repro also exists at event 5476
  (session `code-79985d39`, correlation `code-dm-test-79985d39`).

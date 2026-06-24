---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-24
---

# Handoff: the human's seat for inter-agent comms (+ memory blocks, estimation)

Three handoffs just landed on `main` — agent-comms (`d4c4646`), memory-blocks
(`4cb6bb7`/`6e06dd9`), work-estimation (`fbba3bc`) — and they are almost
entirely **agent-facing**: CLI verbs (`elanus code deliver/inbox`, `elanus
block …`, `elanus estimate …`) plus per-turn context injection the *model*
reads. A human running elanus today **cannot see any of it**: not the
agent-to-agent message traffic, not who is delivering to whom, not a message's
priority/urgency, not the shared-channel/room conversations, not an agent's
memory blocks, and not the estimate-vs-actual of a run.

This handoff makes that traffic legible from the **human's seat** — the web UI.
It is the comms/blocks counterpart to
[coding-agent-observability.md](coding-agent-observability.md) (which made the
*run tree* legible) and [chat-conversations.md](chat-conversations.md) (the
human↔agent chat seat). Those two meet at the nav split: AGENTS/conversations
vs WORKERS/runs. This one adds the **third leg — the cross-agent comms plane**:
what the agents are saying to *each other*.

The good news the survey turned up: most of this data is **already on the
ledger and already streamable**. Agent-to-agent deliveries are `emit()`ed
kernel events on `in/agent/<noun>/<session>` topics carrying `correlation_id`,
`sender`, `priority`, and `state` (`src/codeagent.rs::record_delivery` ~1131);
room traffic is `in/group/<id>` events; both already flow over `/api/stream`
and persist to `elanus.db`. So the comms milestones are mostly **a projection +
a view**, not a new capture path. Blocks and estimation need a thin read route
each.

## What exists today (grounding — real views, routes, anchors)

**The SPA** (`ui/web/src/App.tsx`) has these views, switched by a `sel` union:

- `welcome` (`WelcomeView` ~1317), `setup` (`SetupView` ~1350), per-agent
  `converse`/`sessions`/`telemetry`/`configure` tabs (the `agent-tabs` bar,
  `App.tsx:1144`), and a left `Nav` (~1245) with `signals`, `setup`, **`runs`**
  (`data-sel="code-sessions"`, ~1276), and per-agent entries.
- **`runs`** is `CodeSessions` (`ui/web/src/CodeSessions.tsx`), rendered when
  `sel.kind === 'code-sessions'` (`App.tsx:1203`). It is the nested
  spawner→worker tree with a detail panel (stats, paste-able resume command,
  event timeline). It reads `GET /api/code/sessions` + `/api/code/sessions/{id}`
  and folds the live `/api/stream` tail (`foldLive`/`mergeLive`). **This is where
  estimation belongs** (M5) and where comms cross-references a run.
- **`signals`/`telemetry`** is `RailView` (`App.tsx:1967`): the raw live activity
  stream over `/api/stream`, with filter buttons `data-f={all|work|tools|signals}`
  and a `v-in`/`v-obs`/`v-tool`/`v-signal` class per topic. It already *shows*
  `in/agent/…` deliveries as raw rows — but flat, unthreaded, untyped, with no
  notion of "this is mail from A to B" or its priority. **This is the substrate
  the comms view refines.**

**The web API** (`src/web.rs`, routes registered ~189-201):

- `GET /api/stream` — the live SSE bus tail (every topic; `seq`-stamped).
- `GET /api/status` — health/paths (~343).
- `GET /api/conversations` + `/api/conversations/{session}` — the **human-chat**
  sqlite projection (~402/423), NOT agent↔agent mail.
- `GET /api/code/sessions` + `/api/code/sessions/{id}` — the run projection
  (shells `elanus code sessions/session --json`, ~456/473).
- `GET|POST /api/history` — proxies the history package; kinds
  `agents|sessions|transcript|conversation|search` (~517). `conversation`
  threads `in/agent`/`in/human` by correlation — but only for the human-chat seat.
- `/api/admin/{tail}*` (~541) — the privileged shell-out table
  (`admin_dispatch`, ~573): `models`, `agents`, `kits`, `packages`, `configs`,
  `proposals`, `path-check`, `profile`. **Every admin verb is `cli(root, …)` →
  this binary.** A new read route that shells `elanus block list --json` /
  `elanus estimate actual --json` fits this exact pattern with no new transport.

**The backend data the agents just gained (all real, all queryable):**

- Deliveries: `record_delivery` emits an `in/agent/<noun>/<session>` event with
  `correlation`, `sender`, `priority`. Failure-mail rides the same correlation
  with `{failed:true}` (`src/exec.rs::report_agent_failure`).
- Inbox: `codesession::inbox_for_session` (own-mailbox-only, returns
  `event_id/message/from/correlation/state/created_at/seen/priority`).
- Rooms/channels: `room_recent` (`in/group/<id>` recent-N), `peer_claims`,
  `live_siblings` (`src/codesession.rs`); membership is `code_room_members`.
- Mid-cycle: `take_pending_mid_cycle_mail` (HIGH-priority, threshold from
  `agent-comms.high_priority_threshold`) + the `code_mail_delivered`/
  `code_block_delivered` dedup tables.
- Blocks: `elanus block list --json` already prints one JSON line per block
  (`name/owner/scope/placement/priority/content`, `src/blockcli.rs:132`).
- Estimation: `elanus estimate actual` yields a `Report` (dollars/turns/tokens/
  wall_clock variance + `dollars_unavailable`, `src/estimate.rs`); `obs/estimate/
  <session>` carries the boundary event.

## The human-facing gap (named)

1. **No agent-to-agent traffic view.** The deliveries exist as `in/agent/…`
   events but the only place a human sees them is `RailView` — as flat, raw
   topic rows, not as "A delivered to B (priority N, pending/done/failed)".
   Nothing shows the deliver→complete/fail round trip threaded by correlation.
2. **No who-delivers-to-whom / priority surface.** `events.priority` and the
   `{failed:true}` failure-mail are invisible. A human can't tell an urgent mail
   from a routine handoff, or spot a silent worker failure, without `tail`-ing a
   log.
3. **No room/shared-channel visibility.** `in/group/<id>` traffic, room rosters
   (`live_siblings`), and edit-claims (`peer_claims`) have no UI at all.
4. **No mid-cycle-injection signal.** When elanus injects urgent mail/blocks
   *between an agent's tool calls* (the load-bearing C3/M4 feature), there is no
   human-visible mark that it happened.
5. **No memory-block inspector.** An agent's durable identity/learned prompt
   blocks (the thing that *evolves*) and the ephemeral `inbox`/`channel:` blocks
   are unseeable and uneditable from the UI.
6. **No estimate-vs-actual.** The `runs` view shows tokens/duration but never the
   estimate the agent committed to or its variance — the whole point of E1-E3.

## Decisions to confirm (the wonky bits)

1. **Comms view = projection over existing ledger events, NOT a new bus
   capture.** Deliveries/room msgs are already `in/agent/*` / `in/group/*`
   events with correlation+priority+sender. The right move is a read route that
   queries `elanus.db` (or proxies the history package's `conversation` kind,
   already correlation-threaded) and a live overlay off `/api/stream` — exactly
   the `CodeSessions` backfill+fold pattern. **Do not add a new emit path.**
   Confirm: reuse `/api/history` (add an `agent-mail` kind to the history
   package) vs a dedicated `/api/comms` route in `src/web.rs`. Recommendation: a
   thin `/api/comms` shelling `elanus code mail --json` (a new CLI verb), because
   the history package is optional/parked and comms should work without it —
   mirroring how `/api/code/sessions` shells the CLI rather than depending on the
   history proxy.
2. **Inbox/channel blocks are ephemeral (never persisted).** `inbox_block` and
   `channel_block` are computed each turn and returned as `LoadedBlock`s that are
   *not* written to `context_blocks` (`src/codeagent.rs` ~2329/2357). So a "show
   this agent's blocks" route can read the **durable** blocks straight from the
   table, but to show the live inbox/channel state it must **recompute** them via
   a new route that calls `inbox_for_session`/`room_recent`. Confirm we render
   durable + recomputed-ephemeral in one panel, clearly labeled.
3. **Block editing is a real mutation — gate it.** `elanus block set` writes
   `context_blocks` under an `owner` label. A UI editor must go through the
   `origin_ok` CSRF guard (like every `/api/admin` POST) and stamp a `--by ui`
   decided-by trail. Confirm M4 ships **read-only** first, edit as a follow-on.
4. **Estimation has no dollars source yet.** `pricing.toml` is package-local and
   may be empty; `Report.dollars_unavailable` is the honest signal. The UI must
   render "—/unknown" for dollars when unavailable (matching the setup view's
   existing "dollar estimates are not shown until pricing is known" stance,
   `App.tsx:1428`), never a fabricated number. Confirm the UI leads with the
   non-dollar dims (turns/tokens/wall-clock) and treats dollars as best-effort.
5. **Identity of "an agent's blocks."** Coding-agent blocks are keyed by
   `(agent_noun, session)` via `load_session_blocks`; native/profile blocks are
   keyed by profile. The block inspector must take the same key the run/agent it
   is opened from carries. Confirm the inspector is reached **from a run's detail
   panel** (session-scoped) and **from an agent's configure/telemetry tab**
   (agent/profile-scoped), not as a standalone global list.

## Milestones

### M1 — Agent-mail read route (the data spine for the comms view)
Add the one new read endpoint the comms surface needs. New CLI verb
`elanus code mail --json` (a thin projection over `events` where
`type LIKE 'in/agent/%'`, returning `{id, from(sender), to(noun/session from
topic), correlation, priority, state, failed, preview, ts}`, newest first,
bounded), and a `src/web.rs` route `GET /api/comms/mail` that shells it (same
shape as `code_sessions`, ~456). Threads each delivery to its
completion/failure by `correlation_id`. No new bus capture — pure ledger query.

**Acceptance:** with two sessions where A delivered to B (one normal, one
high-priority, one that failed), `GET /api/comms/mail` returns the three
deliveries with correct `from`/`to`/`priority`/`state`, the failed one flagged
`failed:true`, each threaded by correlation. A root with no mail returns `[]`,
not an error. A unit test in `src/codeagent.rs` (or a new `mailcli`) covers the
projection; an `src/web.rs` test asserts the route maps the CLI JSON through
(mirroring the `code_sessions` route tests).

### M2 — The comms / message-traffic view (the headline)
A new left-nav entry `comms` (`data-sel="comms"` in `Nav`, beside `runs` at
`App.tsx:1276`) opening a new `CommsView` component (own file, scoped `<style>`,
like `CodeSessions.tsx`). It renders the agent-to-agent traffic from M1's
`/api/comms/mail` as a **threaded list**: each row is "FROM → TO", a priority
chip (normal / **high** / signal), a state badge (pending/done/**failed**,
reusing the `StatusBadge` look), the message preview, and relative time; a row
expands to its correlation thread (the completion/failure reply). Live rows fold
in off `/api/stream` (filter `in/agent/#`), exactly the `CodeSessions`
backfill+`foldLive` pattern. Clicking a participant cross-links to that run in
the `runs` view (reuse `selectCodeSessions`/session selection).

**Acceptance:** opening `comms` lists recent agent-to-agent mail with
from/to/priority/state; a high-priority delivery shows the **high** chip; a
failed delivery shows the **failed** badge and, expanded, its `{failed:true}`
reply on the same correlation; a new delivery made while the view is open
appears live without reload. `ui.spec.mjs`: add a block that clicks
`[data-sel="comms"]`, waits for `#view-comms:not([hidden])`, and asserts at
least one `.comms-row` with a priority chip renders against seeded mail (or the
empty-state copy when none) — following the existing `data-sel`/`waitForSelector`
selector discipline.

### M3 — Rooms & shared channels (who's in the room, what's on the wire)
In `CommsView`, add a **rooms** panel: for each room with members
(`code_room_members`) surface its roster (`live_siblings` — live/stale), its
recent channel traffic (`room_recent`, `in/group/<id>`), and the active
edit-claims (`peer_claims`). Backed by extending M1's route (or a sibling
`GET /api/comms/rooms` shelling a new `elanus code rooms --json`) — again pure
ledger/`code_room_members` query, no new capture. This is the per-repo shared
channel from the journey, made visible to the human.

**Acceptance:** with two live sessions in the same workdir-room (one holding an
edit-claim) and a message posted to that room's `in/group/<id>`, the rooms panel
shows the room, both members (liveness honest), the claimed path attributed to
its holder, and the recent channel message. A solo session shows an empty/quiet
room. `ui.spec.mjs` asserts a `.comms-room` with a member count and a claim row
renders against the seeded room.

### M4 — Memory-block inspector (read-only first)
A blocks panel reachable from (a) a run's detail in `CodeSessions` (session-scoped)
and (b) an agent's `telemetry`/`configure` tab (agent/profile-scoped). Backed by
`GET /api/blocks?owner=&session=` (or `?profile=`) shelling `elanus block list
--json` (already JSON, `src/blockcli.rs:132`) for the **durable** blocks, plus the
**recomputed ephemeral** inbox/channel state (a small route calling
`inbox_for_session`/`room_recent`, decision 2) clearly labeled "live, not
stored". Renders each block's name/scope/placement/priority/content; the
`note` and `inbox`/`channel:` blocks are visually distinguished from durable
identity blocks. **Read-only in M4**; an inline editor (POST through the
`origin_ok` guard + `--by ui`, decision 3) is the documented follow-on.

**Acceptance:** opening a coding run that has a durable block and an `estimation`
block shows them with scope/priority; a session with unseen mail shows the live
`inbox` block marked ephemeral; a session with none shows "no blocks". The route
returns `[]` (not 500) for an agent with no blocks. `ui.spec.mjs` asserts the
blocks panel lists a seeded block by name with its scope label.

### M5 — Estimate-vs-actual in the runs view
In `CodeSessions` detail (`ui/web/src/CodeSessions.tsx`, the `cs-kv` stats grid
~494), add an **estimate vs actual** row group when the session has a recorded
estimate: the headline dollars (or "—/unknown" when `dollars_unavailable`,
decision 4), turns, tokens, wall-clock, each with the committed estimate, the
actual, and the signed variance (over/under). Backed by `GET /api/estimate/
{session}` shelling `elanus estimate actual --session <id> --json` (the
`Report` already exists, `src/estimate.rs`; the JSON arm is
`src/estimatecli.rs::actual_json`). A session with no estimate simply omits the group
(no crash — the report path already skips cleanly).

**Acceptance:** a finished run that recorded an estimate shows, in its detail
panel, dollars/turns/tokens/wall-clock with estimate/actual/variance and an
over/under indicator; `dollars_unavailable` renders "unknown", never a fake
number; a run with no estimate shows the existing stats unchanged with no
estimate group. `ui.spec.mjs` asserts the `cs-kv` (or a new `cs-estimate`)
section shows a variance against a seeded estimate, and is absent without one.

### M6 — Mid-cycle / priority signal mark (the algedonic tell)
Surface the moment elanus injects something *mid-cycle*. Two cheap, honest
signals: (a) in M2's comms rows, mark a delivery that was handed mid-cycle
(joinable via `code_mail_delivered`) with a "delivered mid-task" tell; (b) light
the existing global **signal lamp** (`#signal-lamp`, `App.tsx:1131`) when a
high-priority/`signal/` delivery crosses `/api/stream`, so a human watching any
view gets the algedonic cue. No new backend — the priority is already on the
event; the mid-cycle fact is in `code_mail_delivered`/`code_block_delivered`
(optionally exposed via M1's route).

**Acceptance:** a delivery at/above the high-priority threshold lights
`#signal-lamp` live (already wired to clear on click); in `comms`, a message
that was delivered mid-cycle carries the mid-task tell. `ui.spec.mjs` asserts the
signal lamp gains its `lit` class when a high-priority `in/agent` event is
streamed in (reuse the live-stream test harness).

## Honest API ledger — what's free vs what's new

| Surface | Already exposable? | New work |
|---|---|---|
| Agent→agent mail (from/to/priority/state/correlation) | Data on ledger (`in/agent/*` events); **not routed** | `elanus code mail --json` + `GET /api/comms/mail` (M1) |
| Failure-mail | On the correlation already (`{failed:true}`) | Surfaced by M1's join |
| Room roster / channel / claims | `live_siblings`/`room_recent`/`peer_claims` exist (Rust-only) | `elanus code rooms --json` + route (M3) |
| Durable blocks | `elanus block list --json` already JSON | `GET /api/blocks` thin shell (M4) |
| Ephemeral inbox/channel blocks | Computed per-turn, **never persisted** | Recompute route (M4, decision 2) |
| Estimate report | `Report` struct exists | `estimate actual --json` + `GET /api/estimate/{id}` (M5) |
| Mid-cycle fact | In `code_*_delivered` dedup tables | Join in M1 / lamp off `/api/stream` (M6) |
| Live overlay | `/api/stream` already streams all of it | Reuse `foldLive` pattern (M2/M3) |

Everything mutating reuses the `origin_ok` CSRF guard and `--by ui` trail
(`src/web.rs::admin`); every new read route follows the `code_sessions`
shell-out shape so there is no new transport, auth, or subprocess toolchain.

## Correctness / UX concerns spotted in the shipped code (note, not fix)

- **Double-channel delivery on Claude Code is intentional but invisible.** A
  high-priority block/mail is delivered BOTH mid-cycle and next-turn
  (memory-blocks Log, accepted residual #1). The human surface should not
  double-*count* it as two messages — M2 should dedup by `event_id` (the mail is
  one event) so the comms view shows one row with a "also delivered mid-task"
  tell, not two rows.
- **`PostToolUseFailure` also fires the mid-cycle arm** (`src/codeagent.rs`
  ~5889; accepted residual #2). Fine for delivery, but if M6 lights the lamp off
  the hook it must key on the *event*, not the hook firing, or a flaky tool could
  strobe the lamp.
- **Inbox/channel blocks carry `owner: String::new()`** (`inbox_block`/
  `channel_block`, ~2348/2376). The block inspector (M4) must not key these
  ephemeral blocks by owner — they are session-computed, so render them under the
  session, separate from owner-keyed durable blocks.
- **Mid-cycle mail is NOT marked seen** (by design — `take_pending_mid_cycle_mail`
  ~485). So the same message legitimately appears in BOTH the mid-cycle tell and
  the next-turn `inbox` count until the agent pulls it. The UI must explain this
  ("urgent copy delivered early; still unread") rather than look like a bug.
- **`channel_optin` parses `agent-comms.channels` as a TOML fragment**
  (~2284). If M3 ever lets a human edit the opt-in list, validate it round-trips
  as a TOML array, or a malformed value silently yields no channel block.
- **Estimation package ships in `kits/stdlib` (protected/always-on)** — the
  work-estimation Log itself flags this sits awkwardly with the "optional
  bolt-on" framing. Not a UI concern, but if the runs view advertises estimation
  as an opt-in feature, the always-on placement may confuse "why is this here".

## Read these first
- The why: [../journeys/11-profiles.md](../journeys/11-profiles.md)
  ("Inter-agent communication", "Memory blocks", "Estimating work").
- The agent-facing seams just shipped:
  [agent-comms-package.md](agent-comms-package.md),
  [memory-blocks.md](memory-blocks.md), [work-estimation.md](work-estimation.md).
- The two UI handoffs this extends:
  [coding-agent-observability.md](coding-agent-observability.md) (the run tree +
  the `CodeSessions` pattern this reuses) and
  [chat-conversations.md](chat-conversations.md) (the nav split + correlation
  threading).
- The web surface: [web-packaging.md](web-packaging.md) and
  [web-ui-fidelity.md](web-ui-fidelity.md) (the Rust server + the SPA fidelity
  bar), plus [../journeys/07-chatting.md](../journeys/07-chatting.md).
- The code: `src/web.rs` (routes + `admin_dispatch`), `ui/web/src/App.tsx`
  (`Nav`, `RailView`, the `sel` union), `ui/web/src/CodeSessions.tsx` (the
  backfill+live-fold pattern to copy), `ui/web/test/ui.spec.mjs` (selector
  discipline), and the backend producers: `src/codeagent.rs`
  (`record_delivery`/`inbox_block`/`channel_block`/`turn_injection`),
  `src/codesession.rs` (`inbox_for_session`/`room_recent`/`peer_claims`/
  `live_siblings`), `src/blockcli.rs`, `src/estimate.rs`/`src/estimatecli.rs`.

## Log
- **2026-06-24 — block-inspector inline EDITOR shipped** (the M4 follow-on; impl Opus
  medium → adversarial verify Opus xhigh, 1 round `pass`). `POST /api/blocks`
  (`src/web.rs` `block_set`) behind the `origin_ok` CSRF/DNS-rebind guard shells
  `elanus block set … --by ui` (new blockcli `--by` arg → an attribution row in
  `context_build_log`) and re-reads the persisted value; the inspector
  (`CodeSessions.tsx`) gains an inline editor for **durable** blocks only — ephemeral
  inbox/channel blocks stay read-only (owner-less write → 400). Verifier proved the
  guard live (cross-origin & DNS-rebind → 403, local → 200, rejected writes left no
  trace) plus edit-persist + attribution. `cargo test` 286, `ui.spec.mjs` 152 ALL PASS.
  Accepted minor: a hand-crafted POST (not reachable via the UI) could create a durable
  block *named* `inbox`/`channel:*` — a cosmetic namesake within homogeneous-authority
  (CLI parity), no bypass; optional hardening = reserve that ephemeral namespace at
  session scope.
- **2026-06-24 — M1–M6 shipped** (impl on Opus medium → adversarial verify on Opus
  **xhigh**, 1 round `pass`, + a focused fixup pass). All six milestones landed:
  the agent-mail read route (`src/mailcli.rs` `elanus code mail`/`rooms --json` +
  `GET /api/comms/mail|rooms` in `src/web.rs`), the `CommsView` traffic view
  (`ui/web/src/CommsView.tsx`, live-folded like `CodeSessions`, **deduped by
  `event_id`** so a double-channel delivery is one row), rooms & channels, the
  read-only block inspector + the recomputed-ephemeral path, estimate-vs-actual in
  the runs detail (`estimate actual --session --json` → `/api/estimate/{id}`, honest
  "unknown" dollars), and the event-keyed signal lamp. The six shipped-code concerns
  were respected. **Tests:** `cargo test` 283 (new `mailcli` + `web.rs` route tests);
  the full `ui.spec.mjs` suite **ALL PASS against the Rust `elanus web` server**
  (`ELANUS_UI_SPEC_RUST=1`), incl. the new flow-11. Fixups: hardened the M2 browser
  assertion (was self-masking — it now requires rendered rows + the high chip, not
  the empty-state fallback), guarded the signal lamp against idless events, and
  aligned the M5 doc verb to the shipped `estimate actual --json`. Follow-on: the
  block-inspector inline EDITOR (M4 shipped read-only).
- **2026-06-24 — planned.** Reviewed the three just-shipped agent-facing
  handoffs (`d4c4646` agent-comms, `4cb6bb7`/`6e06dd9` memory-blocks, `fbba3bc`
  work-estimation) against the real code, and surveyed the current web UI
  (`src/web.rs` routes, `App.tsx` views, `CodeSessions.tsx`, `ui.spec.mjs`).
  Finding: the comms/blocks/estimation capabilities are entirely agent-facing
  (CLI + per-turn injection); the human can see none of the cross-agent traffic.
  Key leverage: agent-to-agent deliveries and room messages are **already ledger
  events** (`in/agent/*`, `in/group/*`) with correlation/priority/sender, already
  on `/api/stream` — so the headline comms view is a projection + a view in the
  proven `CodeSessions` backfill+fold shape, not a new capture path. Sequenced
  comms-first (M1 route → M2 traffic view → M3 rooms), then memory blocks (M4),
  estimation in the runs view (M5), and the mid-cycle/priority signal (M6).
  Recorded six correctness/UX concerns in the shipped code to respect while
  building (double-channel dedup, `PostToolUseFailure` strobe, owner-less
  ephemeral blocks, not-marked-seen mid-cycle mail, TOML-fragment channel parse,
  stdlib-protected estimation placement).

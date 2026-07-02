---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: the read/network cage is editable per-agent in the web UI

Sprint 1's [single-cage-macos.md](single-cage-macos.md) added three `[sandbox]`
profile keys — `network`, `fs_read_deny`, `fs_read_allow` — and a `CageStatus`
posture surface, but left them **config-file-only**: you can set them by hand-
editing `profile.toml`, and the setup screen *shows* the install-default posture,
but there's no way to *edit* per-agent read/network policy from the interface.
Tim's demo-day decision: make all three visible **and editable per-agent** in the
web UI, in product words, and have the posture cards reflect the edits.

The load-bearing good news from grounding: **the write path already exists and is
already the one true path.** `elanus profile set <name> sandbox.network=loopback`
and `…sandbox.fs_read_deny=["/x","/y"]` already round-trip — the CLI's dotted-key
setter (`src/profilecli.rs:143` + `parse_value` `:289`) handles nested tables and
arrays generically, and the web `agents/set` route (`src/web.rs:1153`) already
shells `elanus profile set`, rendering arrays through `toml_value`. Everything
funnels through the single `write_profile_and_commit` (`src/profilecli.rs:254`).
So there is **no parser and no writer to build** — the work is the read-back, the
form controls, and the live posture.

## Wonky bits / decisions to confirm

1. **No second writer, and no CLI parser change — verify, don't rebuild.** The
   "CLI is the API" path is complete for these keys: `profile set` already parses
   `sandbox.network`, `sandbox.fs_read_deny` (array), `sandbox.fs_read_allow`
   (array); the web `agents/set` route already reuses it; `write_profile_and_
   commit` is the single write. The UI edit MUST go through that exact route
   (`adminPost('agents/set', …)`, `App.tsx:946`) — no new endpoint, no raw-toml
   splice for these fields. *Fable: confirm we treat the existing generic setter
   as done and add zero write machinery.*

2. **The gap on the read side: `profile get` doesn't surface the three keys.**
   `profilecli::get` (`src/profilecli.rs:74`) emits `workdir`/`fs_write`/`capture_
   exclude` as typed JSON but **not** `network`/`fs_read_deny`/`fs_read_allow`, so
   the UI can't load current values without parsing raw TOML. M1 adds them (typed),
   which is the honest place — the UI reads typed fields, not a TOML blob it has to
   re-parse. *Fable: confirm surfacing on `get` over having the client parse
   `r.toml`.*

3. **Posture cards must reflect the *edited agent*, and the product-word mapping
   should live in one place.** Today the cage cards (`App.tsx:1527-1541`) bind to
   `/api/status`'s `cage` block, which reads **only the `default` profile**
   (`src/web.rs:536`, `cage_status` loads `default`). That's correct for the setup
   screen's "install posture" but wrong for a per-agent editor. The enum→product-
   word mapping ("writes fenced", "network open / this machine only / off",
   "reads open / some folders hidden / allow-list") lives inline in `web::cage_
   status` (`src/web.rs:542-566`). Rather than re-implement that mapping in the
   client (drift risk), factor it into a reusable function and have `profile get`
   return a computed **per-agent** posture (from `sandbox::cage_status`,
   `src/sandbox.rs:638`) using the same mapping; the config-view cards bind to that
   and refresh on save. *Fable: confirm per-agent posture via `profile get` (one
   mapping, server-side) over a client-side re-mapping that reflects each keystroke
   before save. Per-keystroke live preview is a nicety we can add on top; the honest
   default is "the card matches what you just saved."*

4. **The allow-list is genuinely dangerous and the UI is the only guardrail.**
   single-cage shipped `fs_read_allow` **experimental and baseline-less** on
   purpose (its M2/wonky-bit-2): a bad allow-list denies *every* read and breaks
   every spawned process for that agent (interpreters, libraries, the repo). The
   UI must put it behind an "advanced — experimental" disclosure with a **loud**
   warning, and must **not** silently "fix" a bad list — the warning is the
   mitigation, matching single-cage's deliberate stance. `network`/`fs_read_deny`
   are low-risk and sit in the open. *Fable: confirm we keep allow-list hidden +
   loud, and don't add client-side "safety" that papers over the real hazard.*

5. **The value mapping (product words → stored enums), stated once:**
   - network: **"open"** → `open` (absent) · **"this machine only"** → `loopback`
     · **"off"** → `none`.
   - "hidden folders" → `fs_read_deny` (a list of paths).
   - "advanced allow-list" → `fs_read_allow` (a list of paths; experimental).
   These are the exact values `sandbox::cage_status`/`NetworkPolicy::parse`
   (`src/sandbox.rs:51`) already read.

**Product language.** Never "SBPL", "Seatbelt", "cage", "sandbox", "profile.toml",
"allow-list" (as chrome — the disclosure can say "advanced"). Use "what this agent
may read", "network", "hidden folders", "this machine only" — the same words the
posture cards already use ([../layering.md](../layering.md), single-cage M4).

## Milestones

### M1 — `profile get` surfaces the three keys + a per-agent posture
Add `network`, `fs_read_deny`, `fs_read_allow` to `profilecli::get`'s JSON
(`src/profilecli.rs:74`, beside `workdir`/`fs_write` at `:90-92`). Also return a
computed **per-agent** `cage` posture block (product words) by calling
`sandbox::cage_status` (`src/sandbox.rs:638`) on that profile's `SandboxCfg` and
mapping the enums to the product strings — factoring the mapping currently inline
in `web::cage_status` (`src/web.rs:542-566`) into a shared helper both callers use.

**Acceptance:** `elanus profile get <name> --json` includes `network`,
`fs_read_deny`, `fs_read_allow`, and a `cage` block whose words match what
`/api/status`'s `cage` would report *for that profile's* config (a unit test
asserts an agent with `network="none"` reports "network off" while `default`
reports its own value); the mapping exists in exactly one place (grep confirms no
duplicated string set). `cargo build`/`cargo test` green.

### M2 — The per-agent edit controls (round-tripping the existing path)
In `ui/web/src/App.tsx`, extend the sandbox config section (`:2039-2046`, today
only "writable prefixes" + "capture exclude"):
- a **network** control — a select "open / this machine only / off" mapped per
  wonky bit 5.
- a **hidden folders** list input (`fs_read_deny`), using the existing `arr` CSV↔
  array helper (`App.tsx:21`).
- an **"advanced — experimental"** disclosure holding the allow-list
  (`fs_read_allow`) with the loud warning (wonky bit 4).
Load current values in `loadConfigure` (`App.tsx:830`, beside `:862-864`) from the
M1 typed fields. Save through the existing `saveConfigure` (`App.tsx:931-933`) by
adding `set['sandbox.network']`, `set['sandbox.fs_read_deny']=arr(...)`,
`set['sandbox.fs_read_allow']=arr(...)`, which flow through `adminPost('agents/
set', …)` (`App.tsx:946`) → `elanus profile set` → `write_profile_and_commit`
unchanged.

**Acceptance:** `ui.spec.mjs` opens an agent's configuration, sets network to
"this machine only" and adds a hidden folder, saves, and asserts `GET /api/admin/
profile?name=<agent>` (or `elanus profile get`) now reports `network="loopback"`
and the folder in `fs_read_deny`; the allow-list control is present only behind
the "advanced — experimental" disclosure and its warning text is in the DOM. No
new write route is added (the diff touches no writer but `App.tsx` + the M1 read).
Rebuild + re-embed the SPA before running (web-embed staleness note in memory).

### M3 — The posture cards reflect the edited agent, live
Bind the cage posture cards on the **configuration view** to the M1 per-agent
posture (from `profile get` for the agent being edited), and refresh them on save
so an edit is reflected. Leave the **setup view** cards (`App.tsx:1527-1541`,
`/api/status`) as the install-default posture — distinct surface, distinct meaning.
Product words unchanged.

**Acceptance:** `ui.spec.mjs` edits an agent's network to "off", saves, and asserts
that agent's posture card now reads "network off" while the setup screen's default
card is unchanged; a `fs_read_deny` edit flips the reads card to "some folders
hidden" and an allow-list entry flips it to "allow-list". The cards derive from the
server-computed posture (no re-implemented mapping in the client).

## Read these first
- The keys + posture this builds on: [single-cage-macos.md](single-cage-macos.md)
  (esp. wonky bit 2 on the experimental allow-list, and M4 CageStatus), [../
  sandbox.md](../sandbox.md).
- The config UX patterns to match: [configuration-ux.md](configuration-ux.md).
- `src/profile.rs` — `SandboxCfg` `:162` (`network` `:195`, `fs_read_deny` `:201`,
  `fs_read_allow` `:208`), embedded at `Profile.sandbox` `:34`.
- `src/profilecli.rs` — `get` `:74` (surface the keys here), `set` `:118` (the
  generic dotted-key setter, `:143` + `parse_value` `:289` — already handles nested
  tables + arrays, do not change), the single writer `write_profile_and_commit`
  `:254`.
- `src/web.rs` — the `agents/set` route `:1153` (`toml_value` `:2183`), the
  per-agent profile GET/PUT `admin_profile` `:1463`, `/api/status` `:466` with
  `cage_status` `:536-574` (the product-word mapping `:542-566` to factor).
- `src/sandbox.rs` — `CageStatus` `:620`, `cage_status` `:638`, `ReadScope` `:607`,
  `NetworkPolicy`/`parse` `:51`.
- `ui/web/src/App.tsx` — sandbox section `:2039`, `loadConfigure` `:830`
  (`:862-864`), `saveConfigure` `:931-933` → `agents/set` `:946`, the cage cards
  `:1527-1541`, `arr` `:21`; `ui/web/src/api.ts` `adminGet`/`adminPost`/`adminPut`.
- The wording rule: [../layering.md](../layering.md).

## Log
- 2026-07-02 — Created from Tim's demo-day findings. Grounded against the
  worktree: the CLI setter and the web `agents/set` route already round-trip
  `sandbox.network` and the two read arrays generically (no parser/writer work);
  the real gaps are (a) `profile get` doesn't emit the three keys, (b) `App.tsx`
  has no controls for them, (c) the cage cards read only the `default` profile from
  `/api/status` and don't refresh on save. Judgment calls for Fable: reuse the
  existing single write path untouched (1); surface the keys + a per-agent posture
  on `profile get`, factoring the product-word mapping to one place (2, 3); keep the
  allow-list hidden + loud + un-"fixed" per single-cage's deliberate stance (4).

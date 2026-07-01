---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-01
---

# Handoff: platform trust level (and letting agents render HTML)

Journey [../journeys/07-chatting.md](../journeys/07-chatting.md) promises that an
agent can, when it wants to, answer with HTML — small UI elements and forms that
continue a conversation without rebuilding context. The chat renderer a sibling
session started blocks raw HTML to be safe against cross-site scripting. Tim's
call reverses the default: **on your own computer you trust everything, so raw
HTML should render.** People who run elanus on a shared or remote machine get a
lower trust level that keeps the safe behavior.

The load-bearing idea: there is one **platform trust level**, the agent can
**see** it (so it knows whether its HTML will render and how exposed the machine
is), and the same setting also decides how careful the web server is.

## Wonky bits / decisions up front

1. **Where the setting lives: `bus.toml`, not the config repo, not a profile.**
   The trust level is one value for the whole installation, not per-package and
   not per-agent. The config repo (`src/config_repo.rs`) is per-package
   (`packages/<name>.toml`) and is deliberately agent-proposable — the wrong
   place for a safety switch an agent must not be able to flip. Profiles are
   per-agent (wrong altitude). `bus.toml` is the existing root config file
   (`src/paths.rs:26` `bus_file()`), read by both the daemon and the web server
   through `bus::config` (`src/bus.rs:52`, struct `BusConfig` at `src/bus.rs:41`),
   and it already carries the network posture (`bind`) this setting must inform.
   Add a `trust` field there. Two values: `full` (default) and `reduced`.
   *Fable: confirm the two-value set — I deliberately did not add a middle tier.*

2. **`bus.toml` is not fenced from agents today — fix that in this handoff.**
   The cage's protected set (`src/sandbox.rs:62`, `Protect::for_root`) fences the
   ledger, profiles, config repo, and secrets, but **not** `bus.toml`, which sits
   directly in the write root. A caged agent can write it right now. If the trust
   level lives there, an injected agent could raise its own trust. So M2 adds
   `bus.toml` to `deny_write_files`. Small, honest, closes an escalation path.

3. **The agent sees the trust level through a computed memory block.** Follow the
   resident-stage pattern (`packages/recent-history/`): a stdlib package declares
   a `[[stage]]` whose script appends a `platform` block to the system prompt each
   time context is assembled, computed from the current config (trust level, and
   whether the web port is loopback-only or exposed wider). Keeping it a package
   keeps the kernel small ([../layering.md](../layering.md)). An `exec`-mode
   stage is enough — no warm daemon is needed, the values are cheap to read.
   *Fable: the alternative is a kernel-native seed block (like the memory-blocks
   M1 seed). I lean package-stage for layering purity; confirm.*

4. **The renderer: create `ui/web/src/Markdown.tsx` here and add a trust gate.**
   A sibling session has an uncommitted `Markdown.tsx` (react-markdown +
   remark-gfm, links open in a new tab, no raw-HTML plugin) in the *other*
   checkout — it does not exist in this worktree (confirmed). Create the same
   component on this branch, then gate it: at **full** trust add `rehype-raw` so
   raw HTML in an agent message renders as real elements; at **reduced** trust
   keep the safe behavior (markdown only, HTML shown as escaped text). Today
   `App.tsx` renders agent messages as plain text (`ui/web/src/App.tsx:2235`,
   `<div className="msg-body">…{m.text}</div>`), so this also upgrades chat to
   markdown for the first time. *Note: the two checkouts will both add
   `Markdown.tsx`; a later merge may need to reconcile them.*

5. **Reduced trust also tightens the web server — the natural home for the
   deferred entry 13.** [../security.md](../security.md) entry 13 is the loopback
   confused-deputy: any local process (including a caged agent's `curl`) can hit
   `POST /api/publish` and `POST /api/admin/approve` and act as the human. Tim
   deferred closing it. Reduced trust is where it belongs: on a shared/remote
   machine, high-stakes actions (approve, publish-as-human) require a human
   gesture or are refused; on your own machine (full trust) nothing changes. This
   is milestone M5 and may ship after the rest — the core deliverable is the
   setting, the block, and the render gate.

**Product language.** In the interface this is a plain choice, e.g.
"This is my own computer (full trust)" vs "Shared or remote machine (reduced
trust)". Never surface "cage", "confused deputy", "Seatbelt", or "sandbox
posture" to the person.

## Milestones

### M1 — The trust setting in `bus.toml`
Add `trust: TrustLevel` (`full` | `reduced`, default `full`) to `BusConfig`
(`src/bus.rs:41`) and its template (`templates/bus.toml`). Expose a reader both
the daemon and the web server can call.

**Acceptance:** `bus::config(root)` returns `trust = full` for a default install
and `reduced` when the file sets it; a unit test in `src/bus.rs` covers both, and
a missing/empty file defaults to `full`.

### M2 — Fence `bus.toml` from caged agents
Add `root.bus_file()` to `Protect::deny_write_files` (`src/sandbox.rs:62-74`).

**Acceptance:** the `seatbelt_actually_cages` test (`src/sandbox.rs:589`) gains a
case: a caged write to `bus.toml` fails while a caged read of it succeeds
(mirroring the existing ledger case). `cargo test` green.

### M3 — The computed `platform` block the agent reads
Ship a small stdlib package with a `[[stage]]` (exec mode) whose script appends a
`platform` system block. Content, in plain words: the trust level, and whether
the web dashboard is reachable only from this machine (loopback) or from the
network. Model the manifest and script on `packages/recent-history/`.

**Acceptance:** with the package approved, `elanus context render <profile>
<session>` shows a `platform` block that states the current trust level and web
exposure; flipping `trust` in `bus.toml` changes what the block says on the next
render.

### M4 — `Markdown.tsx` + the render gate
Create `ui/web/src/Markdown.tsx` (react-markdown + remark-gfm, external links open
in a new tab). Render agent messages through it in the converse view
(`ui/web/src/App.tsx:2235`). Gate raw HTML on trust: full → `rehype-raw` on;
reduced → off (HTML escaped, current safe behavior).

**Acceptance:** `ui.spec.mjs` seeds a converse message containing a small HTML
snippet (e.g. a `<button>`); at full trust the rendered DOM contains the real
element; at reduced trust the same message shows the markup as visible text, no
live element. Follows the existing `data-sel`/`waitForSelector` discipline. (Re-
build the SPA and re-embed before running the spec — see the web-embed staleness
note in project memory.)

### M5 — Reduced trust tightens the web server (partial close of entry 13; may ship last)
At **reduced** trust, mutating routes that act as the human — `POST /api/publish`,
`POST /api/admin/approve` — require a human gesture or are refused for a local
request that presents no proof; free reads and converse stay open. At **full**
trust the behavior is unchanged. Record this as a partial close of
[../security.md](../security.md) entry 13.

**Acceptance:** at reduced trust a no-Origin local `curl` to `POST /api/publish`
is refused (or gated behind the gesture); at full trust the same request behaves
as today. A test drives both. Update entry 13 to note the reduced-trust close and
what remains (full trust is still an open deputy by deliberate choice).

## Read these first
- The why: [../journeys/07-chatting.md](../journeys/07-chatting.md) (the HTML-in-
  chat idea), [../journeys/04-risk-and-trust.md](../journeys/04-risk-and-trust.md)
  (the footprint the block should reflect).
- The rule for product wording: [../layering.md](../layering.md).
- The deferred hole reduced trust closes: [../security.md](../security.md) entry
  13 (`docs/security.md:220`).
- The setting's home: `src/bus.rs` (`BusConfig` :41, `config` :52), `templates/
  bus.toml`, `src/paths.rs:26`.
- The fence: `src/sandbox.rs` (`Protect::for_root` :62, `sbpl` :180, the live
  cage test :589).
- The computed-block exemplar: `packages/recent-history/elanus.toml` and
  `packages/recent-history/scripts/main`; the stage runner `src/context.rs:412`
  (`run_stage`) and manifest types `src/manifest.rs:202`.
- The renderer seam: `ui/web/src/App.tsx:2235` (plain-text message render today).

## Log
- 2026-07-01 — Created from the 2026-07-01 vision-drift recon. Verified in the
  worktree: `Markdown.tsx` absent here; `bus.toml` is the only root-wide config
  file and is read by both daemon and web via `bus::config`; `bus.toml` is NOT in
  the cage's protected set today (an escalation gap, now M2); agent messages
  render as plain text in `App.tsx:2235`. Judgment call for Fable: package-stage
  vs kernel-native block (decision 3), and whether reduced-trust web tightening
  (M5) ships in this handoff or splits out.
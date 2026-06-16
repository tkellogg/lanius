# elanus web — the agent explorer

A local web UI over the bus. The **pure-MQTT-client constraint** from ui/tui
carries over one hop: browsers can't speak raw TCP MQTT, so `server.mjs` is
the ordinary anonymous loopback MQTT 5 client, and the browser talks to *it*
— bus messages relayed over SSE, publishes accepted over POST, history
queries brokered as request/response pairs on the bus. No sqlite, no
trace.jsonl beyond the admin seam below. The filesystem touches: this
directory's static files, `<root>/bus.toml` for broker discovery, profile
files, and the run/ dir for the history endpoint.

**Authority: the same as your terminal — because it shells out to it.**
The admin seam (`/api/admin/*`) runs the `elanus` CLI, so this server adds
no authority of its own and there is one code path for every human
gesture. Commits included (Tim's call, 2026-06-12): pretending the CLI was
a safer channel claimed a boundary that doesn't exist yet
(docs/security.md entries 3–5). What a browser uniquely adds — hostile
ORIGIN traffic — is guarded for real: mutating routes require a
genuinely-local Host and a matching Origin when one is sent, and
UI-driven decisions carry `decided_by=ui` in the ledger:

- **Agents are profiles.** The nav lists every profile on disk (a silent
  root still shows its identities); *new agent* scaffolds one
  (`elanus profile new` — instant, profiles are your files, no review);
  the per-agent **configure** tab edits identity as a form — model, turn
  budget, workdir, skill visibility, and the agent name itself (renaming
  moves the mailbox to `in/agent/<new>` going forward; ledger history
  under the old noun stays). Every form save goes through
  `elanus profile set`: comments survive, and a set that wouldn't load is
  refused before it lands.
- **Kits stage, then you commit — here or in the terminal.** *kits &
  review* lists resolvable kits (`<root>/kits` is seeded with `core` at
  init; drop more in, or `~/.elanus/kits`) with README previews and an
  `installed` badge from grant provenance; staging runs
  `elanus kit add --pending`, and the pending queue renders each request
  with an approve button (ledger trail `decided_by=ui`) alongside the
  equivalent terminal command.
- **The model picker asks the provider.** `/api/admin/models` proxies
  `elanus models` (GET /v1/models with the configured base_url/key);
  compat layers without the endpoint degrade to static suggestions.

## Run

```sh
cd ui/web && npm install
node server.mjs --root /tmp/elanus-live        # or $ELANUS_ROOT / --url mqtt://...
# → http://127.0.0.1:7180   (--port to change)
```

For the historical views, also install + approve the history package on the
same root (the explorer works live-only without it — see degradation below):

```sh
cp -R packages/history <root>/packages/history
elanus approve history          # the daemon supervises it from there
```

## What you're looking at

- **left nav**: agents discovered two ways — live (`in/agent/<noun>` /
  `obs/agent/<noun>/…` traffic) and from the history view's `agents` query —
  each with its recorded sessions beneath, plus a global **signals** entry.
  Arrow keys walk the nav; everything is a real button, so Tab/Enter work.
- **converse** (per agent): the mailbox view, scoped to `in/agent/<noun>`.
  Composes go there as `{prompt}` with a generated `el-correlation`; replies
  come back as `in/human/#` mail on the same correlation. Asks render as
  answerable cards and close themselves if answered elsewhere (CLI, TUI).
  `in/human` mail is owner-addressed, not agent-addressed, so it's routed to
  the agent whose correlation we've seen — unknown correlations land on the
  selected agent.
- **sessions** (per agent): the ledger's transcripts, served by the history
  package. Click a session for the full transcript — user/assistant
  rendered as speech, tool calls and tool results as collapsible ⚙ blocks —
  and page backwards with "load earlier".
- **telemetry** (per agent): the live rail filtered to `obs/agent/<noun>/#`
  (+ that agent's mailbox under the *work* filter).
- **signals** (global): the same rail, unscoped, opening on the algedonic
  lane. **International orange stays strictly reserved for `signal/#`** —
  real flight recorders are orange; the alarm color is earned, not decoration.
- **signal lamp** (top right): lights and pulses on any `signal/#`; click to
  acknowledge.
- The server keeps a 1000-message ring so a late-opened tab gets recent
  history (the bus itself retains only per-topic last values).

## The history view

History is **not** a server privilege — it's a userland reconstruction view
(docs/bus.md: the recorder is a WAL; views that rebuild reality are ordinary
subscribers). `/api/history` proxies to `packages/history`'s HTTP endpoint
on a harness-negotiated loopback port, discovered from
`<root>/run/pkg-history/http.json` (harness state, never retained bus
messages — docs/security.md entry 11); the package reads the sqlite truth
strictly read-only and serving is a granted capability (`elanus approve
history`). `GET /api/history?kind=…` maps query params onto the flat kinds
(`agents`, `sessions`, `transcript`, `conversation`); `POST /api/history`
passes the query DSL through verbatim (kind `search`: filter × projection ×
pagination — see packages/history/SKILL.md). UI reads never become ledger
events: nothing here touches the bus at all.

**Graceful degradation**: if the history package isn't running or approved,
`/api/history` answers 503 and the explorer shows a dim
"history package not running — live view only" hint instead of breaking;
converse and telemetry keep working from live traffic. The probe re-runs, so
approving the package later heals the page without a reload.

## Test

`npm test` — real daemon on a throwaway root, the server as its MQTT client,
a plain HTTP client as the browser: SSE relay, ask + answer round trip into
the ledger with correlation intact, wildcard-publish rejection, ring
catch-up — then the history view in **both states**: absent (503 live-only
degradation, bad-kind rejection) and installed+approved (agents/sessions/
transcript/pagination/conversation/search-DSL end to end against a seeded
transcript).
Not part of tests/e2e.sh (the repo gate stays node-free).

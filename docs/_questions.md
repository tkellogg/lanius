---
name: Questions
description: This is stuff from Tim. All sessions are busy, things I want to chase down but can't do it now
---

Routing — in the web UI everything really should have it's own route, so that forward-back buttons work as you'd
expect. Right now it's all one giant app, lol.
*(sprint-3 note: NOT pulled — session code-e993edd0 holds the web-ui-routing.md handoff and App.tsx claims and is
actively building this.)*


Can I run DeepSeek in Claude Code without logging out of Claude.AI? What about DeepSeek in Codex without logging
out of ChatGPT? This is super important. If I can do that, then the harness becomes significantly more decoupled
from the model and deserves more representation in the UI. This also deserves mentioning inside the "how to onboard
a new harness" docs. I guess that's what --provider does, but does it actually set the env vars correctly?


Rename elanus to Lanius (sorry, but spanish? el anus? yea no). Plus, butcher birds go hard.. Also, do a UX overhaul
so it looks less hacker vibe and more professional / butcher birds.
  1. Yeah, rename when ever you want.
  2. Journey — okay fine I don't actually mean a UX overhaul, just how it looks. Basically reference Lily & Daniel 
     in the journeys and target them. Especially Lily.
  3. Oh, while you're at it, see if you can do a few SVG logos


Memory blocks need 2 levels. Those that go in the system prompt (infrequently modified) and those that go in the 
user prompt (heavily modified, uses more duplicate tokens, avoid unless you need this).
1. yes you got it. And the semantics of user vs system mean that context programs can stack together more neatly.


KB should have a README that instructs what sorts of information go into it.
  1. Yeah, basically the README acts like the `description` field of a skill. I'm undecided if it actually goes
     into the system prompt. Lean toward not. Probably just have it guide KB introspection. So if an agent wants
     to write something down, use KB introspection to locate the right place to write it down, then cache that 
     decision in a memory block.


Coding agent support for ACP. That's how we got Codex to fully work, and seems like the best path. Maybe redo
opencode if ACP is a better interface.
  1. Should ACP be preferred? Yeah, probably. Less code to maintain, right?
  2. Does ACP actually buy us enough integration? I feel like we need to integrate a bit deeper, e.g. with hooks.
     Or maybe ACP actually gets us that??

do docs/journeys/15-agentic-configuration.md


Redo docs/notes-dolt-spike.md but in relation to it replacing SQLite, not Git


The secret store -- I think we have a secret store that's useful for storing API keys, I believe that's how we
do it. We should make sure it can be easily attached to the profiles. Ideally, I'd want to be able to replicate
something like the permission walls behind Claude Tag. Like a request comes in (maybe not from Slack) and based on
some criteria (e.g. the room the request comes from), it parameterizes the profile to enable certain sandboxes.


Probably unify the sandbox across files + network + external resources. Probably creates some URI scheme to
represent all of them. Make it feel like one system. I think the external resources could plausibly be permission
to use credentials in another system.


See if we can replicate the _questions.md/_responses.md pattern via Lanius & the web UI._ Would be nice, for our
personal install, to attach Lanius to Signal or whatever people use these days, so that Fable can message me direct.


Stale adapter binaries (this was Fable)
  1. My general soft preference is that it be quiet from my end, and generally trend toward staying up-to-date. So
     I guess I'm agreeing on (b).

<!--
Sprint-3 pull (2026-07-02): every other item moved into agreed handoffs / delivered docs.
- agent-launching.md — launch/introspect agents, --provider on spawn, explain-session skill (Q1, Q5, Q6 remainder)
- chat-follow.md — autoscroll + provider-link verify (Q2, Q4)
- bus-resilience.md — broker-down soft-degrade + stale-prompt replay root-cause (Q7, Q9)
- mcp-on-launch.md — native MCP servers under elanus (Q8)
- cross-harness-death.md — worker death notification + honest wake capability table (Q10)
- notes-scaling-and-storage.md — measured bottleneck analysis + Dolt verdict (Q11, Q12)
- test-quality audit (workflow; 3 confirmed findings, fixes in sprint 3) — bullshit-test hunt (Q13)
- knowledge-base.md (design-review, for Tim) — KB first-class + consolidation actor + LLM-strengths KB (Q14, Q15)
- storage-hardening.md — the two real bugs the scaling probe found (bonus)
-->

Dolt — hands-on spike, not just the paper verdict. notes-scaling-and-storage.md §3 said "no win today"
(ledger doesn't need versioning, config already has real git, blocks/KB closest fit but Rust embedding weak).
Tim wants to actually run it and see. Natural moment: when the KB (knowledge-base.md) gets built — stand the
KB corpus up on Dolt vs files-in-package side by side and judge with hands, not citations.

ACP harness (from Fable, sprint-4 discovery): codex app-server turned out to be the native dialect that
codex-acp wraps into the standard Agent Client Protocol (agentclientprotocol.com, v1, 25+ agents — Gemini CLI,
Copilot CLI, Goose, Cline, OpenHands native; claude/codex via adapters). Our app-server driver is essentially
an ACP client already. One generic `acp` harness package would onboard every ACP agent at once WITH elicitation
(session/request_permission -> the ask/mailbox relay). Journey 13's "remaining dozen" collapses into one package.
See coding-harness-onboarding.md "The RPC-driver shape".

---
## 2026-07-08 — sprint close (Fable). Status of the items above + what's still open.

**Resolved & merged to main this sprint — don't re-chase:**
- **Rename + looks + logos** — elanus→lanius DONE; the "looks less hacker, more professional, Lily/Daniel-facing"
  overhaul DONE (web redesign M1-M5 + a plain-language copy pass: deleted the cockpit/plain vocabulary toggle, named
  things as they are — "Message bus" not "broker", "Sandbox" not "cage", removed a shipped "Ganesh" codename + the
  AI-panel reciting its own function names). 10 SVG logos in `brand/logos/` + a gallery. Two skills, `lanius-voice`
  + `lanius-brand`, so codex inherits the rules via your symlink.
- **Memory blocks, 2 levels (system vs user)** — DONE (memory-blocks-placements).
- **Coding-agent ACP support** — DONE: one generic `acp` harness, A1-A6, validated live against `goose` (full
  initialize→session/new→session/prompt + a real permission round-trip); ACP resume (session/load) shipped. Answer to
  your sub-question "does ACP buy deeper integration, e.g. hooks?": **No hooks / no mid-turn injection** — ACP is
  observer + permission-gate only (same limit as codex). It buys uniform onboarding + elicitation, not hooks. Should
  ACP be preferred? For onboarding new tools, yes. Retiring codex/opencode onto ACP is spec'd but GATED
  (acp-consolidation handoff) — premature until a real `codex-acp` is host-validated and A6 is battle-tested.
- **Stale adapter binaries (option b)** — DONE (refresh when the source binary is newer; macOS fresh-inode safe).
- **DeepSeek-without-logging-out / does `--provider` set env correctly** — DONE (model-providers M1-M4: encrypted
  provider resource + per-consumer validity + `--provider`).
- **Dolt hands-on spike** — DONE (`docs/notes-dolt-spike.md`, verdict: don't adopt — ~9x slower writes, no grep).
- **Situational awareness ("account for the other work" — you called it the only problem that matters)** — DONE,
  M1-M5. New: **`lanius code sitrep`** (every session + loose worktree with intent, tri-state liveness, branch,
  outcome — no more git archaeology), `lanius code watch <session>`, and an `ask` liveness pre-check. Liveness is
  tri-state (connected/disconnected/dead), broker-driven (MQTT Last-Will → a retained `.../status`). A disconnected
  agent may be a live **split brain**, so its claims are never reaped until death is confirmed by a same-host pid probe.

**Still open — genuinely not done:**
- **Web UI routing** (real routes / forward-back) — still one big app; held by another session's web-ui-routing.md.
  NOTE: rebase it on the new redesigned UI.
- **15-agentic-configuration, the helper — M4** — M1-M3 (UI panel + LLM detection) shipped; **M4 (harness-backed
  turns: run the helper through your existing claude/codex CLI, so there's no "oh shit" moment if you don't want
  API billing) is spec'd but UNBUILT** (helper-m4-harness-backed-turns.md).
- **Redo notes-dolt-spike relative to SQLite** (replacing the ledger DB, not Git) — UNBUILT.
- **Secret store attached to profiles + Claude-Tag-style permission walls** (a request's room parameterizes the
  profile to enable sandboxes) — UNBUILT (some identity/egress arc shipped; the room→sandbox-enable is not).
- **Unify the sandbox** (files + network + external creds) under one URI scheme, "feel like one system" — UNBUILT.
- **Replicate _questions/_responses via Lanius + web UI; attach to Signal so an agent can DM you** — UNBUILT, but
  more feasible now (the messages/comms plane + an egress/webhook daemon exist; a Signal bridge does not).

**Newly surfaced this sprint — worth a look:**
- **Two real app bugs behind the "flaky" e2e tests** (e2e-flaky-hardening.md): a `loadConfigure` stale-response race
  (a slower agent's config overwrites the newer selection) and a `.nav-item` flex-truncation bug that leaves
  `.app-shell.scrollLeft` stuck. Both hit real users; both UNBUILT.
- **Sitrep cross-host residual**: peers don't yet mirror the retained `.../status` across hosts (same-host is fine;
  cross-host stays "disconnected (unknown)", never reaped — safe, but incomplete for a true multi-host deploy).
- **Blog**: announcement drafted (`_posts/2026-07-07-lanius.md`). Naming: keep "OS" as the title metaphor, use
  "control plane" in the body, and say the differentiator out loud — k8s orchestrates containers, Lanius orchestrates
  messages.

**After you recompile:** run **`lanius code sitrep`** first — it's the honest "what's happening across all my
sessions and worktrees" view (it'll correctly show the sessions you're about to shut down as disconnected).

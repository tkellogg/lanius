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


Memory blocks need 2 levels. Those that go in the system prompt (infrequently modified) and those that go in the 
user prompt (heavily modified, uses more duplicate tokens, avoid unless you need this).


KB should have a README that instructs what sorts of information go into it.

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

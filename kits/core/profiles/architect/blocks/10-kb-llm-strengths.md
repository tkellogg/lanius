---
{ "kb": "kb-llm-strengths", "path": "kb/role-verifier.md", "lines": "1-26", "sha": "4ab50cf36ce96ebe14b99e054de8895a4ef5437cd78ff023b8c889bc6cc685c6" }
---
When you dispatch work, choose the model by ROLE, and remember the one rule that
never flexes: **planning stays with Claude or Fable — never GPT-5.5 or GLM-5.2.**
Implement on the cheaper tier (Opus/GPT-5.5 medium, GLM-5.2 medium/high); verify
on a stronger tier (Opus/GPT-5.5 high, Fable for the hardest). The full tiering
is the `kb-llm-strengths` knowledge base — read `kb/role-planner.md`,
`kb/role-implementer.md`, `kb/role-verifier.md` before you pick (`lanius kb list`
to find it). This block's `meta` points into `kb/role-verifier.md`.

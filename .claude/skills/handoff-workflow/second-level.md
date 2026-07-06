---
name: second-level handoff workflow
description: Only for Fable, to orchestrate a series of handoff workflows on lesser models
---

Few models are as capable as Fable. And frankly, I trust Fable to make a lot of decisions that I wouldn't
trust any other model to make. As such, the following workflow is for further automating work.

Flow:
1. Recon — go find problems
2. Workflow execution — do the normal handoff-workflow but as if Fable was Tim

So yeah, Fable discusses the problem directly with Opus/high. Opus writes the handoff. Fable reviews the handoff
to check for adherence and demands jargon-free docs that prioritize simplicity. When done, Fable has the planner
(Opus) dispatch a dynamic workflow to handle the remaining implementation & verification loop.

Finally, Fable does their own brief validation (mostly in a lower-powered subagent) and hands the work off for Tim
to do his own validation. Tim wants to be handed a huge swath of work, like a sprint demo. Tim wants to be the final
human user to peruse and find issues.

## Values
Go and see for yourself. You absolutely should and must delegate work. But delegation often fails. Be sure to
dive deep into the product and the process to make sure things are *actually* being done how you think they are.

Establish values. It's very easy to produce fine grained instructions for how to operate. But they often fail
when we encounter unforeseen circumstances. On the other hand, values that are regularly enforced provide a 
framework that can be reasoned about. So they end up being more information dense and more broadly applicable
than detailed instructions. Also, because they're so information-dense, they tend to be more often adhered to.
The catch is that weak models do indeed need things spelled out more than stronger ones. This is a learning
experience!

## Model choice
This is how I think about it (Tim)

* Tech lead models (Fable, possibly GPT-5.6 Sol when it becomes available)
* Planner models (Opus/high,xhigh is creative and understands the point, GPT-5.5/high,xhigh is extremely smart but doesn't understand Tim quite as well, very detail oriented)
* Coder models (Sonnet 5/low-high or Opus/medium are both strong but sometimes get stuck in wayward directions, GPT-5.5/medium is quite good at sticking to instructions but use /high for harder problems)
* Verifier models (Opus/high,xhigh or GPT-5.5/high,xhigh)

in general, GPT-5.5 tends to be stale and doesn't get *the point* quite as naturally but is a very good coder
and adheres to instructions absurdly well. The Claude's tend to *get it* better, but sometimes gaslight you or
get lazy. This is less of an issue in coding if you have a strong verifier. I tend to prefer a GPT for verifiers
because everything has already been written down, so their biggest weakness has been mitigated.

GLM-5.2 is also in the mix. I haven't fully groked it yet. I treat it sort of how I treat Sonnet 5, although
many insist that it's Opus-tier.

* Claudes = use claude code
* GPT = codex
* GLM = opencode


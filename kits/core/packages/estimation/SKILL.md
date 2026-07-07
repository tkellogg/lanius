---
name: estimation
description: Estimate your work right after you plan, then count actuals against it and retro on the miss. Use `lanius estimate set` once your plan is set (dollars/turns/tokens/wall-clock), `lanius estimate actual` to see the variance, and let the Stop hook (or this package's cron) record the miss into a durable learned block so the next estimate improves.
---

# estimation — estimate, count, retro (the loop)

Right after you have a plan, **declare an estimate**. From that moment everything
counts against it. Later the process retros on why it missed and adjusts a memory
block so the next estimate is better. The loop, in three verbs:

```
lanius estimate set    --session <s> --dollars 0.40 --turns 8 --tokens 120000 --wall-clock 600000
lanius estimate actual --session <s>          # actual vs estimate, dollars headline
lanius estimate retro  --session <s>           # append the miss to the learned block
```

There is **no estimation data model**. The estimate is a memory `estimate` block;
the boundary is an `obs/estimate/<session>` event; actuals come from the obs
projection (`lanius code sessions`); the learned heuristic is a durable
`estimation` block. The verbs are kernel CLI built entirely on those primitives.

## E1 — capture the estimate (the plan-time declaration)

`lanius estimate set` records a multi-dimensional estimate and **marks the
count-from boundary**. Estimates are multi-dimensional but **dollars-normalized** —
dollars is the cross-model axis. Provide whatever dimensions you can; all are
optional:

- `--dollars` — the headline (the great normalizer across models)
- `--turns` — agent turns
- `--tokens` — total tokens
- `--wall-clock` — milliseconds

Calling it again **updates** (latest wins). The estimate shows up as the
`estimate` block in your own context, so you see what you committed to.

## E2 — actuals + variance (dollars depend on pricing.toml)

`lanius estimate actual` reads the obs projection from the estimate boundary
onward — **turns, tool-calls, and wall-clock are always available**. It writes an
`estimate-vs-actual` block and prints the per-dimension variance (actual −
estimate), dollars first.

**Dollars are conditional.** They are computed only when (a) the harness surfaced
token usage and (b) the session's model is in this package's
[`pricing.toml`](pricing.toml) (model id → `$/token`). Token usage is
harness-shaped — present for some harnesses, absent for others — so a session
with no usage reports turns/tool-calls/wall-clock and says **dollars
unavailable** rather than inventing a cost. This is the cost-visibility contract
(docs/journeys/03-cost-visibility.md): dollars are unknown until pricing exists,
and `pricing.toml` is where that pricing lives. **Edit `pricing.toml` to keep
dollars honest** — add your models, update rates when they change.

A session with **no recorded estimate is skipped**, never an error.

## E3 — retro → learned block (the loop closes)

On session end (the coding agent's Stop/SessionEnd hook) the kernel appends the
miss to a durable `estimation` block (agent scope), e.g.:

```
2026-06-23 — estimated $0.40 actual $0.62 (+0.22), estimated 8 actual 13 (+5) turns; underestimated
```

A future `lanius estimate set` runs in a context carrying that `estimation`
block, so the prior misses inform the next estimate — the **default-that-evolves**
loop from memory-blocks. The MVP records the miss + a terse directional note;
the LLM "*why* it missed" reflection (an estimator agent rewriting the heuristic)
is a documented follow-on.

`lanius estimate retro` is **once per session** — it writes a marker block the
first time and is a no-op thereafter, so the Stop hook and the cron backstop
never double-count.

## What this package ships vs. what the kernel owns

- **Kernel** (no data model): the `lanius estimate set/actual/retro` verbs (block
  + obs primitives) and the Stop/SessionEnd retro hook.
- **This package**: [`pricing.toml`](pricing.toml) (the only source of dollars)
  and a **cron backstop** — every 10 minutes it sweeps finished sessions and runs
  the retro for any whose Stop hook never fired (a crash). Approving this package
  (`lanius approve estimation`) activates only that cron; the CLI verbs work
  without it.

## Deferred

- The LLM retro reflection (judging *why* it missed and rewriting the heuristic).
- A live mid-session burn-down watcher — the MVP reads the materialized obs at
  end, which gives the same accounting without a long-running subscriber.

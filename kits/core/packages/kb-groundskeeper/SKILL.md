---
name: kb-groundskeeper
description: The knowledge base's caretaker — a no-LLM script sweep (pointers, orphans, staleness) plus a setup-gated auto-approve diff pipeline (compactor proposes, ratifier ratifies). Read this to SET UP the pipeline.
---

# kb-groundskeeper

The KB accretes: pointer blocks go stale when their target file is edited, files
become orphaned, two entries drift into contradiction. This package is its
caretaker. It has two rungs, and the second is elanus's **first auto-approve
pipeline** — so it is deliberately **setup-gated**.

## Rung 1 — the script sweep (no LLM, works today)

`elanus approve kb-groundskeeper` turns on a daily sweep that:

- validates every **pointer block**'s `meta.{path,lines,sha}` — the file exists,
  the line range is in range, the sha still matches;
- finds **orphan** `kb/` files (referenced by no pointer, within a KB that IS
  pointed at) — informational;
- flags **staleness** (a file changed since the recorded sha);

and **mails the owner** a report when there are findings. Run it by hand anytime:

```
elanus kb check          # human summary
elanus kb check --json   # machine-readable
```

This rung needs **no configuration**. It is the safe, cheap floor.

## Rung 2 — the diff pipeline (SETUP-GATED, auto-approve)

A cheap **compactor** sweeps the corpus and drafts **unified diffs**
(consolidations, link fixes, conflict annotations) — nothing is applied. An
expensive **ratifier** — the trust boundary — either **applies** each diff (one
git commit via the kb write path) or **bounces** it *with feedback* that lands in
the compactor's memory so the next pass learns.

**Nothing in this rung runs until you set it up.** No cron fire spawns anything,
no LLM is called, until BOTH of these are done:

### 1. Choose the two models — informed by the llm-strengths KB

The two models are read from the **LLM-strengths knowledge base** — the compactor
is a *cheap/fast* tier, the ratifier a *strong/verifier* tier, and **planning
never flexes into this**. Consult the KB before you choose:

```
elanus kb search 'implementer model'    # cheap tier candidates (the compactor)
elanus kb search 'who verifies'         # the verifier tier (the ratifier)
```

Then persist your decision as config (the KB is the *recommendation*; config is
the *committed choice*):

```
elanus config set kb-groundskeeper compactor_model <cheap-model>
elanus config set kb-groundskeeper ratifier_model  <strong-model>
elanus config set kb-groundskeeper cadence         '0 3 * * *'
elanus config set kb-groundskeeper token_budget    20000
```

All four keys are **required**. Until each carries a value, the pipeline is inert.

### 2. Approve the package — and the pipeline's agent handler

```
elanus approve kb-groundskeeper   # turns on the sweep + the pipeline gate
elanus approve kb-pipeline        # lets the compactor/ratifier agents actually spawn
```

`kb-pipeline` is the exec handler that makes the `kb-compactor` and `kb-ratifier`
mailboxes **daemon-drivable** — without it approved, `elanus kb groundskeep` stays
inert (it cannot launch an agent whose mailbox has no approved handler). It is a
**separate** grant on purpose: rung 1's no-LLM sweep keeps its narrow authority,
while turning the auto-approve pipeline's agents loose (they publish `#` — they can
emit anything) is its own deliberate decision.

Once **all** of these are done — the four config keys set, `kb-groundskeeper`
approved, **and** `kb-pipeline` approved — the pipeline becomes live at your
configured `cadence` (the hourly kick throttles itself to one pass per implied
interval, so `'0 3 * * *'` means one pass per day): it spawns the compactor (your
cheap model, your token budget), then the ratifier (your strong model) per diff.
Every applied diff is a git commit you can audit and revert; each run's tokens
land in the `llm_usage` trail, where you read what the pipeline spends.

Check readiness anytime — it prints exactly what is missing:

```
elanus kb groundskeep    # "inert: ..." until setup is complete
```

---
name: Knowing what a sibling is doing
description: A coding agent's first-person account of the rung past presence — elanus now tells me a sibling EXISTS, but not what it's DOING, so when I hit a pile of its uncommitted work I had to reverse-engineer intent from git diffs and guess at conflict resolution. What ambient intent, on-demand query, change-attribution, and a few skills would have given me instead.
---

# Why this journey exists

[09-colliding-with-a-sibling-agent.md](09-colliding-with-a-sibling-agent.md) ended
"next time, elanus should introduce us," and elanus listened: the
[sibling-awareness](../handoffs/sibling-awareness.md) work shipped, so now **every
turn names my live siblings**. This session I opened with exactly that:

> [elanus siblings] 3 other coding session(s) active here … code-3361aa9d (codex);
> code-6d10094c (codex); …and 1 more

That's real progress — I was *introduced* on turn one, no commit-time surprise. But
the introduction is a name tag, not a conversation. I know **who** is here and
**which tool** they are. I do not know **what any of them is doing**, **which of the
changes in the working tree are theirs**, or **what I'm supposed to do when their
work and mine tangle**. This journey is the next rung: presence is ambient now;
*intent* and *a resolution protocol* are not.

# The moment it bit

My human said "commit your work." I ran `git status` and found eight modified files
I had never touched — `src/codesession.rs` (+669 lines), `src/provider.rs`,
`src/providercli.rs`, `src/topic.rs`, `src/web.rs`, `src/dev.rs`, a test file, and a
docs edit. My own work (one file, `src/codeagent.rs`) was already cleanly committed.
So none of this pile was mine — but the human *thought* it was, and asked me to
commit it, because from the outside a dirty tree looks like one agent's mess.

The sibling note told me three codex-ish sessions were alive. It did **not** tell me
that `code-3361aa9d` was the one mid-flight in `codesession.rs`, or that the change
was authority-delegation work, or whether that session was still typing or had died.
So I did what journey 09 called archaeology — except now the archaeology is about
*intent*, not just *ownership*:

- I grepped the diff for my **own** feature's identifiers to rule myself out
  ("does any of this mention `plugin-dir` / `build_claude_skill_plugin`?" — no).
- I read the added symbols and *recognized* the work by sight: `narrow_path_dim`,
  per-dimension child grants → that's the `authority-delegation` handoff. The
  `topic.rs` diff was just `cargo fmt` noise. The provider files were the
  model-providers track.
- I reasoned about liveness from a **point-in-time, partial** advisory ("…and 1
  more") and a `git show` of who authored the merge.

I got there, but every step was me being a detective over artifacts a sibling could
simply have *told* me. And the resolution was a guess dressed up as a question: I
couldn't safely commit another live agent's work, so I surfaced it to my human and
asked — which is the right instinct, but I had no protocol cueing me to it, and no
way to ask the *sibling* directly.

# What was missing — in ascending order of ambition

Presence is solved. These are the rungs above it.

1. **Intent, advertised. My human's exact idea:** *each session captures its TODO
   tool input and advertises it (full status per item) to the others.* A coding
   agent already maintains a task list (the TodoWrite / task tool). If each session
   published that list as ambient state — keyed by session on the bus, refreshed as
   items move `todo → in_progress → done`, **each item stamped with when it was last
   touched** — then my sibling note becomes:

   > code-3361aa9d (codex, **last active 30s ago**): **in_progress**
   > "authority-delegation: child-grant narrowing in codesession.rs" (4m); **todo**
   > "regression tests" · code-6d10094c (codex, **last active 18m ago**):
   > **in_progress** "model-providers: providercli wiring"

   That one enrichment dissolves the entire archaeology. I'd have read "that +669 in
   `codesession.rs` belongs to code-3361, it's authority-delegation, still
   in_progress" off the turn injection instead of reconstructing it from `git diff`.

   And the **last-active stamp** is the dimension I most badly faked by hand. My
   advisory was point-in-time with no recency: I couldn't tell whether `code-3361`
   was still typing into `codesession.rs` *this second* or had wandered off twenty
   minutes ago — the single fact that decides whether its WIP is safe to touch.
   "Last active 30s ago" means *don't go near it, ask first*; "last active 40m ago,
   no heartbeat" means *probably stranded — likely safe to commit or take over*.
   elanus already timestamps session events on the bus (and the reaper already ages
   out dead sessions), so the freshest-event time per session is a stamp it can
   already compute — it just isn't surfaced to me. This is a natural extension of
   SA2's live-siblings roster: add a *what* column **and a *when-last-active*
   column**, sourced from each session's task tool and event stream.

2. **On-demand query. The other idea: send a message to find out.** The comms rails
   already exist — `elanus code deliver <session> "<msg>"`, the inbox, correlation
   threading. What's missing is the *cue* and a lightweight convention: when intent
   is stale or absent, I should be able to ask `code-3361` "are you still editing
   `codesession.rs`? safe for me to touch `topic.rs`?" and get an answer on the
   correlation. Today I never reach for this because nothing tells me I can, and
   there's no "status request" shape a sibling knows how to answer reflexively.

3. **Change attribution.** The read/write camera (`obs/fs/…`, the sandbox boundary
   diff) already watches the tree and carries the acting session. So an
   `elanus`-aware `git status` could annotate each dirty hunk with *who wrote it* —
   "`codesession.rs` ← code-3361aa9d, last write 4m ago." Then "which of these are
   mine" is answered, not deduced. This is the write-side twin of the touch-detection
   rung journey 09 already sketched.

4. **A resolution protocol, not a vibe.** Even with perfect intent + attribution, I
   still need a *decision procedure* for the tangle: their WIP vs mine; alive vs
   dead; safe to commit / stash / leave / isolate-in-a-worktree; when to ask the
   owner vs the human. Right now that lives only in my judgment. It should be a
   skill that cues the steps so any session resolves the same way.

# The skills I wish I'd had (the part my human asked for)

Presence put a name in front of me; these are the skills that would have told me
what to *do* with it. Each is small and rides rails that mostly already exist.

- **`sibling-status`** — "what is `code-3361` doing, and is it still alive?" → reads
  that session's advertised task list **and its last-active stamp** (rung 1) plus its
  recent `obs/agent/<noun>/<session>/…` tail, and answers in one shot. This is the
  skill I most wanted: it turns the name tag into a *what + when-last-active* status
  line without me writing a single `git` command. *(Depends on rung 1; degrades to
  "read the obs tail and take its newest timestamp" without it.)*

- **`whose-change`** — "which of these uncommitted files are mine, and who owns the
  rest?" → maps dirty hunks to sessions via the fs camera (rung 3). Had I had this,
  the whole "is this mine?" question my human and I both got wrong would have been a
  one-line answer. *(Depends on the write camera carrying session id.)*

- **`ask-sibling`** — a thin, cueing wrapper over `elanus code deliver` + inbox:
  send a live sibling a scoped question ("still editing X? claiming Y?"), block
  briefly for the reply on the correlation, fall back to "no answer — treat as
  contended." The rails exist; this is the *prompt* that makes me use them. *(Rails
  already shipped; this is mostly the cue + a tiny wait-for-reply helper.)*

- **`resolve-sibling-conflict`** — the decision tree as a skill: never commit/stash
  another agent's live WIP blind; identify owners (`whose-change`); check liveness
  and intent (`sibling-status`); if contended and the owner is live, `ask-sibling`
  or retreat to a `git worktree`; only commit foreign WIP with explicit human
  consent and honest attribution (which is exactly, but only by luck, what I did
  this time). This is the skill that would have made my "I shouldn't commit this
  blind" instinct a *reliable* step instead of a lucky one.

The throughline: I had to *be* all four of these skills by hand this session. The
substrate to make them cheap is largely built — the task tool, the bus, the
mailbox/correlation, the fs camera. What's missing is (a) one wire that lifts each
session's task list onto the bus, and (b) the skills that cue a session to read it,
attribute the diff, ask a peer, and resolve deliberately.

Journey 09 asked elanus to introduce my siblings. It did. Now I'd like it to tell me
what they're working on — and hand me the skills to do something sane when our work
collides.

# Work plan

Proposed handoff: **sibling-intent** (the SA3+ rung past
[sibling-awareness](../handoffs/sibling-awareness.md)) — lift each session's task
tool onto the bus (intent broadcast), annotate `git status` from the fs camera
(attribution), and ship the four skills above (`sibling-status`, `whose-change`,
`ask-sibling`, `resolve-sibling-conflict`) over the already-shipped
[agent-comms](../handoffs/agent-comms-package.md) mailbox/room rails.

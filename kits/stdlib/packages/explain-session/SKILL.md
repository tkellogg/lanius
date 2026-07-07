---
name: explain-session
description: Explain what a dead session was doing — dispatch a read-only reader agent at the history of a past session (what it touched, why) and get a mailed-back explanation. It explains intent; it cannot change course.
---

# explain-session

"What was that session doing to my files?" You don't answer it by hand —
you dispatch a cheap reader agent at the history and let it explain. The
substrate is already there: the `history` package (a read-only HTTP daemon
over the sqlite truth) and the session-detail event timeline. This skill is
the recipe for pointing a reader at them.

The reader **explains intent, it cannot change course.** It reads the past
(the history daemon holds a `mode=ro` connection — it physically cannot
write the truth it reconstructs); it does not resume the session, edit
files, or act. If you want action, that's a fresh dispatch with its own
authority.

## 1. Identify the session

You usually start from a symptom ("this file changed and I don't know
why") or a session id you already have.

- List recent sessions: `lanius code sessions` (coding workers) or query
  the `history` package: `POST /query {"kind":"sessions","agent":"<name>"}`.
- Find by content: `history` search DSL — e.g.
  `{"kind":"search","filter":{"text":"reactor.rs"}}` surfaces sessions that
  mention the file. See the `history` skill for the full DSL.
- The Rust projection `code_projection::session_detail` (behind the same
  history surfaces) holds the full event timeline — every tool call and
  file delta — for one dead session.

## 2. Dispatch a reader

Pick the cheapest capable path.

- **A native reader** (bus-native, can mail you back): launch a native
  profile at the history.
  ```
  launch_agent {
    "profile": "helper",
    "prompt": "Read-only history task. Query the history package for session
      s-abc123 (kind=transcript, and kind=search filter.channels for its
      obs/fs/ file deltas). Explain in plain language what that session was
      DOING to the file src/reactor.rs — which tool calls touched it and to
      what end. Do not edit anything or resume the session. Mail the
      explanation back to me."
  }
  ```
  (From the CLI: `lanius agent spawn --profile helper "…"`.) The reply — or a
  failure — arrives as mail on the returned correlation.
- **A coding reader** (when the explanation wants code-level reading of a
  repo): `lanius code spawn <tool> "read-only: explain what session … did to
  <files>, using lanius history queries; mail back the summary"` on a cheap
  tier.

## 3. Read the answer

The reader mails back a plain-language account: what the session set out to
do, which files it touched, and why. That's the whole loop. If the account
says the session left something half-done, deciding what to do next is your
call (or the human's) — the reader only told you what happened.

Pointers: the `history` skill (query DSL and endpoint discovery), the
`launching-agents` skill (how to choose a profile and spawn), and
`lanius agent catalog` (which profiles are spawn-ready).

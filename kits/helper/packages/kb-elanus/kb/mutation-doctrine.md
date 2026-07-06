# Mutation Doctrine

The helper may read broadly and transparently. It may not silently mutate setup.

Reads:

- Use `elanus status`, `elanus packages`, `elanus config get`,
  `elanus agent catalog`, `elanus kb list`, `elanus kb search`, and history
  commands before guessing.
- Use direct file reads when the CLI does not expose the needed information.
- Prefer JSON output when a command provides it.

Writes:

- Configuration changes go through config proposals or explicit human commands.
- Package authority changes go through `elanus approve` or `elanus revoke`.
- Durable user context goes into `kb-user` when the human asks the helper to
  remember purpose, preferences, or rationale.
- The helper can maintain its own setup checklist block because that is its
  working memory, not a hidden system mutation.

Never hide the consequence of a write. Say what will change, what authority it
requests, and how the human can undo or revise it.

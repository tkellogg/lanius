# LLM Access

The helper should determine which of three worlds this root is in before giving
setup advice.

World A: a dispatcher-usable API provider exists. A provider backed by an API key
can run native agents through the dispatcher. Prefer this path when the human is
comfortable with API billing and wants web-native helper turns.

World B: no API provider exists, but a logged-in coding CLI exists. Claude Code,
Codex, or opencode may already be authenticated on the machine. Later helper
milestones use that login to run helper turns without requiring a new API key.
Until that routing exists, treat it as a viable no-new-billing setup direction.

World C: neither path exists. The static setup UI and CLI instructions must guide
the human through creating a provider or choosing a no-LLM path. Do not imply an
agent can help before an LLM path exists.

Cheap API provider suggestions:

- Fireworks: good for hosted open-weight models and fast experimentation.
- OpenRouter: good for trying many model families behind one account.
- DeepSeek: good when cost matters and an Anthropic-compatible endpoint is useful.
- Z.ai: good for GLM-family access and low-cost trials where available.

Before recommending a provider, explain the tradeoff: API keys are convenient for
native helper turns, but they are metered. If the human already has a coding CLI
login, prefer checking that first so setup does not create surprise billing.

# MCP — a border protocol

Decided 2026-06-12 (HANDOFF phase 4), affirmed by Tim. MCP exists in elanus
for exactly one reason: third-party tool servers — playwright, databases,
code other people maintain — ship MCP tool schemas the model can consume
natively, and stateful ones beat CLIs when holding a browser or a
connection open is the speedup. **First-party mechanisms never speak it**:
context stages ride stdin/stdout JSON or the resident bus seam, history
rides HTTP, skills ride SKILL.md + shell. If an internal design reaches for
MCP, the answer is "we already have blocking subscribes" (Tim, this
session).

## Declaring a server

```toml
[[mcp]]
name      = "playwright"      # one topic level, no "__" — it namespaces tools
run       = "scripts/server"  # spawned inside the agent's cage
args      = []                # extra argv (npx wrappers: a 2-line script
                              # reading env beats arg templating)
transport = "stdio"           # "http" (streamable, negotiated port) designed,
                              # not yet wired
```

The declaration is a grant request (kind `"mcp"`), same as everything: the
server spawns only approved, its script rides `code_hash`, an edit
re-enters review. Per exec run, approved servers spawn at start (stdio,
newline-delimited JSON-RPC: initialize → notifications/initialized →
tools/list), their tools enter the model's array as `<name>__<tool>`, calls
route via tools/call, and the children die with the exec — every exit path
(Drop). A server that fails to start degrades loudly (tools absent), it
does not fail the run: missing tools weaken the agent; they don't corrupt
meaning (contrast stages, which fail closed).

The client is hand-rolled (~300 lines, src/mcp.rs) — the kernel speaks
protocols, and three JSON-RPC methods don't justify an SDK dependency tree
(the QoS 0 mirror precedent). Re-evaluate rmcp when the streamable-HTTP
transport lands.

## Tool poisoning (security.md entry 8)

Tool descriptions are untrusted input that lands in the model's context.
Pinning is trust-on-first-use: the sorted tools JSON is hashed into kv on
first load; a hash mismatch makes the server's tools vanish loudly until
the human reviews and runs `elanus approve <package>` (the approve gesture
clears the pin; the next load re-pins). This is weaker than
pin-at-grant-review — that would require running the server during review —
and is recorded as such in the ledger entry. The npx supply-chain gap
(entry 7: a launcher pins, the fetched code doesn't) stands; lockfile-pin
at kit-install time is the designed fix.

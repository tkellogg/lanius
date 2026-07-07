---
name: kb-search
description: Search the shared knowledge base before you guess. Call the search_knowledge tool (or `lanius kb search <query>`) to recall curated facts — model tiering, roles, conventions — and get back the file + line to open. The tool's existence is high-availability; the mechanics here are expando.
---

# kb-search — recall from the knowledge base

The knowledge base is the union of every enabled package's `kb/` corpus (see
`lanius kb list`). It feels like one KB even though it is many packages. This
package makes it **searchable** two ways, both over the same FTS5 index:

- **The `search_knowledge` tool** — your primary surface. It is an ordinary
  entry in your tool array (folded in once the package is approved and visible).
  Call it with a plain-words `query`; it returns ranked hits, each a
  `{package, path, lines, snippet}` pointing at a file and a line range you can
  open with the ordinary read tools. It is read-only and cheap — call it freely
  before answering from memory.

  ```json
  { "query": "who verifies", "limit": 5 }
  → { "query": "who verifies",
      "hits": [ { "package": "kb-llm-strengths",
                  "path": "kb/role-verifier.md",
                  "lines": "6-13",
                  "snippet": "Who verifies … Opus on high … Fable for the hardest" } ] }
  ```

- **`lanius kb search <query>`** — the same hits from the CLI, for a harness or a
  human at a shell. `--json` for one JSON object per hit; `--limit N` to widen.

## Availability tiers (journey 14)

The *existence* of `search_knowledge` is high-availability: know it is there and
reach for it. The *mechanics* — the FTS5 index, the poll-driven re-index, the
engine-swap seam — are expando: read them here only when you need them.

## How it stays fresh

A read-only daemon (`scripts/index`) rebuilds the index from the corpus on disk,
polling for changes: enable or disable a `kb`-carrying package and its content
becomes findable on the next pass. The daemon never writes a `kb/` file — it only
writes its own index, in its own state dir. There is no kernel schema behind any
of this.

## Swapping the engine

`search_knowledge` is a **bare** tool name, so a different engine (an
embedding-based indexer, say) can ship the SAME `[[tool]] name = "search_knowledge"`
and replace this one invisibly — your tool array is unchanged. The harness allows
exactly **one** live holder of the name: disable this package, enable the other.
Enabling both and approving the second is refused, loudly.

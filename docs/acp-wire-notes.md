# ACP wire notes

Checked on 2026-07-06 against the live ACP v1 docs and latest schema release.

Sources:
- Protocol docs: <https://agentclientprotocol.com/protocol/v1/schema>
- Transport docs: <https://agentclientprotocol.com/protocol/v1/transports>
- Latest schema release: <https://github.com/agentclientprotocol/agent-client-protocol/releases/tag/schema-v1.19.0>
- Release asset checked: `schema.json`, digest `sha256:92c1dfcda10dd47e99127500a3763da2b471f9ac61e12b9bf0430c32cf953796`
- Release asset checked: `meta.json`, digest `sha256:e0bf36f8123b2544b499174197fdc371ec49a1b4572a35114513d56492741599`
- Real-agent handshake checked: `@zed-industries/codex-acp@0.16.0` direct binary from a local npm cache. The package warns it is deprecated in favor of `@agentclientprotocol/codex-acp`, but it returned a valid ACP initialize response.

## Transport

ACP v1 uses JSON-RPC 2.0. The stdio transport is newline-delimited UTF-8 JSON:
one complete JSON-RPC request, response, or notification per line, no embedded
newlines on the wire, client writes only ACP messages to stdin, and the agent
writes only ACP messages to stdout. Agent stderr is available for logging.

## Exact method table

Pulled from `meta.json` in schema release `schema-v1.19.0`.

| Direction | Method | Kind | A1-A3 handling |
|---|---|---|---|
| client -> agent | `initialize` | request | Sent first. |
| client -> agent | `authenticate` | request | Known but not implemented in A1-A3. |
| client -> agent | `session/new` | request | Sent after initialize. |
| client -> agent | `session/load` | request | Deferred to A6. |
| client -> agent | `session/resume` | request | Not in handoff draft; deferred. |
| client -> agent | `session/prompt` | request | Sent for the headless turn. |
| client -> agent | `session/cancel` | notification | Known but not used in A1-A3. |
| client -> agent | `session/list` | request | Not in handoff draft; not used. |
| client -> agent | `session/delete` | request | Not in handoff draft; not used. |
| client -> agent | `session/close` | request | Not in handoff draft; not used. |
| client -> agent | `session/set_mode` | request | Not in handoff draft; not used. |
| client -> agent | `session/set_config_option` | request | Not in handoff draft; not used. |
| client -> agent | `logout` | request | Not in handoff draft; not used. |
| agent -> client | `session/update` | notification | Projected to obs. |
| agent -> client | `session/request_permission` | request | Relayed to human mailbox, answered fail-closed. |
| agent -> client | `fs/read_text_file` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `fs/write_text_file` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `terminal/create` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `terminal/output` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `terminal/release` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `terminal/wait_for_exit` | request | Not advertised; fail-closed `-32601`. |
| agent -> client | `terminal/kill` | request | Not advertised; fail-closed `-32601`. |
| either side | `$/cancel_request` | notification | Protocol-level cancellation; not used in A1-A3. |

## Field names pinned

| Shape | Exact fields |
|---|---|
| `InitializeRequest` | `protocolVersion`, `clientCapabilities`, optional `clientInfo`, optional `_meta` |
| `ClientCapabilities` | `fs.readTextFile`, `fs.writeTextFile`, `terminal`, optional `session`, optional `_meta` |
| `InitializeResponse` | `protocolVersion`, `agentCapabilities`, `authMethods`, optional `agentInfo`, optional `_meta` |
| `AgentCapabilities` | `loadSession`, `promptCapabilities.image/audio/embeddedContext`, `mcpCapabilities.http/sse/acp`, `sessionCapabilities`, `auth`, optional `_meta` |
| `NewSessionRequest` | `cwd`, `mcpServers`, optional `additionalDirectories`, optional `_meta` |
| `NewSessionResponse` | `sessionId`, optional `modes`, optional `configOptions`, optional `_meta` |
| `PromptRequest` | `sessionId`, `prompt: ContentBlock[]`, optional `_meta` |
| `PromptResponse` | `stopReason`, optional `_meta` |
| `SessionNotification` | `sessionId`, `update`, optional `_meta` |
| `SessionUpdate` discriminator | `sessionUpdate` |
| `ToolCall` | required `toolCallId`, `title`; optional/defaulted `kind`, `status`, `content`, `locations`, `rawInput`, `rawOutput`, `_meta` |
| `ToolCallUpdate` | required `toolCallId`; optional `title`, `kind`, `status`, `content`, `locations`, `rawInput`, `rawOutput`, `_meta` |
| `ToolCallLocation` | `path`, optional `line`, optional `_meta` |
| `RequestPermissionRequest` | `sessionId`, `toolCall`, `options`, optional `_meta` |
| `PermissionOption` | `optionId`, `name`, `kind`, optional `_meta` |
| `RequestPermissionResponse` | `outcome`, optional `_meta` |
| selected permission outcome | `{ "outcome": "selected", "optionId": "<agent option id>" }` nested under response `outcome` |
| cancelled permission outcome | `{ "outcome": "cancelled" }` nested under response `outcome` |

## Update and enum values pinned

| Type | Exact values checked |
|---|---|
| `SessionUpdate` | `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`, `available_commands_update`, `current_mode_update`, `config_option_update`, `session_info_update`, `usage_update` |
| `ToolKind` | `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`, `other` |
| `ToolCallStatus` | `pending`, `in_progress`, `completed`, `failed` |
| `StopReason` | `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled` |
| `PermissionOptionKind` | `allow_once`, `allow_always`, `reject_once`, `reject_always` |
| `ContentBlock` baseline | `text`, `resource_link`; optional capability-gated `image`, `audio`, `resource` |

## Real initialize handshake

The checked `codex-acp` initialize response returned:

```json
{
  "protocolVersion": 1,
  "agentCapabilities": {
    "loadSession": true,
    "promptCapabilities": {
      "image": true,
      "audio": false,
      "embeddedContext": true
    },
    "mcpCapabilities": {
      "http": true,
      "sse": false,
      "acp": false
    },
    "sessionCapabilities": {
      "list": {},
      "resume": {},
      "close": {}
    },
    "auth": {
      "logout": {}
    }
  },
  "authMethods": [
    { "id": "chatgpt", "name": "Login with ChatGPT" },
    { "type": "env_var", "id": "codex-api-key", "vars": [{ "name": "CODEX_API_KEY" }] },
    { "type": "env_var", "id": "openai-api-key", "vars": [{ "name": "OPENAI_API_KEY" }] }
  ],
  "agentInfo": {
    "name": "codex-acp",
    "title": "Codex",
    "version": "0.16.0"
  }
}
```

## Divergences from the handoff draft

| Draft claim | Checked result | Impact |
|---|---|---|
| Draft omitted `clientInfo`/`agentInfo`. | Both are schema fields; currently optional. | Adapter sends `clientInfo` and records `agentInfo` if present. |
| Draft `agentCapabilities` listed `loadSession`, prompt caps, MCP HTTP/SSE. | Current schema also has `sessionCapabilities` and `auth`; real `codex-acp` also returned `mcpCapabilities.acp`. | Driver stores the full initialize result instead of hand-parsing a narrow shape. |
| Draft client request set omitted several session lifecycle/config methods. | Current method table includes `session/list`, `session/delete`, `session/resume`, `session/close`, `session/set_mode`, `session/set_config_option`, and `logout`. | A1-A3 do not call them; unknown agent requests still fail closed. |
| Draft `NewSessionRequest` and `LoadSessionRequest` had `cwd`, `mcpServers`. | They also support `additionalDirectories`; `mcpServers` remains required. | A2 sends an empty `mcpServers` array and omits additional directories. |
| Draft update variants omitted `config_option_update`, `session_info_update`, and `usage_update`. | These are current `SessionUpdate` variants. | A2 projects them to generic session obs leaves instead of dropping them. |
| Draft `ToolKind` omitted `switch_mode`. | Current `ToolKind` includes `switch_mode`. | Projection sanitizes any tool kind into `tool/<kind>/...`; no special case needed. |
| Draft `ToolCall` made `kind`, `status`, `content`, `locations`, `rawInput` look always present. | Schema requires only `toolCallId` and `title`; other fields default or may be omitted. | Mapper treats all optional tool fields defensively. |
| Draft stop reasons were open-ended. | Current schema lists `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`. | `session/idle` records the exact returned value. |
| Draft fs/terminal callback set wrote `terminal/*`. | Current exact terminal methods are `terminal/create`, `terminal/output`, `terminal/release`, `terminal/wait_for_exit`, `terminal/kill`. | Adapter advertises none and refuses all unmodeled requests with `-32601`. |
| Draft auth sentence referenced `new_session` in prose. | Wire method is `session/new`; the schema prose still contains that legacy-ish phrase in one description. | Use `session/new` only on the wire. |

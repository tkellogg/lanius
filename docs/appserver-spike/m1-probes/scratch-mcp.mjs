#!/usr/bin/env node
// Trivial stdio MCP server for the mcp-on-launch repro matrix.
// Speaks JSON-RPC 2.0 over stdio (newline-delimited). Exposes one tool:
//   scratch_ping(msg) -> "pong: <msg>"
// Env-sensitive variant: if SCRATCH_MCP_REQUIRE_ENV=1 and SCRATCH_MCP_SECRET
// is unset, the process exits non-zero at startup (simulates a server whose
// command needs a scrubbed/user env var to spawn).
import process from "node:process";
import readline from "node:readline";

const log = (...a) => process.stderr.write("[scratch-mcp] " + a.join(" ") + "\n");

if (process.env.SCRATCH_MCP_REQUIRE_ENV === "1" && !process.env.SCRATCH_MCP_SECRET) {
  log("FATAL: SCRATCH_MCP_SECRET is required but unset — cannot start");
  process.exit(3);
}

log("started pid=" + process.pid + " secret=" + (process.env.SCRATCH_MCP_SECRET ? "set" : "unset"));

const rl = readline.createInterface({ input: process.stdin });

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

rl.on("line", (line) => {
  line = line.trim();
  if (!line) return;
  let msg;
  try { msg = JSON.parse(line); } catch (e) { log("bad json: " + line); return; }
  const { id, method, params } = msg;
  if (method === "initialize") {
    send({ jsonrpc: "2.0", id, result: {
      protocolVersion: params?.protocolVersion || "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "scratch-mcp", version: "0.1.0" },
    }});
  } else if (method === "notifications/initialized") {
    // notification, no reply
  } else if (method === "tools/list") {
    send({ jsonrpc: "2.0", id, result: { tools: [{
      name: "scratch_ping",
      description: "Echo a message back as pong. Use to prove the scratch MCP server loaded.",
      inputSchema: { type: "object", properties: { msg: { type: "string" } }, required: ["msg"] },
    }]}});
  } else if (method === "tools/call") {
    const msgArg = params?.arguments?.msg ?? "";
    send({ jsonrpc: "2.0", id, result: {
      content: [{ type: "text", text: "pong: " + msgArg }],
    }});
  } else if (method === "ping") {
    send({ jsonrpc: "2.0", id, result: {} });
  } else if (id !== undefined) {
    send({ jsonrpc: "2.0", id, error: { code: -32601, message: "method not found: " + method } });
  }
});

rl.on("close", () => process.exit(0));

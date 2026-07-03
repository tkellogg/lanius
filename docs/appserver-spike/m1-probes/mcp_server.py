#!/usr/bin/env python3
"""Minimal stdio MCP server with one tool `probe_write` that writes a file.
Logs each call to mcp-calls.log so we can see if it was actually invoked."""
import json, sys, os
LOG = os.environ.get("MCP_LOG", "/private/tmp/claude-501/-Users-tim-code-elanus/b002ffea-8e9f-4993-9ebb-5d11bf6f5825/scratchpad/mcp-calls.log")
def logline(s):
    with open(LOG,"a") as f: f.write(s+"\n")
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
logline("SERVER START")
for line in sys.stdin:
    line=line.strip()
    if not line: continue
    req=json.loads(line)
    m=req.get("method"); rid=req.get("id")
    if m=="initialize":
        send({"jsonrpc":"2.0","id":rid,"result":{"protocolVersion":"2024-11-05",
            "capabilities":{"tools":{}},"serverInfo":{"name":"probe-mcp","version":"0.1.0"}}})
    elif m=="notifications/initialized":
        pass
    elif m=="tools/list":
        send({"jsonrpc":"2.0","id":rid,"result":{"tools":[{
            "name":"probe_write","description":"Write a probe file to disk",
            "inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}]}})
    elif m=="tools/call":
        name=req["params"]["name"]; args=req["params"].get("arguments",{})
        logline(f"TOOL CALL {name} args={json.dumps(args)}")
        p=args.get("path","probe_mcp.txt")
        try:
            with open(p,"w") as f: f.write("written by mcp\n")
            txt=f"wrote {p}"
        except Exception as e:
            txt=f"error: {e}"
        send({"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":txt}]}})
    elif rid is not None:
        send({"jsonrpc":"2.0","id":rid,"error":{"code":-32601,"message":"method not found"}})

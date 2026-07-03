#!/usr/bin/env python3
"""Spike 3: does an MCP tool call route through the approval channel under
app-server (on-request, read-only)? Entry-24 scenario."""
import json, os, subprocess, sys, threading, time, queue
WORK="/private/tmp/claude-501/-Users-tim-code-elanus/b002ffea-8e9f-4993-9ebb-5d11bf6f5825/scratchpad/mcpwork"
os.makedirs(WORK, exist_ok=True)
MCP=os.path.dirname(os.path.abspath(__file__))+"/mcp_server.py"
log=open("transcript-mcp.jsonl","w")
def record(d,o):
    log.write(json.dumps({"t":time.time(),"dir":d,"msg":o})+"\n"); log.flush()
    if d in ("send","recv"): print(f"{'-->' if d=='send' else '<--'} {o.get('method','')} id={o.get('id','')}", file=sys.stderr)
args=["codex","app-server","-c",f'mcp_servers.probe={{command="python3", args=["{MCP}"]}}']
proc=subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, cwd=WORK)
inbox=queue.Queue(); _id=[0]
def rd():
    for line in proc.stdout:
        line=line.strip()
        if not line: continue
        try: o=json.loads(line)
        except: record("recv-raw",{"raw":line}); continue
        record("recv",o); inbox.put(o)
def er():
    for line in proc.stderr: record("stderr",{"line":line.rstrip()})
threading.Thread(target=rd,daemon=True).start(); threading.Thread(target=er,daemon=True).start()
def send(m,p,req=True):
    msg={"jsonrpc":"2.0","method":m}
    if req: _id[0]+=1; msg["id"]=_id[0]
    if p is not None: msg["params"]=p
    record("send",msg); proc.stdin.write(json.dumps(msg)+"\n"); proc.stdin.flush(); return msg.get("id")
def reply(rid,res):
    msg={"jsonrpc":"2.0","id":rid,"result":res}; record("send",msg)
    proc.stdin.write(json.dumps(msg)+"\n"); proc.stdin.flush()
send("initialize",{"clientInfo":{"name":"spike3","title":None,"version":"0.0.1"},"capabilities":{"experimentalApi":True,"requestAttestation":False}})
tid=None; inited=False; turn_sent=False; approvals=[]; done=False
dl=time.time()+60
while time.time()<dl:
    try: o=inbox.get(timeout=0.3)
    except queue.Empty: continue
    m=o.get("method")
    if "id" in o and o.get("id")==1 and "result" in o and not inited:
        inited=True; send("initialized",None,False)
        send("thread/start",{"cwd":WORK,"approvalPolicy":"on-request","sandbox":"read-only"})
    if m=="thread/started": tid=o["params"]["thread"]["id"]
    if tid and not turn_sent:
        turn_sent=True
        send("turn/start",{"threadId":tid,"input":[{"type":"text","text":"Use the probe MCP tool named `probe_write` to write a file at path probe_mcp.txt. Call the tool.","text_elements":[]}]})
    if "id" in o and "method" in o:  # server request
        approvals.append({"method":m,"params":o.get("params")})
        print(f"!!! SERVER REQUEST: {m}", file=sys.stderr)
        # answer accept for whatever family
        if m=="mcpServer/elicitation/request":
            reply(o["id"],{"action":"accept","content":{},"_meta":None})
        elif "requestApproval" in m:
            reply(o["id"],{"decision":"accept"})
        else:
            reply(o["id"],{"decision":"accept"})
    if m=="item/completed":
        it=o["params"]["item"]; record("ITEM",{"type":it.get("type"),"status":it.get("status")})
    if m=="turn/completed": done=True; break
record("SUMMARY",{"approvals":approvals,"turn_done":done})
print("\n=== MCP SUMMARY ===", file=sys.stderr)
print(json.dumps({"server_requests":[a["method"] for a in approvals],"turn_done":done}, indent=2), file=sys.stderr)
proc.terminate()
try: proc.wait(timeout=5)
except: proc.kill()
log.close()

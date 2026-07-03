#!/usr/bin/env python3
"""Spike 2: hold an approval request open for HOLD seconds, watch for any
server-side timeout / turn-failure. Then optionally test reattach: drop the
client connection mid-approval, reconnect a fresh app-server client, and
thread/resume by threadId to see if we can rejoin the in-flight turn/approval.
"""
import json, os, subprocess, sys, threading, time, queue

WORK = "/private/tmp/claude-501/-Users-tim-code-elanus/b002ffea-8e9f-4993-9ebb-5d11bf6f5825/scratchpad/blockwork"
HOLD = int(os.environ.get("HOLD", "70"))
MODE = os.environ.get("MODE", "block")  # block | reattach
os.makedirs(WORK, exist_ok=True)
log = open(os.environ.get("TRANSCRIPT", "transcript-block.jsonl"), "w")

def record(dir, obj):
    log.write(json.dumps({"t": time.time(), "dir": dir, "msg": obj}) + "\n"); log.flush()
    if dir in ("send","recv"):
        print(f"{'-->' if dir=='send' else '<--'} {obj.get('method','')} id={obj.get('id','')}", file=sys.stderr)

class Client:
    def __init__(self, tag):
        self.tag = tag
        self.proc = subprocess.Popen(["codex","app-server"], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, cwd=WORK)
        self.inbox = queue.Queue(); self._id = 0
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._err, daemon=True).start()
    def _read(self):
        for line in self.proc.stdout:
            line=line.strip()
            if not line: continue
            try: obj=json.loads(line)
            except: record(f"recv-raw:{self.tag}", {"raw":line}); continue
            record("recv", obj); self.inbox.put(obj)
    def _err(self):
        for line in self.proc.stderr:
            record(f"stderr:{self.tag}", {"line":line.rstrip()})
    def send(self, method, params, is_req=True):
        msg={"jsonrpc":"2.0","method":method}
        if is_req:
            self._id+=1; msg["id"]=self._id
        if params is not None: msg["params"]=params
        record("send", msg)
        self.proc.stdin.write(json.dumps(msg)+"\n"); self.proc.stdin.flush()
        return msg.get("id")
    def reply(self, rid, result):
        msg={"jsonrpc":"2.0","id":rid,"result":result}
        record("send", msg)
        self.proc.stdin.write(json.dumps(msg)+"\n"); self.proc.stdin.flush()
    def kill(self):
        self.proc.terminate()
        try: self.proc.wait(timeout=5)
        except: self.proc.kill()

def drive_until_approval(c, timeout=30):
    """init, start thread, start turn; return (thread_id, turn_id, approval_req) when approval arrives."""
    thread_id=None; turn_id=None; approval=None
    c.send("initialize", {"clientInfo":{"name":"spike2","title":None,"version":"0.0.1"},
                          "capabilities":{"experimentalApi":True,"requestAttestation":False}})
    inited=False; thread_started=False; turn_started=False
    dl=time.time()+timeout
    while time.time()<dl:
        try: obj=c.inbox.get(timeout=0.2)
        except queue.Empty: continue
        m=obj.get("method")
        if "id" in obj and obj.get("id")==1 and "result" in obj and not inited:
            inited=True
            c.send("initialized", None, is_req=False)
            c.send("thread/start", {"cwd":WORK,"approvalPolicy":"on-request","sandbox":"read-only"})
        if m=="thread/started":
            thread_id=obj["params"]["thread"]["id"]
        if "result" in obj and isinstance(obj.get("result"),dict) and "thread" in obj["result"] and not turn_started:
            thread_id=obj["result"]["thread"].get("id",thread_id)
        if thread_id and not turn_started:
            turn_started=True
            c.send("turn/start", {"threadId":thread_id,"input":[{"type":"text",
                "text":"Run the shell command `touch probe.txt` in the current directory. Actually execute it.","text_elements":[]}]})
        if m=="turn/started":
            turn_id=obj["params"]["turn"].get("id")
        if "id" in obj and "method" in obj and "requestApproval" in (m or ""):
            approval=obj
            return thread_id, turn_id, approval
    return thread_id, turn_id, None

c1=Client("c1")
tid, turnid, approval = drive_until_approval(c1)
record("CHECKPOINT", {"phase":"approval-arrived","thread_id":tid,"turn_id":turnid,
                      "approval_method": approval.get("method") if approval else None,
                      "approval_id": approval.get("id") if approval else None})
if not approval:
    print("NO APPROVAL ARRIVED — aborting", file=sys.stderr); c1.kill(); sys.exit(1)

print(f"\n### Approval arrived. HOLDING for {HOLD}s (mode={MODE}) ###", file=sys.stderr)
hold_start=time.time()
saw_timeout=False
if MODE=="block":
    # do NOT reply. watch inbox for any server-side timeout/turn failure.
    while time.time()-hold_start < HOLD:
        try: obj=c1.inbox.get(timeout=0.5)
        except queue.Empty: continue
        m=obj.get("method")
        if m in ("turn/completed","turn/failed","error") or (isinstance(obj.get("params"),dict) and obj["params"].get("status")=="failed"):
            record("HOLD-EVENT", {"elapsed":time.time()-hold_start,"method":m,"msg":obj})
            print(f"!!! server emitted {m} at {time.time()-hold_start:.1f}s during hold", file=sys.stderr)
            saw_timeout=True
    record("HOLD-RESULT", {"held_seconds":time.time()-hold_start,"server_timed_out":saw_timeout})
    print(f"\n=== BLOCK RESULT: held {time.time()-hold_start:.1f}s, server_timed_out={saw_timeout} ===", file=sys.stderr)
    # now reply accept to confirm the turn still resolves after a long hold
    c1.reply(approval["id"], {"decision":"accept"})
    dl=time.time()+20; done=False
    while time.time()<dl:
        try: obj=c1.inbox.get(timeout=0.3)
        except queue.Empty: continue
        if obj.get("method")=="turn/completed":
            done=True; record("POST-HOLD", {"turn_completed_after_hold":True}); break
    print(f"=== turn completed after {HOLD}s hold + late accept: {done} ===", file=sys.stderr)
    c1.kill()
elif MODE=="reattach":
    # kill client mid-approval WITHOUT replying, then reconnect and try thread/resume
    time.sleep(2)
    record("REATTACH", {"phase":"killing-c1-mid-approval"})
    c1.kill()
    time.sleep(2)
    c2=Client("c2")
    c2.send("initialize", {"clientInfo":{"name":"spike2b","title":None,"version":"0.0.1"},
                          "capabilities":{"experimentalApi":True,"requestAttestation":False}})
    inited=False; resumed=False; got_approval2=False
    dl=time.time()+30
    while time.time()<dl:
        try: obj=c2.inbox.get(timeout=0.3)
        except queue.Empty: continue
        m=obj.get("method")
        if "id" in obj and obj.get("id")==1 and "result" in obj and not inited:
            inited=True
            c2.send("initialized", None, is_req=False)
            c2.send("thread/resume", {"threadId":tid})
        if "id" in obj and "method" in obj and "requestApproval" in (m or ""):
            got_approval2=True
            record("REATTACH-RESULT", {"re_received_approval":True,"method":m})
            print(f"!!! REATTACH re-received approval: {m}", file=sys.stderr)
            c2.reply(obj["id"], {"decision":"accept"})
        if "id" in obj and "error" in obj:
            record("REATTACH-ERROR", {"error":obj.get("error")})
            print(f"reattach error: {obj.get('error')}", file=sys.stderr)
        if m=="turn/completed":
            record("REATTACH-RESULT", {"turn_completed_after_reattach":True}); break
    record("REATTACH-SUMMARY", {"re_received_approval":got_approval2})
    c2.kill()
log.close()

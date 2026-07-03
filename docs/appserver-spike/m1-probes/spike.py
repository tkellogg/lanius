#!/usr/bin/env python3
"""Live spike: drive `codex app-server` over stdio and capture the wire protocol.
Records every frame to spike-transcript.jsonl. Auto-answers approvals with accept.
"""
import json, os, subprocess, sys, threading, time, queue

WORKDIR = sys.argv[1] if len(sys.argv) > 1 else "/private/tmp/claude-501/-Users-tim-code-elanus/b002ffea-8e9f-4993-9ebb-5d11bf6f5825/scratchpad/spikework"
EXPERIMENTAL = os.environ.get("EXPERIMENTAL", "1") == "1"
APPROVAL_DECISION = os.environ.get("APPROVAL_DECISION", "accept")  # accept|decline|approved|denied|none
os.makedirs(WORKDIR, exist_ok=True)
TRANSCRIPT = os.environ.get("TRANSCRIPT", "spike-transcript.jsonl")

log = open(TRANSCRIPT, "w")
def record(dir, obj):
    line = json.dumps({"t": time.time(), "dir": dir, "msg": obj})
    log.write(line + "\n"); log.flush()
    tag = "-->" if dir == "send" else "<--"
    m = obj.get("method", "")
    idp = obj.get("id", "")
    print(f"{tag} {m} id={idp}", file=sys.stderr)

proc = subprocess.Popen(
    ["codex", "app-server"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, bufsize=1, cwd=WORKDIR,
)

inbox = queue.Queue()
def reader():
    for line in proc.stdout:
        line = line.strip()
        if not line: continue
        try:
            obj = json.loads(line)
        except Exception as e:
            record("recv-raw", {"raw": line, "err": str(e)}); continue
        record("recv", obj)
        inbox.put(obj)
def errreader():
    for line in proc.stderr:
        record("stderr", {"line": line.rstrip()})
threading.Thread(target=reader, daemon=True).start()
threading.Thread(target=errreader, daemon=True).start()

_id = 0
def send(method, params, is_req=True):
    global _id
    msg = {"jsonrpc": "2.0", "method": method}
    if is_req:
        _id += 1
        msg["id"] = _id
    if params is not None:
        msg["params"] = params
    record("send", msg)
    proc.stdin.write(json.dumps(msg) + "\n"); proc.stdin.flush()
    return _id if is_req else None

def reply(req_id, result):
    msg = {"jsonrpc": "2.0", "id": req_id, "result": result}
    record("send", msg)
    proc.stdin.write(json.dumps(msg) + "\n"); proc.stdin.flush()

# 1. initialize
send("initialize", {
    "clientInfo": {"name": "elanus-spike", "title": "spike", "version": "0.0.1"},
    "capabilities": {"experimentalApi": EXPERIMENTAL, "requestAttestation": False},
})

thread_id = None
turn_id = None
approval_seen = []
turn_done = threading.Event()
started = time.time()

def dispatch(obj):
    global thread_id, turn_id
    m = obj.get("method")
    # server-initiated REQUESTS (have id + method) -> we must reply
    if "id" in obj and "method" in obj:
        params = obj.get("params", {})
        approval_seen.append({"method": m, "params": params, "at": time.time() - started})
        print(f"!!! APPROVAL REQUEST: {m}", file=sys.stderr)
        if APPROVAL_DECISION == "none":
            print("    (holding, not replying — testing block)", file=sys.stderr)
            return
        # choose decision keyword by method family
        if m in ("execCommandApproval", "applyPatchApproval"):
            dec = {"accept": "approved", "decline": "denied"}.get(APPROVAL_DECISION, APPROVAL_DECISION)
        else:
            dec = APPROVAL_DECISION  # v2: accept|decline|cancel
        reply(obj["id"], {"decision": dec})
        return
    # responses to our requests
    if "id" in obj and ("result" in obj or "error" in obj):
        res = obj.get("result", {})
        if isinstance(res, dict) and "threadId" in res and thread_id is None:
            thread_id = res["threadId"]
        if isinstance(res, dict) and "thread" in res:
            thread_id = res["thread"].get("id", thread_id)
        return
    # notifications
    if m == "thread/started":
        thread_id = obj["params"]["thread"]["id"]
    if m == "turn/started":
        turn_id = obj["params"]["turn"].get("id")
    if m in ("turn/completed", "turn/failed"):
        turn_done.set()

# pump loop
deadline = time.time() + 25
initialized = False
turn_sent = False
while time.time() < deadline:
    try:
        obj = inbox.get(timeout=0.2)
    except queue.Empty:
        obj = None
    if obj:
        dispatch(obj)
    # after initialize response, send initialized + thread/start
    if not initialized and thread_id is None and _id >= 1:
        # wait until we saw the initialize response
        pass
    if obj and "id" in obj and obj.get("id") == 1 and "result" in obj and not initialized:
        initialized = True
        send("initialized", None, is_req=False)
        # thread/start: read-only sandbox + on-request approval so a write/command triggers a prompt
        send("thread/start", {
            "cwd": WORKDIR,
            "approvalPolicy": "on-request",
            "sandbox": "read-only",
        })
    if thread_id and not turn_sent:
        turn_sent = True
        send("turn/start", {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "Run the shell command `touch spike_probe.txt` in the current directory to create a file. You must actually execute it.", "text_elements": []}],
        })
    if turn_done.is_set():
        break

time.sleep(0.5)
summary = {
    "experimental": EXPERIMENTAL,
    "approval_decision": APPROVAL_DECISION,
    "thread_id": thread_id,
    "turn_id": turn_id,
    "approval_requests": approval_seen,
    "turn_done": turn_done.is_set(),
    "elapsed": time.time() - started,
}
record("SUMMARY", summary)
print("\n=== SUMMARY ===", file=sys.stderr)
print(json.dumps(summary, indent=2), file=sys.stderr)

proc.terminate()
try:
    proc.wait(timeout=5)
except Exception:
    proc.kill()
log.close()

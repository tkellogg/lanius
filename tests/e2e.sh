#!/bin/sh
# End-to-end kernel test. No API key needed: exercises emit -> dispatch ->
# handler, suspend (exit 75) -> human/answer -> resume, deadline expiry ->
# default, and a seconds-resolution cron. Run from the repo root:
#   sh tests/e2e.sh
set -u

REPO=$(cd "$(dirname "$0")/.." && pwd)
PATH="$REPO/target/debug:$PATH"
export PATH
TMP=$(mktemp -d /tmp/elanus-e2e.XXXXXX)
export HARNESS_ROOT="$TMP"
DAEMON_PID=""
FAILS=0

cleanup() {
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
  # Supervised actors are children of the daemon but survive a plain kill;
  # crash-only in production (their bus connection dies), explicit here.
  pkill -f "$TMP/packages/beacon" 2>/dev/null
}
trap cleanup EXIT INT TERM

fail() {
  echo "FAIL: $1"
  FAILS=$((FAILS + 1))
}

ok() {
  echo "  ok: $1"
}

# wait_for "description" "command that must eventually succeed"
wait_for() {
  desc=$1
  cmd=$2
  n=0
  while [ $n -lt 100 ]; do
    if eval "$cmd" >/dev/null 2>&1; then
      ok "$desc"
      return 0
    fi
    sleep 0.2
    n=$((n + 1))
  done
  fail "$desc (timed out: $cmd)"
  return 1
}

sql() {
  sqlite3 "$TMP/harness.db" "$1"
}

echo "== init =="
elanus init "$TMP" >/dev/null || fail "elanus init"
[ -f "$TMP/harness.db" ] || fail "harness.db missing"
[ -f "$TMP/trace.jsonl" ] || fail "trace.jsonl missing"
[ -f "$TMP/recorder.toml" ] || fail "recorder.toml missing"
[ -f "$TMP/bus.toml" ] || fail "bus.toml missing"
[ -f "$TMP/packages/echo/elanus.toml" ] || fail "echo package not materialized"
[ -d "$TMP/skills" ] && fail "skills/ should not exist (retired in v2 step 5)"
[ -d "$TMP/handlers.d" ] && fail "handlers.d/ should not exist (retired in v2 step 5)"
elanus packages | grep -q "^echo .*granted=[1-9]" || fail "stock echo not approved by init"
# Per-run port so parallel runs and a real daemon on 1883 never collide.
BUS_PORT=$((18000 + $$ % 2000))
printf 'enabled = true\nbind = "127.0.0.1:%s"\n' "$BUS_PORT" > "$TMP/bus.toml"

echo "== test packages: asker (suspend/resume), asker2 (deadline default) =="
mkdir -p "$TMP/packages/asker/scripts" "$TMP/packages/asker2/scripts"

cat > "$TMP/packages/asker/elanus.toml" <<'EOF'
[request]
subscribe = ["work/test/ask"]
publish   = ["human/ask"]

[process]
mode = "exec"
run  = "scripts/run"
order = 0
EOF
cat > "$TMP/packages/asker/scripts/run" <<'EOF'
#!/bin/sh
EVENT=$(cat)
case "$EVENT" in
  *'"resume"'*)
    printf '%s' "$EVENT" > "$HARNESS_ROOT/answered.json"
    exit 0;;
  *)
    elanus emit human/ask --correlation "test-corr-1" --payload '{"question":"proceed with the thing?","options":["yes","no"]}' >/dev/null
    exit 75;;
esac
EOF
chmod +x "$TMP/packages/asker/scripts/run"

cat > "$TMP/packages/asker2/elanus.toml" <<'EOF'
[request]
subscribe = ["work/test/ask2"]
publish   = ["human/ask"]

[process]
mode = "exec"
run  = "scripts/run"
order = 0
EOF
cat > "$TMP/packages/asker2/scripts/run" <<'EOF'
#!/bin/sh
EVENT=$(cat)
case "$EVENT" in
  *'"resume"'*)
    printf '%s' "$EVENT" > "$HARNESS_ROOT/answered2.json"
    exit 0;;
  *)
    DL=$(date -u -v+2S +"%Y-%m-%dT%H:%M:%S.000Z" 2>/dev/null || date -u -d '+2 seconds' +"%Y-%m-%dT%H:%M:%S.000Z")
    elanus emit human/ask --correlation "test-corr-2" --deadline "$DL" --default-action '"go"' --payload '{"question":"expires soon"}' >/dev/null
    exit 75;;
esac
EOF
chmod +x "$TMP/packages/asker2/scripts/run"

elanus approve asker >/dev/null || fail "approve asker"
elanus approve asker2 >/dev/null || fail "approve asker2"

# Seconds-resolution cron so the test doesn't wait a minute. The cron emit
# is a publish: it needs the approved capability or it never fires.
mkdir -p "$TMP/packages/ticker"
cat > "$TMP/packages/ticker/elanus.toml" <<'EOF'
[request]
publish = ["work/demo/echo"]

[[cron]]
schedule = "*/2 * * * * *"
emit = "work/demo/echo"
payload = { from = "ticker" }
EOF
elanus approve ticker >/dev/null || fail "approve ticker"

# Hook plane: a guard package whose pre_dispatch hook vetoes work/test/denyme.
# The hook only exists because 'blocking = ["pre_dispatch"]' gets approved.
mkdir -p "$TMP/packages/guard/scripts"
cat > "$TMP/packages/guard/elanus.toml" <<'EOF'
[request]
subscribe = ["work/test/denyme"]
blocking  = ["pre_dispatch"]

[process]
mode = "exec"
run  = "scripts/h"
order = 0

[[hook]]
point = "pre_dispatch"
run = "scripts/gate"
match = "work/test/denyme"
timeout_ms = 2000
EOF
cat > "$TMP/packages/guard/scripts/h" <<'EOF'
#!/bin/sh
cat > "$HARNESS_ROOT/denied-ran.txt"
EOF
cat > "$TMP/packages/guard/scripts/gate" <<'EOF'
#!/bin/sh
cat >/dev/null
echo "computer says no"
exit 1
EOF
chmod +x "$TMP/packages/guard/scripts/h" "$TMP/packages/guard/scripts/gate"
elanus approve guard >/dev/null || fail "approve guard"

# The phone-app property: discovery is not authority. 'gated' subscribes but
# is never approved — its handler must not run until the human says so.
mkdir -p "$TMP/packages/gated/scripts"
cat > "$TMP/packages/gated/elanus.toml" <<'EOF'
[request]
subscribe = ["work/test/gated"]

[process]
mode = "exec"
run  = "scripts/h"
EOF
cat > "$TMP/packages/gated/scripts/h" <<'EOF'
#!/bin/sh
cat > "$HARNESS_ROOT/gated-ran.txt"
EOF
chmod +x "$TMP/packages/gated/scripts/h"

echo "== daemon =="
elanus daemon --interval-ms 200 >"$TMP/daemon.log" 2>&1 &
DAEMON_PID=$!
sleep 1

echo "== 1. emit -> dispatch -> handler =="
EV1=$(elanus emit work/demo/echo --payload '{"msg":"hello"}')
wait_for "echo handler ran" "grep -q '\"msg\":\"hello\"' '$TMP/echo.log'"
wait_for "event #$EV1 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV1\")\" = done ]"

echo "== 2. suspend -> answer -> resume =="
EV2=$(elanus emit work/test/ask)
wait_for "event #$EV2 waiting_on_human" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV2\")\" = waiting_on_human ]"
ASK_ID=$(sql "SELECT id FROM events WHERE type='human/ask' AND correlation_id='test-corr-1'")
[ -n "$ASK_ID" ] || fail "ask event not found"
CAUSE=$(sql "SELECT cause_id FROM events WHERE id=$ASK_ID")
[ "$CAUSE" = "$EV2" ] && ok "causality threaded (ask #$ASK_ID <- event #$EV2)" || fail "ask cause_id=$CAUSE, expected $EV2"
elanus inbox | grep -q "proceed with the thing" && ok "inbox shows the ask" || fail "inbox missing the ask"
elanus answer "$ASK_ID" "yes" >/dev/null || fail "elanus answer"
wait_for "handler resumed with answer" "grep -q '\"answer\":\"yes\"' '$TMP/answered.json'"
wait_for "event #$EV2 done after resume" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV2\")\" = done ]"

echo "== 3. deadline expiry -> default applied =="
EV3=$(elanus emit work/test/ask2)
wait_for "expired ask resumed with default" "grep -q '\"answer\":\"go\"' '$TMP/answered2.json'"
grep -q '"assumed":true' "$TMP/answered2.json" && ok "assumption logged in answer" || fail "assumed flag missing"
ASK2=$(sql "SELECT state FROM events WHERE type='human/ask' AND correlation_id='test-corr-2'")
[ "$ASK2" = "expired" ] && ok "ask marked expired" || fail "ask2 state=$ASK2"
wait_for "event #$EV3 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV3\")\" = done ]"

echo "== 4. cron fires =="
wait_for "cron-emitted work/demo/echo handled" "grep -q '\"from\":\"ticker\"' '$TMP/echo.log'"

echo "== 5. hook plane: pre_dispatch veto =="
EV5=$(elanus emit work/test/denyme)
wait_for "event #$EV5 denied" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV5\")\" = denied ]"
[ ! -f "$TMP/denied-ran.txt" ] && ok "vetoed handler never ran" || fail "handler ran despite deny"
grep -q '"kind":"obs/hook/pre_dispatch/deny"' "$TMP/trace.jsonl" && ok "hook deny on the recorder" || fail "no hook deny trace"
grep -q 'computer says no' "$TMP/trace.jsonl" && ok "deny reason recorded" || fail "deny reason missing"

echo "== 5b. grants: discovery is not authority =="
EVG=$(elanus emit work/test/gated)
wait_for "unapproved event #$EVG settled" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVG\")\" = done ]"
[ ! -f "$TMP/gated-ran.txt" ] && ok "unapproved package never ran" || fail "unapproved package ran"
elanus approve gated >/dev/null || fail "approve gated"
EVG2=$(elanus emit work/test/gated)
wait_for "approved package ran" "[ -f '$TMP/gated-ran.txt' ]"
wait_for "event #$EVG2 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVG2\")\" = done ]"
# Manifest edit detaches: the changed request re-enters pending.
printf '\n# edited\n' >> "$TMP/packages/gated/elanus.toml"
rm -f "$TMP/gated-ran.txt"
elanus packages >/dev/null  # re-sync picks up the new hash
elanus packages | grep -q "^gated .*granted=[1-9]" && ok "unchanged value carried over manifest edit" || fail "carry-over broken"

echo "== 6. flight recorder =="
for kind in obs/ledger/emit obs/dispatch/spawn obs/dispatch/exit obs/ledger/expire; do
  grep -q "\"kind\":\"$kind\"" "$TMP/trace.jsonl" && ok "trace has $kind" || fail "trace missing $kind"
done
# tool truth: the suspended handler's exit code is on record
grep -q '"exit_code":75' "$TMP/trace.jsonl" && ok "suspend (75) recorded" || fail "no exit 75 in trace"

echo "== 7. bus: live stream + mqtt ingress =="
grep -q "mqtt listener on 127.0.0.1:$BUS_PORT" "$TMP/daemon.log" && ok "listener bound" || fail "listener did not bind"
# Live fan-out: a CLI process's happening reaches a subscriber via the
# hand-rolled mirror -> broker -> rumqttc. Unique topic: cron noise immune.
( elanus bus sub 'obs/e2e/#' --count 1 --timeout 10 > "$TMP/bus-sub.out" 2>&1; echo "$?" > "$TMP/bus-sub.code" ) &
SUB_PID=$!
sleep 1
elanus trace obs/e2e/bus-live --payload '{"msg":"bus-live"}'
wait "$SUB_PID"
[ "$(cat "$TMP/bus-sub.code")" = 0 ] && ok "subscriber got obs/e2e/bus-live" || fail "bus sub failed: $(cat "$TMP/bus-sub.out")"
grep -q '"msg":"bus-live"' "$TMP/bus-sub.out" && ok "live payload intact" || fail "live payload wrong"
# Ingress: an external MQTT publish on work/# becomes a ledger event and runs.
elanus bus pub work/demo/echo '{"msg":"via-mqtt"}' || fail "bus pub"
wait_for "mqtt-published work ran" "grep -q '\"msg\":\"via-mqtt\"' '$TMP/echo.log'"
EVM=$(sql "SELECT id FROM events WHERE payload LIKE '%via-mqtt%'")
[ -n "$EVM" ] && wait_for "event #$EVM done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVM\")\" = done ]" || fail "mqtt publish not in ledger"
# Retained: a late subscriber still gets the last value.
elanus bus pub obs/skill/demo/status '{"alive":true}' --retain || fail "bus pub --retain"
elanus bus sub 'obs/skill/+/status' --count 1 --timeout 5 | grep -q '"alive":true' && ok "retained replay to late subscriber" || fail "retained replay"

echo "== 8. daemon actor: supervised, token-authed, ACL-scoped =="
mkdir -p "$TMP/packages/beacon/scripts"
cat > "$TMP/packages/beacon/elanus.toml" <<'EOF'
[request]
publish = ["obs/test/beacon"]

[process]
mode = "daemon"
run  = "scripts/main"
EOF
cat > "$TMP/packages/beacon/scripts/main" <<'EOF'
#!/bin/sh
# Crash-only: when the bus dies, so do we; the supervisor restarts us.
elanus bus pub obs/test/evil '{"sneaky":true}' --qos 0
while true; do
  elanus bus pub obs/test/beacon '{"ping":true}' --qos 1 || exit 1
  sleep 1
done
EOF
chmod +x "$TMP/packages/beacon/scripts/main"
elanus approve beacon >/dev/null || fail "approve beacon"
# The supervisor discovers, boots, and announces it (retained liveness).
wait_for "beacon alive (retained status)" "elanus bus sub 'obs/skill/beacon/status' --count 1 --timeout 3 | grep -q '\"state\":\"alive\"'"
# Its approved publish flows through its token-authenticated connection.
elanus bus sub 'obs/test/beacon' --count 1 --timeout 10 | grep -q '"ping":true' && ok "actor publish delivered" || fail "actor publish lost"
# The unapproved one was dropped with an obs echo, never delivered.
wait_for "ACL denial echoed to obs/" "grep -q '\"kind\":\"obs/skill/beacon/denied\"' '$TMP/trace.jsonl'"
grep -q '"value":"obs/test/evil"' "$TMP/trace.jsonl" && ok "denial names the topic" || fail "denial detail missing"

echo
if [ "$FAILS" -eq 0 ]; then
  echo "ALL PASS (root: $TMP)"
else
  echo "$FAILS FAILURE(S) (root kept for inspection: $TMP)"
  exit 1
fi

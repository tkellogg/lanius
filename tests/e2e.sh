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
[ -L "$TMP/handlers.d/work.demo.echo/00-echo-echo" ] || fail "echo handler not wired"
# Per-run port so parallel runs and a real daemon on 1883 never collide.
BUS_PORT=$((18000 + $$ % 2000))
printf 'enabled = true\nbind = "127.0.0.1:%s"\n' "$BUS_PORT" > "$TMP/bus.toml"

echo "== test skills: asker (suspend/resume), asker2 (deadline default) =="
mkdir -p "$TMP/skills/asker/scripts" "$TMP/skills/asker2/scripts"

cat > "$TMP/skills/asker/harness.toml" <<'EOF'
[[handler]]
on = "work/test/ask"
run = "scripts/run"
order = 0
EOF
cat > "$TMP/skills/asker/scripts/run" <<'EOF'
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
chmod +x "$TMP/skills/asker/scripts/run"

cat > "$TMP/skills/asker2/harness.toml" <<'EOF'
[[handler]]
on = "work/test/ask2"
run = "scripts/run"
order = 0
EOF
cat > "$TMP/skills/asker2/scripts/run" <<'EOF'
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
chmod +x "$TMP/skills/asker2/scripts/run"

elanus enable asker >/dev/null || fail "enable asker"
elanus enable asker2 >/dev/null || fail "enable asker2"

# Seconds-resolution cron so the test doesn't wait a minute.
mkdir -p "$TMP/skills/ticker"
cat > "$TMP/skills/ticker/harness.toml" <<'EOF'
[[cron]]
schedule = "*/2 * * * * *"
emit = "work/demo/echo"
payload = { from = "ticker" }
EOF
elanus enable ticker >/dev/null || fail "enable ticker"

# Hook plane: a guard package whose pre_dispatch hook vetoes work/test/denyme.
mkdir -p "$TMP/skills/guard/scripts"
cat > "$TMP/skills/guard/harness.toml" <<'EOF'
[[handler]]
on = "work/test/denyme"
run = "scripts/h"
order = 0

[[hook]]
point = "pre_dispatch"
run = "scripts/gate"
match = "work/test/denyme"
timeout_ms = 2000
EOF
cat > "$TMP/skills/guard/scripts/h" <<'EOF'
#!/bin/sh
cat > "$HARNESS_ROOT/denied-ran.txt"
EOF
cat > "$TMP/skills/guard/scripts/gate" <<'EOF'
#!/bin/sh
cat >/dev/null
echo "computer says no"
exit 1
EOF
chmod +x "$TMP/skills/guard/scripts/h" "$TMP/skills/guard/scripts/gate"
elanus enable guard >/dev/null || fail "enable guard"

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

echo
if [ "$FAILS" -eq 0 ]; then
  echo "ALL PASS (root: $TMP)"
else
  echo "$FAILS FAILURE(S) (root kept for inspection: $TMP)"
  exit 1
fi

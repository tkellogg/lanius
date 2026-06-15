#!/bin/sh
# End-to-end kernel test. No API key needed: exercises emit -> dispatch ->
# handler, suspend (exit 75) -> answer (in/agent/main) -> resume, deadline expiry ->
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
  # -9, unapologetically: these are throwaway test processes and the system
  # is crash-only by design. A surviving daemon holds its port and poisons
  # later runs (random bus-section failures) — that bit us repeatedly.
  [ -n "$DAEMON_PID" ] && kill -9 "$DAEMON_PID" 2>/dev/null
  [ -n "${DAEMON2_PID:-}" ] && kill -9 "$DAEMON2_PID" 2>/dev/null
  [ -n "${DAEMON3_PID:-}" ] && kill -9 "$DAEMON3_PID" 2>/dev/null
  [ -n "${LLM_PID:-}" ] && kill -9 "$LLM_PID" 2>/dev/null
  [ -n "${LLM2_PID:-}" ] && kill -9 "$LLM2_PID" 2>/dev/null
  [ -n "${LLM3_PID:-}" ] && kill -9 "$LLM3_PID" 2>/dev/null
  [ -n "${LLM5_PID:-}" ] && kill -9 "$LLM5_PID" 2>/dev/null
  [ -n "${LLM6_PID:-}" ] && kill -9 "$LLM6_PID" 2>/dev/null
  [ -n "${WH_PID:-}" ] && kill -9 "$WH_PID" 2>/dev/null
  # Supervised actors are children of the daemon but survive its death;
  # crash-only in production (their bus connection dies), explicit here.
  pkill -9 -f "$TMP/packages/" 2>/dev/null
  [ -n "${TMP4:-}" ] && pkill -9 -f "$TMP4/packages/" 2>/dev/null
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
# Evict any leaked daemon squatting on our port (a hard-killed prior run's
# trap never fired) — otherwise our daemon's bind fails with a warning and
# every bus section times out mysteriously.
lsof -ti "tcp:$BUS_PORT" 2>/dev/null | xargs kill -9 2>/dev/null
printf 'enabled = true\nbind = "127.0.0.1:%s"\n' "$BUS_PORT" > "$TMP/bus.toml"

echo "== test packages: asker (suspend/resume), asker2 (deadline default) =="
mkdir -p "$TMP/packages/asker/scripts" "$TMP/packages/asker2/scripts"

cat > "$TMP/packages/asker/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/test/ask"]
publish   = ["in/human/owner"]

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
    elanus emit in/human/owner --correlation "test-corr-1" --payload '{"question":"proceed with the thing?","options":["yes","no"]}' >/dev/null
    exit 75;;
esac
EOF
chmod +x "$TMP/packages/asker/scripts/run"

cat > "$TMP/packages/asker2/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/test/ask2"]
publish   = ["in/human/owner"]

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
    elanus emit in/human/owner --correlation "test-corr-2" --deadline "$DL" --default-action '"go"' --payload '{"question":"expires soon"}' >/dev/null
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
publish = ["in/package/demo/echo"]

[[cron]]
schedule = "*/2 * * * * *"
emit = "in/package/demo/echo"
payload = { from = "ticker" }
EOF
elanus approve ticker >/dev/null || fail "approve ticker"

# Hook plane: a guard package whose pre_dispatch hook vetoes in/package/test/denyme.
# The hook only exists because 'blocking = ["pre_dispatch"]' gets approved.
mkdir -p "$TMP/packages/guard/scripts"
cat > "$TMP/packages/guard/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/test/denyme"]
blocking  = ["pre_dispatch"]

[process]
mode = "exec"
run  = "scripts/h"
order = 0

[[hook]]
point = "pre_dispatch"
run = "scripts/gate"
match = "in/package/test/denyme"
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
subscribe = ["in/package/test/gated"]

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
EV1=$(elanus emit in/package/demo/echo --payload '{"msg":"hello"}')
wait_for "echo handler ran" "grep -q '\"msg\":\"hello\"' '$TMP/echo.log'"
wait_for "event #$EV1 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV1\")\" = done ]"
# The verified sender reaches the handler in its event envelope (docs/
# identity.md) — a CLI emit is kernel-originated, so sender is "kernel".
grep -q '"sender":"kernel"' "$TMP/echo.log" && ok "handler envelope carries the verified sender" || fail "handler did not receive sender in its envelope"

echo "== 2. suspend -> answer -> resume =="
EV2=$(elanus emit in/package/test/ask)
wait_for "event #$EV2 waiting_on_human" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV2\")\" = waiting_on_human ]"
ASK_ID=$(sql "SELECT id FROM events WHERE type='in/human/owner' AND correlation_id='test-corr-1'")
[ -n "$ASK_ID" ] || fail "ask event not found"
CAUSE=$(sql "SELECT cause_id FROM events WHERE id=$ASK_ID")
[ "$CAUSE" = "$EV2" ] && ok "causality threaded (ask #$ASK_ID <- event #$EV2)" || fail "ask cause_id=$CAUSE, expected $EV2"
elanus inbox | grep -q "proceed with the thing" && ok "inbox shows the ask" || fail "inbox missing the ask"
elanus answer "$ASK_ID" "yes" >/dev/null || fail "elanus answer"
wait_for "handler resumed with answer" "grep -q '\"answer\":\"yes\"' '$TMP/answered.json'"
wait_for "event #$EV2 done after resume" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV2\")\" = done ]"

echo "== 3. deadline expiry -> default applied =="
EV3=$(elanus emit in/package/test/ask2)
wait_for "expired ask resumed with default" "grep -q '\"answer\":\"go\"' '$TMP/answered2.json'"
grep -q '"assumed":true' "$TMP/answered2.json" && ok "assumption logged in answer" || fail "assumed flag missing"
ASK2=$(sql "SELECT state FROM events WHERE type='in/human/owner' AND correlation_id='test-corr-2'")
[ "$ASK2" = "expired" ] && ok "ask marked expired" || fail "ask2 state=$ASK2"
wait_for "event #$EV3 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV3\")\" = done ]"

echo "== 4. cron fires =="
wait_for "cron-emitted in/package/demo/echo handled" "grep -q '\"from\":\"ticker\"' '$TMP/echo.log'"

echo "== 5. hook plane: pre_dispatch veto =="
EV5=$(elanus emit in/package/test/denyme)
wait_for "event #$EV5 denied" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EV5\")\" = denied ]"
[ ! -f "$TMP/denied-ran.txt" ] && ok "vetoed handler never ran" || fail "handler ran despite deny"
grep -q '"kind":"obs/harness/hook/pre_dispatch/deny"' "$TMP/trace.jsonl" && ok "hook deny on the recorder" || fail "no hook deny trace"
grep -q 'computer says no' "$TMP/trace.jsonl" && ok "deny reason recorded" || fail "deny reason missing"

echo "== 5b. grants: discovery is not authority =="
EVG=$(elanus emit in/package/test/gated)
wait_for "unapproved event #$EVG settled" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVG\")\" = done ]"
[ ! -f "$TMP/gated-ran.txt" ] && ok "unapproved package never ran" || fail "unapproved package ran"
elanus approve gated >/dev/null || fail "approve gated"
EVG2=$(elanus emit in/package/test/gated)
wait_for "approved package ran" "[ -f '$TMP/gated-ran.txt' ]"
wait_for "event #$EVG2 done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVG2\")\" = done ]"
# Manifest-only edit: the code is unchanged, so unchanged requests carry.
printf '\n# edited\n' >> "$TMP/packages/gated/elanus.toml"
rm -f "$TMP/gated-ran.txt"
elanus packages >/dev/null  # re-sync picks up the new hash
elanus packages | grep -q "^gated .*granted=[1-9]" && ok "unchanged value carried over manifest edit" || fail "carry-over broken"
# Script swap: a grant authorizes CODE, so editing the handler re-gates the
# package even though its requests are byte-identical. The handler must not
# run again until re-approved.
printf '\necho swapped >> "$HARNESS_ROOT/gated-swapped.txt"\n' >> "$TMP/packages/gated/scripts/h"
elanus packages >/dev/null  # re-sync sees the new code_hash
elanus packages | grep -q "^gated .*granted=0" && ok "script edit re-gated the package" || fail "script edit did not re-gate"
EVG3=$(elanus emit in/package/test/gated)
wait_for "re-gated event #$EVG3 settled" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVG3\")\" = done ]"
[ ! -f "$TMP/gated-swapped.txt" ] && ok "swapped code never ran (re-approval required)" || fail "swapped code ran without re-approval"

echo "== 6. flight recorder =="
for kind in obs/harness/ledger/emit obs/harness/dispatch/spawn obs/harness/dispatch/exit obs/harness/ledger/expire; do
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
# Ingress: an external MQTT publish on in/# becomes a ledger event and runs.
elanus bus pub in/package/demo/echo '{"msg":"via-mqtt"}' || fail "bus pub"
wait_for "mqtt-published work ran" "grep -q '\"msg\":\"via-mqtt\"' '$TMP/echo.log'"
EVM=$(sql "SELECT id FROM events WHERE payload LIKE '%via-mqtt%'")
[ -n "$EVM" ] && wait_for "event #$EVM done" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVM\")\" = done ]" || fail "mqtt publish not in ledger"
# Verified sender (docs/identity.md): the broker stamps who it authenticated
# onto the ledgered event, derived from the connection — a client cannot
# forge it by putting a sender in the payload. `elanus bus pub` presents the
# owner identity (default "owner") from the fenced store.
elanus bus pub in/package/demo/echo '{"msg":"forgery","sender":"admin"}' || fail "bus pub forge"
wait_for "forged-sender event ledgered" "[ -n \"\$(sql \"SELECT id FROM events WHERE payload LIKE '%forgery%'\")\" ]"
FSND=$(sql "SELECT sender FROM events WHERE payload LIKE '%forgery%' ORDER BY id DESC LIMIT 1")
[ "$FSND" = "owner" ] && ok "publish stamped 'owner'; forged payload sender ignored" || fail "verified sender was '$FSND', expected owner"
# Multi-human / identity-as-a-name (docs/identity.md): a second fenced secret is
# a second full-authority identity, and ELANUS_OWNER picks which one a surface
# presents. The broker stamps the real one — proving the principal is a name,
# not the role "human". (The broker reads the store per-connect, so a freshly
# dropped secret is honored immediately.)
printf 'alice-secret-xyzxyzxyz' > "$TMP/.secrets/alice"
ELANUS_OWNER=alice elanus bus pub in/package/demo/echo '{"msg":"as-alice"}' || fail "bus pub as alice"
wait_for "alice's event ledgered" "[ -n \"\$(sql \"SELECT id FROM events WHERE payload LIKE '%as-alice%'\")\" ]"
ASND=$(sql "SELECT sender FROM events WHERE payload LIKE '%as-alice%' ORDER BY id DESC LIMIT 1")
[ "$ASND" = "alice" ] && ok "a second identity authenticates and is stamped 'alice'" || fail "second-identity sender was '$ASND', expected alice"
# Deny-by-default (docs/identity.md): a connection with no credential is
# refused. The CLI normally presents the owner secret from the fenced store;
# hide it and the CLI is in the same spot as a caged agent that cannot read
# the store — its connection must be refused, so the publish fails.
mv "$TMP/.secrets/owner" "$TMP/.secrets/owner.hidden"
if elanus bus pub obs/e2e/should-be-refused '{}' >/dev/null 2>&1; then
  fail "unauthenticated publish was accepted (deny-by-default not enforced)"
else
  ok "deny-by-default: unauthenticated connection refused"
fi
mv "$TMP/.secrets/owner.hidden" "$TMP/.secrets/owner"
# The owner name has a cache the surfaces read (kept in sync with the profile).
[ "$(cat "$TMP/.secrets/.owner-name" 2>/dev/null)" = "owner" ] \
  && ok ".owner-name cache written (surfaces resolve the owner identity)" || fail ".owner-name cache missing/wrong"
# Retained: a late subscriber still gets the last value. (Exact topic, not
# obs/package/+/status: stock daemon actors — recent-history — retain their
# own statuses now, and --count 1 on a wildcard grabs whichever replays
# first.)
elanus bus pub obs/package/demo/status '{"alive":true}' --retain || fail "bus pub --retain"
elanus bus sub 'obs/package/demo/status' --count 1 --timeout 5 | grep -q '"alive":true' && ok "retained replay to late subscriber" || fail "retained replay"

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
wait_for "beacon alive (retained status)" "elanus bus sub 'obs/package/beacon/status' --count 1 --timeout 3 | grep -q '\"state\":\"alive\"'"
# Its approved publish flows through its token-authenticated connection.
elanus bus sub 'obs/test/beacon' --count 1 --timeout 10 | grep -q '"ping":true' && ok "actor publish delivered" || fail "actor publish lost"
# The unapproved one was dropped with an obs echo, never delivered.
wait_for "ACL denial echoed to obs/" "grep -q '\"kind\":\"obs/package/beacon/denied\"' '$TMP/trace.jsonl'"
grep -q '"value":"obs/test/evil"' "$TMP/trace.jsonl" && ok "denial names the topic" || fail "denial detail missing"

echo "== 9. ingress bridge: linemux -> triage -> agent work =="
cp -R "$REPO/packages/linemux" "$TMP/packages/"
cp -R "$REPO/packages/triage-demo" "$TMP/packages/"
elanus approve linemux >/dev/null || fail "approve linemux"
elanus approve triage-demo >/dev/null || fail "approve triage-demo"
wait_for "linemux alive (retained status)" "elanus bus sub 'obs/package/linemux/status' --count 1 --timeout 2 | grep -q '\"state\":\"alive\"'"
mkdir -p "$TMP/run/pkg-linemux/inbox"
printf 'just a note\n' > "$TMP/run/pkg-linemux/inbox/a.line"
printf 'agent: say hello\n' > "$TMP/run/pkg-linemux/inbox/b.line"
# The variety ladder: the plain line is absorbed by the script rung...
wait_for "triage absorbed the plain line" "grep -q 'just a note' '$TMP/triage.log'"
# ...and only the agent-addressed line becomes expensive agent work.
wait_for "agent line escalated to in/agent/main" \
  "[ \"\$(sql \"SELECT COUNT(*) FROM events WHERE type='in/agent/main' AND payload LIKE '%say hello%'\")\" -ge 1 ]"
# Twin-publish died (docs/topics.md decided item 3): the arrival is published
# once, addressed; the ledger row is its record.
wait_for "arrival on the ledger" \
  "[ \"\$(sql \"SELECT COUNT(*) FROM events WHERE type='in/package/linemux/triage'\")\" -ge 2 ]"
[ ! -e "$TMP/run/pkg-linemux/inbox/a.line" ] && ok "consumed line removed" || fail "inbox file not consumed"

echo "== 10. delivery receipts + escalation =="
# notify (stock) already surfaced the earlier asks; headless osascript
# fails but the receipt must exist regardless — attempted delivery is data.
wait_for "desktop sent-receipt on the ledger" \
  "[ \"\$(sql \"SELECT COUNT(*) FROM events WHERE type='obs/channel/desktop/sent'\")\" -ge 1 ]"
cp -R "$REPO/packages/escalation" "$TMP/packages/"
# Shipped defaults are humane (30s sweep, 20s threshold); e2e tightens both.
sed -i '' -e 's,\*/30,\*/2,' -e 's,after_secs = 20,after_secs = 2,' "$TMP/packages/escalation/elanus.toml"
elanus approve escalation >/dev/null || fail "approve escalation"
EVN=$(elanus emit in/human/owner --correlation nag-corr --payload '{"question":"will you ever answer?"}')
wait_for "unanswered ask got nagged" \
  "[ \"\$(sql \"SELECT COUNT(*) FROM events WHERE type='signal/attention' AND cause_id=$EVN\")\" -ge 1 ]"
elanus emit obs/channel/desktop/acked --payload "{\"ask_id\":$EVN}" --cause "$EVN" >/dev/null
sleep 3   # let any in-flight sweep land
N1=$(sql "SELECT COUNT(*) FROM events WHERE type='signal/attention' AND cause_id=$EVN")
sleep 5   # two more sweep cycles
N2=$(sql "SELECT COUNT(*) FROM events WHERE type='signal/attention' AND cause_id=$EVN")
[ "$N1" = "$N2" ] && ok "acked receipt stops the nagging (held at $N1)" || fail "nagged past ack: $N1 -> $N2"

echo "== 11. work plane on the bus =="
# (a) A kernel-minted event (`elanus emit`, never near the listener) must be
# announced under its own topic and reach a subscribed DAEMON ACTOR — before
# this landed, kernel emits only reached exec handlers; daemon actors only
# saw bus-origin publishes. The worker is a real supervised actor: token
# CONNECT, subscribe ACL-checked against its approved filter.
mkdir -p "$TMP/packages/worker/scripts"
cat > "$TMP/packages/worker/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/e2e/work"]

[process]
mode = "daemon"
run  = "scripts/main"
EOF
cat > "$TMP/packages/worker/scripts/main" <<'EOF'
#!/bin/sh
elanus bus sub 'in/package/e2e/work' --count 1 --timeout 60 > "$ELANUS_SCRATCH/got.json" 2>&1 || exit 1
# Got it; park so the supervisor doesn't see a crash loop.
while true; do sleep 5; done
EOF
chmod +x "$TMP/packages/worker/scripts/main"
elanus approve worker >/dev/null || fail "approve worker"
wait_for "worker alive (retained status)" "elanus bus sub 'obs/package/worker/status' --count 1 --timeout 3 | grep -q '\"state\":\"alive\"'"
sleep 1  # alive is published at spawn; give the script a beat to SUBSCRIBE
EVW=$(elanus emit in/package/e2e/work --payload '{"msg":"kernel-minted"}')
wait_for "daemon actor received the kernel-minted event" "grep -q '\"msg\":\"kernel-minted\"' '$TMP/run/pkg-worker/got.json'"
grep -q "\"event_id\":$EVW" "$TMP/run/pkg-worker/got.json" && ok "announcement carries the ledger event id" || fail "event_id missing from announcement"
# (b) Completion fan-in: the worker's QoS 1 PUBACK resolves the delivery;
# the kernel-owned completion observation lands on the recorder.
wait_for "delivery completion observed in trace" \
  "grep '\"kind\":\"obs/harness/delivery/complete\"' '$TMP/trace.jsonl' | grep -q '\"topic\":\"in/package/e2e/work\"'"
grep '"kind":"obs/harness/delivery/complete"' "$TMP/trace.jsonl" | grep "in/package/e2e/work" | grep -q "\"event_id\":$EVW" \
  && ok "completion names the event" || fail "completion missing event_id"
# (c) Idempotence: a bus-origin in/# publish is announced exactly once (the
# broker fans out at inbound; the dispatcher sweep must not repeat it).
( elanus bus sub 'in/package/e2e/once' --timeout 6 > "$TMP/once.out" 2>&1 ) &
ONCE_PID=$!
sleep 1
elanus bus pub in/package/e2e/once '{"n":1}' || fail "bus pub once"
wait "$ONCE_PID"
NONCE=$(grep -c '"n":1' "$TMP/once.out")
[ "$NONCE" = 1 ] && ok "bus-origin event delivered exactly once" || fail "expected 1 delivery, saw $NONCE"
# (d) $share shared subscriptions: two group members, two events — each
# member receives exactly one (round-robin one-of-N delivery, §4.8.2).
( elanus bus sub '$share/e2eg/in/package/e2e/shared' --count 1 --timeout 15 > "$TMP/share-a.out" 2>&1; echo "$?" > "$TMP/share-a.code" ) &
SHA_PID=$!
( elanus bus sub '$share/e2eg/in/package/e2e/shared' --count 1 --timeout 15 > "$TMP/share-b.out" 2>&1; echo "$?" > "$TMP/share-b.code" ) &
SHB_PID=$!
sleep 1
elanus emit in/package/e2e/shared --payload '{"k":1}' >/dev/null
elanus emit in/package/e2e/shared --payload '{"k":2}' >/dev/null
wait "$SHA_PID" "$SHB_PID"
[ "$(cat "$TMP/share-a.code")" = 0 ] && [ "$(cat "$TMP/share-b.code")" = 0 ] \
  && ok "each \$share member got exactly one message" \
  || fail "shared group delivery: a=$(cat "$TMP/share-a.out"), b=$(cat "$TMP/share-b.out")"
cat "$TMP/share-a.out" "$TMP/share-b.out" | grep -q '"k":1' && cat "$TMP/share-a.out" "$TMP/share-b.out" | grep -q '"k":2' \
  && ok "both events delivered across the group" || fail "an event was lost in the shared group"

echo "== 12. resident hooks: live registration, broker-coordinated round trip =="
# -- 12a. pre_dispatch: a token-authed actor with an approved blocking grant
# registers via SUBSCRIBE user properties and vetoes an event before any
# handler runs. The target is a real exec handler so the dispatcher actually
# reaches the pre_dispatch gate.
mkdir -p "$TMP/packages/rtarget/scripts"
cat > "$TMP/packages/rtarget/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/test/rdeny"]

[process]
mode = "exec"
run  = "scripts/h"
EOF
cat > "$TMP/packages/rtarget/scripts/h" <<'EOF'
#!/bin/sh
cat > "$HARNESS_ROOT/rtarget-ran.txt"
EOF
chmod +x "$TMP/packages/rtarget/scripts/h"
elanus approve rtarget >/dev/null || fail "approve rtarget"

mkdir -p "$TMP/packages/resident-gate/scripts"
cat > "$TMP/packages/resident-gate/elanus.toml" <<'EOF'
[request]
blocking = ["pre_dispatch"]

[process]
mode = "daemon"
run  = "scripts/main"
EOF
cat > "$TMP/packages/resident-gate/scripts/main" <<'EOF'
#!/bin/sh
# Shell-scriptable resident hook: one deny per registration, re-register.
while true; do
  printf 'deny:gate says no\n' | elanus bus sub 'obs/harness/hookreq/pre_dispatch/in/package/test/rdeny' \
    --blocking --order 10 --timeout-ms 3000 --on-timeout deny --count 1 --timeout 600 \
    >> "$ELANUS_SCRATCH/seen.jsonl" 2>> "$ELANUS_SCRATCH/err.log"
done
EOF
chmod +x "$TMP/packages/resident-gate/scripts/main"
elanus approve resident-gate >/dev/null || fail "approve resident-gate"
wait_for "resident-gate registered (hookreg/attach)" \
  "grep '\"kind\":\"obs/harness/hookreg/attach\"' '$TMP/trace.jsonl' | grep -q resident-gate"
EVR=$(elanus emit in/package/test/rdeny)
wait_for "event #$EVR denied by resident hook" "[ \"\$(sql \"SELECT state FROM events WHERE id=$EVR\")\" = denied ]"
[ ! -f "$TMP/rtarget-ran.txt" ] && ok "vetoed handler never ran" || fail "handler ran despite resident deny"
grep '"kind":"obs/harness/hook/pre_dispatch/deny"' "$TMP/trace.jsonl" | grep -q '"hook":"resident:resident-gate"' \
  && ok "resident deny echoed to obs/harness/hook" || fail "resident pre_dispatch deny echo missing"
grep -q 'gate says no' "$TMP/trace.jsonl" && ok "deny reason recorded" || fail "resident deny reason missing"
grep -q '"point":"pre_dispatch"' "$TMP/run/pkg-resident-gate/seen.jsonl" \
  && ok "hook client saw the event" || fail "hook client never saw the request"

# -- 12b. pre_tool_call: the security seam. A fake Anthropic endpoint makes
# `elanus exec` issue a real shell tool call with no API key: first request
# returns a tool_use, any request carrying a tool_result returns text.
cat > "$TMP/fake_llm.py" <<'EOF'
import json, sys
from http.server import HTTPServer, BaseHTTPRequestHandler
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(n).decode()
        if '"tool_result"' in body:
            content = [{"type": "text", "text": "task complete"}]
            stop = "end_turn"
        else:
            content = [{"type": "tool_use", "id": "tc1", "name": "shell",
                        "input": {"command": "echo ran >> \"$HARNESS_ROOT/tool-ran.txt\""}}]
            stop = "tool_use"
        resp = json.dumps({"id": "msg_1", "type": "message", "role": "assistant",
                           "model": "claude-3-5-haiku-latest", "content": content,
                           "stop_reason": stop,
                           "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(resp)))
        self.end_headers()
        self.wfile.write(resp)
    def log_message(self, *a): pass
# Port 0: the OS picks a free one; the chosen port goes to the port file.
srv = HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
EOF
python3 "$TMP/fake_llm.py" "$TMP/llm.port" &
LLM_PID=$!
wait_for "fake LLM bound" "[ -s '$TMP/llm.port' ]"
LLM_PORT=$(cat "$TMP/llm.port")
cat > "$TMP/profiles/default/profile.toml" <<EOF
agent = "main"
owner = "owner"

[model]
model = "claude-3-5-haiku-latest"
max_turns = 6
base_url = "http://127.0.0.1:$LLM_PORT"
api_key_env = "FAKE_LLM_KEY"
EOF
export FAKE_LLM_KEY=dummy

mkdir -p "$TMP/packages/resident-guard/scripts"
cat > "$TMP/packages/resident-guard/elanus.toml" <<'EOF'
[request]
blocking = ["pre_tool_call"]

[process]
mode = "daemon"
run  = "scripts/main"
EOF
cat > "$TMP/packages/resident-guard/scripts/main" <<'EOF'
#!/bin/sh
while true; do
  printf 'deny:resident veto\n' | elanus bus sub 'obs/harness/hookreq/pre_tool_call/#' \
    --blocking --timeout-ms 3000 --on-timeout deny --count 1 --timeout 600 \
    >> "$ELANUS_SCRATCH/seen.jsonl" 2>> "$ELANUS_SCRATCH/err.log"
done
EOF
chmod +x "$TMP/packages/resident-guard/scripts/main"
elanus approve resident-guard >/dev/null || fail "approve resident-guard"
wait_for "resident-guard registered (hookreg/attach)" \
  "grep '\"kind\":\"obs/harness/hookreg/attach\"' '$TMP/trace.jsonl' | grep -q resident-guard"
elanus exec "run the tool" --session rh1 > "$TMP/exec1.out" 2>&1 || fail "exec under resident deny: $(cat "$TMP/exec1.out")"
[ ! -f "$TMP/tool-ran.txt" ] && ok "denied tool never executed" || fail "tool ran despite resident deny"
grep -q '"tool":"shell"' "$TMP/run/pkg-resident-guard/seen.jsonl" \
  && ok "hook saw the tool call" || fail "hook never saw the tool call"
grep '"kind":"obs/harness/hook/pre_tool_call/deny"' "$TMP/trace.jsonl" | grep -q '"hook":"resident:resident-guard"' \
  && ok "resident pre_tool_call deny echoed" || fail "resident pre_tool_call deny echo missing"
grep '"kind":"obs/agent/main/rh1/tool/shell/result"' "$TMP/trace.jsonl" | grep -q '"denied":true' \
  && ok "deny recorded on the tool result (transcript-visible)" || fail "deny not recorded on tool result"

# -- 12c. dead hook client: revocation detaches live (grant re-checked per
# invocation), and a registered-but-mute hook falls to its declared
# on_timeout without wedging the tool call.
elanus revoke resident-guard >/dev/null || fail "revoke resident-guard"
pkill -f "$TMP/packages/resident-guard" 2>/dev/null
mkdir -p "$TMP/packages/resident-mute/scripts"
cat > "$TMP/packages/resident-mute/elanus.toml" <<'EOF'
[request]
blocking = ["pre_tool_call"]

[process]
mode = "daemon"
run  = "scripts/main"
EOF
cat > "$TMP/packages/resident-mute/scripts/main" <<'EOF'
#!/bin/sh
# Registers, receives requests, never answers (stdin is EOF): the broker's
# per-registration timeout + on_timeout=allow must decide.
elanus bus sub 'obs/harness/hookreq/pre_tool_call/#' \
  --blocking --timeout-ms 1500 --on-timeout allow --timeout 600 \
  < /dev/null >> "$ELANUS_SCRATCH/seen.jsonl" 2>> "$ELANUS_SCRATCH/err.log"
EOF
chmod +x "$TMP/packages/resident-mute/scripts/main"
elanus approve resident-mute >/dev/null || fail "approve resident-mute"
wait_for "resident-mute registered (hookreg/attach)" \
  "grep '\"kind\":\"obs/harness/hookreg/attach\"' '$TMP/trace.jsonl' | grep -q resident-mute"
START=$(date +%s)
elanus exec "run the tool" --session rh2 > "$TMP/exec2.out" 2>&1 || fail "exec under mute hook: $(cat "$TMP/exec2.out")"
ELAPSED=$(( $(date +%s) - START ))
[ -f "$TMP/tool-ran.txt" ] && ok "on_timeout=allow let the tool run" || fail "tool blocked by a hook that never answered"
[ "$ELAPSED" -lt 30 ] && ok "tool call did not hang past the timeout (${ELAPSED}s)" || fail "tool call hung ${ELAPSED}s"
grep '"kind":"obs/harness/hook/pre_tool_call/allow"' "$TMP/trace.jsonl" | grep '"hook":"resident:resident-mute"' | grep -q '"mode":"timeout"' \
  && ok "timeout outcome echoed with the declared default" || fail "timeout echo missing"

echo "== 13. agent reply is mail to the human =="
# The mailbox model's last leg: a dispatched, CORRELATED run's final text
# becomes in/human/<owner> mail carrying the conversation's correlation —
# that's how the composer (TUI/CLI) sees the reply. Drive handle-exec
# directly, with the env the dispatcher would set.
# cause_id is a real FK — anchor on an actual ledger row, as a dispatch would
EV13=$(elanus emit obs/e2e/chat-anchor --correlation chat-corr-1)
printf '{"id":%s,"correlation_id":"chat-corr-1","payload":{"prompt":"hi"}}' "$EV13" | \
  HARNESS_EVENT_ID="$EV13" HARNESS_CORRELATION_ID=chat-corr-1 elanus handle-exec >/dev/null 2>&1
REPLY=$(sql "SELECT json_extract(payload,'\$.text') FROM events WHERE type='in/human/owner' AND correlation_id='chat-corr-1'")
[ "$REPLY" = "task complete" ] \
  && ok "reply mailed to the human with the conversation correlation" \
  || fail "reply mail missing/wrong: '$REPLY'"
# Provenance: the agent's reply attributes to the agent (HARNESS_ACTOR set
# by exec from the profile), not to the kernel.
RSND=$(sql "SELECT sender FROM events WHERE type='in/human/owner' AND correlation_id='chat-corr-1'")
[ "$RSND" = "main" ] && ok "agent reply attributed to the agent (sender=main)" || fail "reply sender was '$RSND', expected main"
# An UNcorrelated run stays quiet: background work lives in the transcript,
# never the human's inbox.
NTEXT=$(sql "SELECT COUNT(*) FROM events WHERE type='in/human/owner' AND json_extract(payload,'\$.text') IS NOT NULL")
EV13B=$(elanus emit obs/e2e/chat-anchor2)
printf '{"id":%s,"payload":{"prompt":"hi"}}' "$EV13B" | \
  HARNESS_EVENT_ID="$EV13B" elanus handle-exec >/dev/null 2>&1
NTEXT2=$(sql "SELECT COUNT(*) FROM events WHERE type='in/human/owner' AND json_extract(payload,'\$.text') IS NOT NULL")
[ "$NTEXT2" = "$NTEXT" ] && ok "uncorrelated run stays out of the inbox" || fail "uncorrelated run mailed the human"

# 13b. agent FAILURE is mail too (Tim: if anything is wrong with the agent,
# every client must be told). A correlated run whose agent can't work —
# here a profile pointed at a dead LLM port — emits a labeled failure on the
# same correlation channel as a reply would, so the converse view never
# strands a delivered message.
mkdir -p "$TMP/profiles/broken"
cat > "$TMP/profiles/broken/profile.toml" <<EOF
agent = "broken"
owner = "owner"
[model]
model = "claude-3-5-haiku-latest"
base_url = "http://127.0.0.1:1"
api_key_env = "FAKE_LLM_KEY"
EOF
EVF=$(elanus emit obs/e2e/fail-anchor --correlation fail-corr-1)
printf '{"id":%s,"correlation_id":"fail-corr-1","payload":{"prompt":"hi","profile":"broken"}}' "$EVF" | \
  FAKE_LLM_KEY=dummy HARNESS_EVENT_ID="$EVF" HARNESS_CORRELATION_ID=fail-corr-1 elanus handle-exec >/dev/null 2>&1
FAILED=$(sql "SELECT json_extract(payload,'\$.failed') FROM events WHERE type='in/human/owner' AND correlation_id='fail-corr-1'")
[ "$FAILED" = "1" ] && ok "agent failure mailed to the human, labeled failed:true" || fail "failure mail missing (failed='$FAILED')"
FERR=$(sql "SELECT json_extract(payload,'\$.error') FROM events WHERE type='in/human/owner' AND correlation_id='fail-corr-1'")
[ -n "$FERR" ] && ok "failure mail carries the reason ($(echo "$FERR" | head -c 40)…)" || fail "failure mail has no error reason"
# A CLI-direct failure (no correlation) does NOT mail anyone.
NFAIL=$(sql "SELECT COUNT(*) FROM events WHERE type='in/human/owner' AND json_extract(payload,'\$.failed')=1")
FAKE_LLM_KEY=dummy elanus exec "hi" --profile broken >/dev/null 2>&1
NFAIL2=$(sql "SELECT COUNT(*) FROM events WHERE type='in/human/owner' AND json_extract(payload,'\$.failed')=1")
[ "$NFAIL2" = "$NFAIL" ] && ok "CLI-direct failure stays out of the inbox (error went to the terminal)" || fail "CLI-direct failure mailed the human"

echo "== 14. dev kit: init --kit, workdir, git-protect =="
# A fresh root, no daemon: `elanus exec` is standalone and the exec-hook
# chain is in-process. The fake LLM here reads its tool command from a file
# so each assertion drives a different shell call.
TMP2=$(mktemp -d /tmp/elanus-kit.XXXXXX)
elanus init "$TMP2" --kit "$REPO/kits/dev" --copy > "$TMP2/init.out" 2>&1 || fail "init --kit kits/dev: $(cat "$TMP2/init.out")"
[ -f "$TMP2/packages/git-protect/elanus.toml" ] && ok "git-protect materialized" || fail "git-protect not materialized"
[ -x "$TMP2/packages/git-protect/scripts/gate" ] && ok "gate script executable" || fail "gate script not executable"
[ -f "$TMP2/profiles/dev/profile.toml" ] && ok "kit profile copied" || fail "kit profile missing"
grep -q "approved git-protect blocking pre_tool_call" "$TMP2/init.out" \
  && ok "git-protect granted at init (init is the install gesture)" || fail "git-protect not approved by init"
grep -q "dev kit" "$TMP2/init.out" && ok "kit README printed" || fail "kit README not printed"
# Bare-name resolution: <repo>/kits found by walking up from the executable.
TMP3=$(mktemp -d /tmp/elanus-kit3.XXXXXX)
elanus init "$TMP3" --kit dev --copy >/dev/null 2>&1 && [ -f "$TMP3/packages/git-protect/elanus.toml" ] \
  && ok "bare kit name resolved against <repo>/kits" || fail "bare-name kit resolution"
rm -rf "$TMP3"

cat > "$TMP2/fake_llm2.py" <<'EOF'
import json, sys
from http.server import HTTPServer, BaseHTTPRequestHandler
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(n).decode()
        with open(sys.argv[3], "w") as bf:
            bf.write(body)
        if '"tool_result"' in body:
            content = [{"type": "text", "text": "done"}]
            stop = "end_turn"
        else:
            with open(sys.argv[2]) as f:
                cmd = f.read().strip()
            if cmd.startswith("TOOL:"):
                spec = json.loads(cmd[5:])
                content = [{"type": "tool_use", "id": "tc1",
                            "name": spec["name"], "input": spec["input"]}]
            else:
                content = [{"type": "tool_use", "id": "tc1", "name": "shell",
                            "input": {"command": cmd}}]
            stop = "tool_use"
        resp = json.dumps({"id": "msg_1", "type": "message", "role": "assistant",
                           "model": "claude-3-5-haiku-latest", "content": content,
                           "stop_reason": stop,
                           "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(resp)))
        self.end_headers()
        self.wfile.write(resp)
    def log_message(self, *a): pass
srv = HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
EOF
python3 "$TMP2/fake_llm2.py" "$TMP2/llm.port" "$TMP2/cmd.txt" "$TMP2/llm.body" &
LLM2_PID=$!
wait_for "kit fake LLM bound" "[ -s '$TMP2/llm.port' ]"
LLM2_PORT=$(cat "$TMP2/llm.port")
export FAKE_LLM_KEY=dummy

WS="$TMP2/agent-ws"
mkdir -p "$WS"
cat > "$TMP2/profiles/default/profile.toml" <<EOF
agent = "main"
owner = "owner"

[model]
model = "claude-3-5-haiku-latest"
max_turns = 6
base_url = "http://127.0.0.1:$LLM2_PORT"
api_key_env = "FAKE_LLM_KEY"

[sandbox]
workdir = "$WS"
EOF

# (a) workdir: the shell tool's cwd is the profile's workdir, not the root.
printf 'pwd -P > "$HARNESS_ROOT/pwd.out"' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit1 > "$TMP2/exec1.out" 2>&1 || fail "exec (workdir pwd): $(cat "$TMP2/exec1.out")"
WS_PHYS=$(cd "$WS" && pwd -P)
[ "$(cat "$TMP2/pwd.out" 2>/dev/null)" = "$WS_PHYS" ] \
  && ok "shell tool ran in workdir ($WS_PHYS)" || fail "pwd was '$(cat "$TMP2/pwd.out" 2>/dev/null)', wanted $WS_PHYS"

# (b) a missing workdir fails the tool call loudly — never a silent fallback.
sed "s,workdir = .*,workdir = \"$TMP2/no-such-dir\"," "$TMP2/profiles/default/profile.toml" > "$TMP2/p.tmp" \
  && mv "$TMP2/p.tmp" "$TMP2/profiles/default/profile.toml"
printf 'pwd > "$HARNESS_ROOT/fallback.out"' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit2 > "$TMP2/exec2.out" 2>&1 || fail "exec (missing workdir)"
[ ! -f "$TMP2/fallback.out" ] && ok "missing workdir: command never ran" || fail "missing workdir fell back silently"
grep '"kind":"obs/agent/main/kit2/tool/shell/result"' "$TMP2/trace.jsonl" | grep -q "does not exist" \
  && ok "missing workdir error is clear" || fail "missing-workdir error not surfaced"
sed "s,workdir = .*,workdir = \"$WS\"," "$TMP2/profiles/default/profile.toml" > "$TMP2/p.tmp" \
  && mv "$TMP2/p.tmp" "$TMP2/profiles/default/profile.toml"

# (c) git-protect: a force push in a scratch repo is DENIED with the reason
# echoed on obs/harness/hook, and the tool result carries the denial.
git init -q "$WS/repo" || fail "git init scratch repo"
printf 'cd "%s/repo" && git push --force origin main && echo pushed > "$HARNESS_ROOT/pushed.txt"' "$WS" > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit3 > "$TMP2/exec3.out" 2>&1 || fail "exec (git deny): $(cat "$TMP2/exec3.out")"
[ ! -f "$TMP2/pushed.txt" ] && ok "force push denied (command never ran)" || fail "force push ran despite git-protect"
grep '"kind":"obs/harness/hook/pre_tool_call/deny"' "$TMP2/trace.jsonl" | grep -q '"hook":"git-protect:' \
  && ok "deny echoed on obs/harness/hook" || fail "no git-protect deny in trace"
grep '"kind":"obs/harness/hook/pre_tool_call/deny"' "$TMP2/trace.jsonl" | grep -q 'force-push discards remote history' \
  && ok "deny reason names the pattern" || fail "deny reason missing"
grep '"kind":"obs/agent/main/kit3/tool/shell/result"' "$TMP2/trace.jsonl" | grep -q '"denied":true' \
  && ok "denial on the tool result (transcript-visible)" || fail "denial not on tool result"

# (d) innocuous commands pass: ordinary git, and --force-with-lease.
printf 'cd "%s/repo" && git status --short >/dev/null 2>&1; git push --force-with-lease origin main >/dev/null 2>&1; echo fine > "$HARNESS_ROOT/innocuous.txt"' "$WS" > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit4 > "$TMP2/exec4.out" 2>&1 || fail "exec (innocuous): $(cat "$TMP2/exec4.out")"
[ -f "$TMP2/innocuous.txt" ] && ok "innocuous git (incl. --force-with-lease) allowed" || fail "innocuous command blocked"

# (e) context pipeline (docs/context.md): a [[stage]] declaration is a
# REQUEST — inert until approved (loud skip on obs), then it transforms the
# document the model actually sees, and a runtime failure fails closed with
# a stage-attributed error.
mkdir -p "$TMP2/packages/stagepkg/scripts"
cat > "$TMP2/packages/stagepkg/elanus.toml" <<'EOF'
[[stage]]
name  = "marker"
run   = "scripts/stage"
order = 30
EOF
cat > "$TMP2/packages/stagepkg/scripts/stage" <<'EOF'
#!/usr/bin/env python3
import json, os, sys
if os.path.exists(os.path.join(os.environ["HARNESS_ROOT"], "stage-break")):
    sys.exit(1)
doc = json.load(sys.stdin)
doc["system"].append({"name": "marker", "text": "STAGEMARK-7c4f"})
json.dump(doc, sys.stdout)
EOF
chmod +x "$TMP2/packages/stagepkg/scripts/stage"

printf 'true' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit5 > "$TMP2/exec5.out" 2>&1 || fail "exec (unapproved stage): $(cat "$TMP2/exec5.out")"
grep -q "STAGEMARK-7c4f" "$TMP2/llm.body" && fail "unapproved stage ran" || ok "requested stage is inert (discovery is not authority)"
grep -q '"kind":"obs/agent/main/kit5/context/marker-skipped"' "$TMP2/trace.jsonl" \
  && ok "skip is loud on obs" || fail "no skipped-stage obs record"

HARNESS_ROOT="$TMP2" elanus approve stagepkg >/dev/null 2>&1 || fail "approve stagepkg"
HARNESS_ROOT="$TMP2" elanus stages | grep "stagepkg/marker" | grep -q "approved" \
  && ok "elanus stages prints the effective chain" || fail "stages listing wrong"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit6 > "$TMP2/exec6.out" 2>&1 || fail "exec (approved stage): $(cat "$TMP2/exec6.out")"
grep -q "STAGEMARK-7c4f" "$TMP2/llm.body" \
  && ok "approved stage's block reached the model" || fail "stage block missing from llm request"
grep -q '"kind":"obs/agent/main/kit6/context/marker"' "$TMP2/trace.jsonl" \
  && ok "per-stage delta on obs (camera doctrine)" || fail "no stage delta in trace"

# (f) fail-closed: same approved bytes, runtime error -> the run fails and
# the error names the stage. (An EDITED script wouldn't even run: code_hash
# pin de-approves it back to requested.)
touch "$TMP2/stage-break"
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit7 > "$TMP2/exec7.out" 2>&1 \
  && fail "broken stage did not fail the run" || ok "broken stage fails closed"
grep -q "stagepkg/marker" "$TMP2/exec7.out" && ok "failure is stage-attributed" || fail "error not stage-attributed: $(cat "$TMP2/exec7.out")"
rm -f "$TMP2/stage-break"

# (g) recent-history pending: the stock reconstruction stage ships inert —
# nothing of it reaches the prompt until approved. (Its resident round trip
# needs a broker; section 16 proves it against a live daemon.)
HARNESS_ROOT="$TMP2" elanus emit in/human/owner --payload '{"text":"remember the blue key is under the mat"}' >/dev/null 2>&1
HARNESS_ROOT="$TMP2" elanus exec "go" --session kit8 > "$TMP2/exec8.out" 2>&1 || fail "exec (recent-history pending): $(cat "$TMP2/exec8.out")"
grep -q "blue key" "$TMP2/llm.body" && fail "pending recent-history leaked into the prompt" \
  || ok "recent-history ships pending (inert until approved)"

# (h) MCP, the border protocol (src/mcp.rs): an approved [[mcp]] server's
# tools enter the model's tool array as <server>__<tool>, a call round-trips
# over stdio JSON-RPC, and changed tool descriptions (tool poisoning) are
# refused until the human re-approves (TOFU pin, cleared by decide).
mkdir -p "$TMP2/packages/adderpkg/scripts"
cat > "$TMP2/packages/adderpkg/elanus.toml" <<'EOF'
[[mcp]]
name = "adder"
run  = "scripts/server"
EOF
cat > "$TMP2/packages/adderpkg/scripts/server" <<'EOF'
#!/usr/bin/env python3
import json, sys
DESC = "add two integers"
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method")
    if msg.get("id") is None:
        continue
    if m == "initialize":
        r = {"protocolVersion": msg["params"]["protocolVersion"],
             "capabilities": {"tools": {}}, "serverInfo": {"name": "adder", "version": "0"}}
    elif m == "tools/list":
        r = {"tools": [{"name": "add", "description": DESC,
                        "inputSchema": {"type": "object",
                                        "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
                                        "required": ["a", "b"]}}]}
    elif m == "tools/call":
        a = msg["params"]["arguments"]
        r = {"content": [{"type": "text", "text": str(a["a"] + a["b"])}], "isError": False}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                     "error": {"code": -32601, "message": "nope"}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": r}) + "\n")
    sys.stdout.flush()
EOF
chmod +x "$TMP2/packages/adderpkg/scripts/server"

printf 'true' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session mcp1 > "$TMP2/execm1.out" 2>&1 || fail "exec (unapproved mcp): $(cat "$TMP2/execm1.out")"
grep -q "adder__add" "$TMP2/llm.body" && fail "unapproved mcp tools reached the model" \
  || ok "requested mcp server is inert"
HARNESS_ROOT="$TMP2" elanus approve adderpkg >/dev/null 2>&1 || fail "approve adderpkg"
printf 'TOOL:{"name":"adder__add","input":{"a":19,"b":23}}' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session mcp2 > "$TMP2/execm2.out" 2>&1 || fail "exec (mcp call): $(cat "$TMP2/execm2.out")"
grep -q '"adder__add"' "$TMP2/llm.body" && ok "mcp tools in the model's tool array" || fail "adder__add not offered to the model"
grep -q '"content":"42"' "$TMP2/llm.body" || grep -q '"content": *"42"' "$TMP2/llm.body" \
  && ok "mcp tool call round-tripped (19+23=42 in the tool_result)" || fail "tool_result 42 missing from llm request"

# Tool poisoning: change the description in place (same code_hash would be a
# lie — so this edits the file, which ALSO re-gates the grant; prove the pin
# alone by tampering the kv instead).
sqlite3 "$TMP2/harness.db" "UPDATE kv SET value='tampered' WHERE key='mcp_tools:adderpkg:adder'"
printf 'true' > "$TMP2/cmd.txt"
HARNESS_ROOT="$TMP2" elanus exec "go" --session mcp3 > "$TMP2/execm3.out" 2>&1 || fail "exec (pin mismatch): $(cat "$TMP2/execm3.out")"
grep -q "adder__add" "$TMP2/llm.body" && fail "changed tools served despite pin mismatch" \
  || ok "pin mismatch refuses the server's tools"
grep -q "TOOLS CHANGED" "$TMP2/execm3.out" && ok "refusal is loud and names the cure" || fail "no loud refusal"
HARNESS_ROOT="$TMP2" elanus approve adderpkg >/dev/null 2>&1 || fail "re-approve adderpkg"
HARNESS_ROOT="$TMP2" elanus exec "go" --session mcp4 > "$TMP2/execm4.out" 2>&1 || fail "exec (re-pinned): $(cat "$TMP2/execm4.out")"
grep -q "adder__add" "$TMP2/llm.body" && ok "re-approval re-pins (tools restored)" || fail "tools not restored after re-approval"

# (i) core kit: the harness teaching itself. Skill-only packages install
# with nothing to approve (content, not capability); the architect profile
# lands with its identity block; render proves both reach a prompt.
HARNESS_ROOT="$TMP2" elanus kit add core > "$TMP2/kitcore.out" 2>&1 || fail "kit add core: $(cat "$TMP2/kitcore.out")"
[ -f "$TMP2/profiles/architect/profile.toml" ] && ok "architect profile installed" || fail "architect profile missing"
HARNESS_ROOT="$TMP2" elanus render --profile architect --session corez > "$TMP2/render.out" 2>&1 || fail "render architect: $(cat "$TMP2/render.out")"
grep -q "strongest rung" "$TMP2/render.out" && ok "architect identity block renders" || fail "architect block missing"
grep -q "escalate" "$TMP2/render.out" && grep -q "harness-doctrine" "$TMP2/render.out" \
  && ok "core skills in the inventory (no grants needed — content, not capability)" \
  || fail "core skills missing from inventory"

echo "== 15. funnel kit: the variety ladder end to end =="
# Fresh root + its own daemon/broker: intake (daemon actor) -> sift (regex,
# zero tokens) -> scout (fake cheap LLM) -> emit_event KEEP mail. Three
# lines go in; two die on the regex rung, one survives to the scout, and
# the KEEP lands in the human's inbox carrying the original item.
TMP4=$(mktemp -d /tmp/elanus-funnel.XXXXXX)
elanus init "$TMP4" --kit "$REPO/kits/funnel" --copy > "$TMP4/init.out" 2>&1 || fail "init --kit kits/funnel: $(cat "$TMP4/init.out")"
[ -f "$TMP4/packages/funnel-intake/elanus.toml" ] && ok "funnel-intake materialized" || fail "funnel-intake not materialized"
[ -f "$TMP4/packages/funnel-sift/rules.txt" ] && ok "sift rules file shipped" || fail "rules.txt missing"
[ -f "$TMP4/profiles/scout/profile.toml" ] && ok "scout profile copied" || fail "scout profile missing"
for p in funnel-intake funnel-sift funnel-scout; do
  HARNESS_ROOT="$TMP4" elanus packages | grep -q "^$p .*granted=[1-9]" \
    && ok "$p granted at init" || fail "$p not approved by init"
done

# The scout's fake cheap model: first request answers with an emit_event
# tool call escalating the item to in/human/owner (what the scout profile's
# system block instructs); the follow-up (carrying the tool_result) answers
# the bare verdict. The item is parsed out of the prompt, not hardcoded.
cat > "$TMP4/fake_llm3.py" <<'EOF'
import json, re, sys
from http.server import HTTPServer, BaseHTTPRequestHandler
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(n).decode()
        if '"tool_result"' in body:
            content = [{"type": "text", "text": "KEEP interesting because reasons"}]
            stop = "end_turn"
        else:
            m = re.search(r'ITEM:\\n(.*?)(?:\\n|")', body)
            item = m.group(1) if m else "unparsed item"
            content = [{"type": "tool_use", "id": "tc1", "name": "emit_event",
                        "input": {"type": "in/human/owner",
                                  "payload": {"text": "KEEP: %s — interesting because reasons" % item}}}]
            stop = "tool_use"
        resp = json.dumps({"id": "msg_1", "type": "message", "role": "assistant",
                           "model": "claude-3-5-haiku-latest", "content": content,
                           "stop_reason": stop,
                           "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(resp)))
        self.end_headers()
        self.wfile.write(resp)
    def log_message(self, *a): pass
srv = HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
EOF
python3 "$TMP4/fake_llm3.py" "$TMP4/llm.port" &
LLM3_PID=$!
wait_for "funnel fake LLM bound" "[ -s '$TMP4/llm.port' ]"
LLM3_PORT=$(cat "$TMP4/llm.port")
export FAKE_LLM_KEY=dummy
# Point the kit's scout profile at the fake LLM; keep its blocks/ as shipped.
cat > "$TMP4/profiles/scout/profile.toml" <<EOF
agent = "scout"
owner = "owner"

[model]
model = "claude-3-5-haiku-latest"
max_turns = 4
base_url = "http://127.0.0.1:$LLM3_PORT"
api_key_env = "FAKE_LLM_KEY"

[skills]
include = []
EOF

# Second daemon, own port — the section-7 daemon keeps running on $BUS_PORT.
BUS_PORT2=$((BUS_PORT + 1))
lsof -ti "tcp:$BUS_PORT2" 2>/dev/null | xargs kill -9 2>/dev/null
printf 'enabled = true\nbind = "127.0.0.1:%s"\n' "$BUS_PORT2" > "$TMP4/bus.toml"
HARNESS_ROOT="$TMP4" elanus daemon --interval-ms 200 >"$TMP4/daemon.log" 2>&1 &
DAEMON2_PID=$!
sleep 1
wait_for "funnel-intake alive (retained status)" \
  "HARNESS_ROOT='$TMP4' elanus bus sub 'obs/package/funnel-intake/status' --count 1 --timeout 2 | grep -q '\"state\":\"alive\"'"

sql4() { sqlite3 "$TMP4/harness.db" "$1"; }
# Three lines: one killed by a drop rule, one falls to the default drop,
# one passes (matches the shipped `pass ...alert...` rule).
mkdir -p "$TMP4/run/pkg-funnel-intake/inbox"
cat > "$TMP4/run/pkg-funnel-intake/inbox/feed.line" <<'EOF'
heartbeat ok from node 7
routine sync completed
ALERT: reactor pressure rising
EOF
# Every line is ledgered on arrival — dropped ones included; nothing silent.
wait_for "all 3 lines ledgered as in/package/funnel/sift" \
  "[ \"\$(sql4 \"SELECT COUNT(*) FROM events WHERE type='in/package/funnel/sift'\")\" -ge 3 ]"
[ ! -e "$TMP4/run/pkg-funnel-intake/inbox/feed.line" ] && ok "consumed file removed after PUBACK" || fail "intake file not consumed"
# Verified sender: events published by a token-authed daemon actor attribute
# to that actor, not to "human".
SIFTSND=$(sql4 "SELECT sender FROM events WHERE type='in/package/funnel/sift' ORDER BY id DESC LIMIT 1")
[ "$SIFTSND" = "funnel-intake" ] && ok "daemon actor's events attributed to it (sender=funnel-intake)" || fail "sift sender was '$SIFTSND', expected funnel-intake"
# The regex rung absorbs two, escalates one.
wait_for "passing line escalated to in/agent/scout" \
  "[ \"\$(sql4 \"SELECT COUNT(*) FROM events WHERE type='in/agent/scout' AND payload LIKE '%reactor pressure%'\")\" -ge 1 ]"
NSCOUT=$(sql4 "SELECT COUNT(*) FROM events WHERE type='in/agent/scout'")
[ "$NSCOUT" = 1 ] && ok "dropped lines produced no scout work (exactly 1 escalation)" || fail "expected 1 in/agent/scout event, saw $NSCOUT"
# The scout ran and its KEEP reached the human via its own emit_event —
# mail carries the original item, and the cause chain is intact.
wait_for "KEEP landed as in/human/owner mail with the original item" \
  "sql4 \"SELECT json_extract(payload,'\\\$.text') FROM events WHERE type='in/human/owner'\" | grep -q 'reactor pressure rising'"
sql4 "SELECT json_extract(payload,'\$.text') FROM events WHERE type='in/human/owner'" | grep -q '^KEEP:' \
  && ok "mail carries the scout's verdict + reason" || fail "KEEP mail malformed"
NMAIL=$(sql4 "SELECT COUNT(*) FROM events WHERE type='in/human/owner'")
[ "$NMAIL" = 1 ] && ok "exactly one KEEP mail (no reply-mail leak from the run)" || fail "expected 1 in/human/owner event, saw $NMAIL"

echo "== 16. kit linking: elanus kit add, stale-at-dispatch, list/show =="
# A kit installed by LINK (the default): packages stay in the kit dir,
# discovery rides the default profile's package_path, and the hash pin
# means an upstream edit re-enters review in the linking root — enforced
# AT DISPATCH (fresh-hash check), so there is no window between the edit
# and the next sync (docs/security.md entry 9).
TMP5=$(mktemp -d /tmp/elanus-linkroot.XXXXXX)
KITS5=$(mktemp -d /tmp/elanus-kits.XXXXXX)
mkdir -p "$KITS5/linkkit/packages/linker/scripts"
cat > "$KITS5/linkkit/packages/linker/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/linker/go"]
[process]
mode = "exec"
run  = "scripts/main"
EOF
cat > "$KITS5/linkkit/packages/linker/scripts/main" <<'EOF'
#!/bin/sh
echo ran-v1 >> "$HARNESS_ROOT/linker.out"
EOF
chmod +x "$KITS5/linkkit/packages/linker/scripts/main"
printf '# link kit\n\nthe linking starter\n' > "$KITS5/linkkit/README.md"

elanus init "$TMP5" >/dev/null 2>&1 || fail "init link root"
HARNESS_ROOT="$TMP5" elanus kit add "$KITS5/linkkit" > "$TMP5/kitadd.out" 2>&1 \
  || fail "kit add: $(cat "$TMP5/kitadd.out")"
[ ! -e "$TMP5/packages/linker" ] && ok "linked, not copied" || fail "kit add copied despite link default"
grep -q "$KITS5/linkkit/packages" "$TMP5/profiles/default/profile.toml" \
  && ok "package_path carries the link" || fail "package_path not updated"
sql5() { sqlite3 "$TMP5/harness.db" "$1"; }
[ "$(sql5 "SELECT decided_by FROM grants WHERE package='linker' AND state='approved' LIMIT 1")" = "kit:linkkit" ] \
  && ok "grant provenance is kit:linkkit" || fail "kit provenance missing on grants"
grep -q "the linking starter" "$TMP5/kitadd.out" && ok "kit add prints the README" || fail "README not printed"
ELANUS_KIT_PATH="$KITS5" elanus kit list | grep -q '^linkkit ' \
  && ok "kit list resolves via ELANUS_KIT_PATH" || fail "kit list missed linkkit"
ELANUS_KIT_PATH="$KITS5" elanus kit show linkkit | grep -q "the linking starter" \
  && ok "kit show prints without installing" || fail "kit show failed"

# Dispatch flows through the linked dir.
BUS_PORT3=$((BUS_PORT + 2))
lsof -ti "tcp:$BUS_PORT3" 2>/dev/null | xargs kill -9 2>/dev/null
printf 'enabled = true\nbind = "127.0.0.1:%s"\n' "$BUS_PORT3" > "$TMP5/bus.toml"
HARNESS_ROOT="$TMP5" elanus daemon --interval-ms 200 >"$TMP5/daemon.log" 2>&1 &
DAEMON3_PID=$!
sleep 1
HARNESS_ROOT="$TMP5" elanus emit in/package/linker/go >/dev/null 2>&1
wait_for "linked handler dispatched" "grep -q ran-v1 '$TMP5/linker.out'"

# Upstream edit under a RUNNING daemon: the grant is pinned to the bytes,
# so the edited script must not run — not even in the same tick.
cat > "$KITS5/linkkit/packages/linker/scripts/main" <<'EOF'
#!/bin/sh
echo ran-v2 >> "$HARNESS_ROOT/linker.out"
EOF
chmod +x "$KITS5/linkkit/packages/linker/scripts/main"
HARNESS_ROOT="$TMP5" elanus emit in/package/linker/go >/dev/null 2>&1
wait_for "drift re-entered review (requested rows under new hash)" \
  "[ \"\$(sql5 \"SELECT COUNT(*) FROM grants WHERE package='linker' AND state='requested'\")\" -ge 1 ]"
sleep 1
grep -q ran-v2 "$TMP5/linker.out" 2>/dev/null \
  && fail "edited linked script ran while stale" || ok "stale at dispatch: edited code did not run"

# Re-approval heals: the same gesture as any package review.
HARNESS_ROOT="$TMP5" elanus approve linker >/dev/null 2>&1 || fail "approve linker after edit"
HARNESS_ROOT="$TMP5" elanus emit in/package/linker/go >/dev/null 2>&1
wait_for "re-approved linked handler ran new code" "grep -q ran-v2 '$TMP5/linker.out'"

# Staging (--pending, the phase-5 path): files land, requests register,
# NOTHING is granted — the commit gesture is a separate elanus approve.
mkdir -p "$KITS5/stagekit/packages/stager/scripts"
cat > "$KITS5/stagekit/packages/stager/elanus.toml" <<'EOF'
[request]
subscribe = ["in/package/stager/go"]
[process]
mode = "exec"
run  = "scripts/main"
EOF
printf '#!/bin/sh\necho staged-ran >> "$HARNESS_ROOT/stager.out"\n' > "$KITS5/stagekit/packages/stager/scripts/main"
chmod +x "$KITS5/stagekit/packages/stager/scripts/main"
HARNESS_ROOT="$TMP5" elanus kit add "$KITS5/stagekit" --pending > "$TMP5/stage.out" 2>&1 \
  || fail "kit add --pending: $(cat "$TMP5/stage.out")"
grep -q "staged stager" "$TMP5/stage.out" && ok "kit add --pending stages" || fail "no staging message"
[ "$(sql5 "SELECT COUNT(*) FROM grants WHERE package='stager' AND state='approved'")" = "0" ] \
  && ok "staging granted nothing" || fail "staging approved grants"
HARNESS_ROOT="$TMP5" elanus emit in/package/stager/go >/dev/null 2>&1
sleep 1
[ ! -f "$TMP5/stager.out" ] && ok "staged package is inert" || fail "staged package handled an event"
HARNESS_ROOT="$TMP5" elanus packages --json | python3 -c '
import json,sys
for line in sys.stdin:
    p = json.loads(line)
    if p["name"] == "stager":
        assert any(g["state"] == "requested" for g in p["grants"]), p
        break
else: raise SystemExit(1)
' && ok "packages --json carries the pending queue" || fail "packages --json wrong"
HARNESS_ROOT="$TMP5" elanus approve stager >/dev/null 2>&1 || fail "approve stager"
HARNESS_ROOT="$TMP5" elanus emit in/package/stager/go >/dev/null 2>&1
wait_for "approve committed the staged kit" "grep -q staged-ran '$TMP5/stager.out'"

# Resident stage round trip: recent-history's daemon holds a warm read-only
# sqlite handle and answers consults over the bus (docs/context.md) — prior
# mail in the ledger reaches the model as a system block, the forgetting
# model's deliberate re-injection. Fail-closed transport: the kernel would
# fail the run if the daemon vanished mid-consult; here we prove the happy
# path against this root's live broker.
python3 "$TMP2/fake_llm2.py" "$TMP5/llm.port" "$TMP5/cmd.txt" "$TMP5/llm.body" &
LLM5_PID=$!
wait_for "link-root fake LLM bound" "[ -s '$TMP5/llm.port' ]"
cat > "$TMP5/profiles/default/profile.toml" <<EOF
agent = "main"
owner = "owner"
package_path = ["packages"]

[model]
model = "claude-3-5-haiku-latest"
max_turns = 4
base_url = "http://127.0.0.1:$(cat "$TMP5/llm.port")"
api_key_env = "FAKE_LLM_KEY"
EOF
export FAKE_LLM_KEY=dummy
HARNESS_ROOT="$TMP5" elanus emit in/human/owner --payload '{"text":"the launch code is petrichor-9"}' >/dev/null 2>&1
HARNESS_ROOT="$TMP5" elanus approve recent-history >/dev/null 2>&1 || fail "approve recent-history"
wait_for "recent-history daemon serving (parked -> approved, no restart)" \
  "grep -q 'serving obs/harness/stagereq' '$TMP5/run/pkg-recent-history/stderr.log'"
printf 'true' > "$TMP5/cmd.txt"
HARNESS_ROOT="$TMP5" elanus exec "go" --session res1 > "$TMP5/execr.out" 2>&1 || fail "exec (resident stage): $(cat "$TMP5/execr.out")"
grep -q "petrichor-9" "$TMP5/llm.body" \
  && ok "resident consult round-tripped (ledger mail in the prompt)" || fail "resident stage block missing from llm request"
grep -q '"kind":"obs/agent/main/res1/context/recent-history"' "$TMP5/trace.jsonl" \
  && ok "resident stage delta on obs" || fail "no resident stage delta in trace"
grep -q "stagereq" "$TMP5/trace.jsonl" \
  && fail "stage RPC leaked into the flight recorder" || ok "stage RPC never recorded (carve-out holds)"
kill -9 "$LLM5_PID" 2>/dev/null

echo "== 17. history over HTTP: negotiated port, granted serving, query DSL =="
# The reconstruction view moved off the bus (HANDOFF phase 3): the daemon
# assigns a loopback port (process.http), records it in run/pkg-history/
# http.json (discovery from harness state, security.md entry 11), and the
# package PARKS until the http grant is approved — transcripts are the
# crown jewels (entry 10). The main root's daemon is still running.
[ -f "$TMP/packages/history/elanus.toml" ] || cp -R "$REPO/packages/history" "$TMP/packages/"
wait_for "http port negotiated (run/pkg-history/http.json)" "[ -s '$TMP/run/pkg-history/http.json' ]"
HPORT=$(python3 -c "import json;print(json.load(open('$TMP/run/pkg-history/http.json'))['port'])")
# Parked before approval: the port is reserved but nothing serves.
curl -s -m 2 "http://127.0.0.1:$HPORT/healthz" >/dev/null 2>&1 \
  && fail "history served before the http grant was approved" \
  || ok "parked until approved (serving is a capability)"
elanus approve history >/dev/null 2>&1 || fail "approve history"
wait_for "history serving after approval (no restart dance)" \
  "curl -s -m 2 'http://127.0.0.1:$HPORT/healthz' | grep -q '\"ok\": *true'"
# A real query: sessions exist from the earlier sections.
curl -s "http://127.0.0.1:$HPORT/query" -d '{"kind":"sessions"}' | grep -q '"sessions"' \
  && ok "sessions query answers" || fail "sessions query failed"
# The DSL: filter x projection x pagination, interpreted (never SQL).
curl -s "http://127.0.0.1:$HPORT/query" \
  -d '{"kind":"search","filter":{"roles":["user"]},"page":{"limit":2}}' \
  | grep -q '"role": *"user"' && ok "search DSL: role filter" || fail "search DSL failed"
curl -s "http://127.0.0.1:$HPORT/query" -d '{"kind":"search","filter":{"roles":["nope"]}}' \
  | grep -q '"ok": *false' && ok "bad DSL rejected, not executed" || fail "bad DSL not rejected"

echo "== 18. phonebook: identity directory, HTTP reads + bus writes (verified provenance) =="
# An identity is reachable through many channels; the phonebook records which
# channel belongs to whom (docs/identity.md). Reads over HTTP like history;
# WRITES over the authenticated bus so a link's provenance is the
# broker-verified sender, never a payload field. The store is the phonebook's
# OWN sqlite in its scratch, never harness.db. Ships PENDING; parks until
# approved. The main root's daemon is still running and picks it up.
[ -f "$TMP/packages/phonebook/elanus.toml" ] || cp -R "$REPO/packages/phonebook" "$TMP/packages/"
wait_for "phonebook http port negotiated (run/pkg-phonebook/http.json)" "[ -s '$TMP/run/pkg-phonebook/http.json' ]"
PBPORT=$(python3 -c "import json;print(json.load(open('$TMP/run/pkg-phonebook/http.json'))['port'])")
curl -s -m 2 "http://127.0.0.1:$PBPORT/healthz" >/dev/null 2>&1 \
  && fail "phonebook served before approval" || ok "parked until approved (serving + writing is a capability)"
elanus approve phonebook >/dev/null 2>&1 || fail "approve phonebook"
wait_for "phonebook serving after approval" \
  "curl -s -m 2 'http://127.0.0.1:$PBPORT/healthz' | grep -q '\"ok\": *true'"
# Writes go over the bus; the CLI authenticates as the owner identity, so the
# broker stamps provenance "owner" — an unforgeable claim, not a chosen field.
elanus bus pub in/package/phonebook/identity '{"id":"tim","kind":"human","canonical":"Tim"}' --qos 1 >/dev/null || fail "pub identity"
elanus bus pub in/package/phonebook/channel '{"channel_kind":"bluesky","address":"@tim","identity":"tim","confidence":1.0}' --qos 1 >/dev/null || fail "pub channel"
elanus bus pub in/package/phonebook/channel '{"channel_kind":"discord","address":"tim#1"}' --qos 1 >/dev/null || fail "pub unresolved channel"
wait_for "channel resolves to its identity" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"resolve\",\"channel_kind\":\"bluesky\",\"address\":\"@tim\"}' | grep -q '\"id\": *\"tim\"'"
curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"resolve","channel_kind":"bluesky","address":"@tim"}' | grep -q '"provenance": *"owner"' \
  && ok "link provenance = broker-verified sender (not a payload field)" || fail "provenance is not the verified sender"
# A sighting recorded before it is matched — the matcher's work queue.
curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"resolve","channel_kind":"discord","address":"tim#1"}' | grep -q '"resolved": *false' \
  && ok "unresolved channel recorded before it is matched" || fail "unresolved sighting missing"
# Link it later -> retroactive resolution (the guess was never frozen).
elanus bus pub in/package/phonebook/link '{"channel_kind":"discord","address":"tim#1","identity":"tim","confidence":0.7}' --qos 1 >/dev/null || fail "pub link"
wait_for "a late link resolves the earlier sighting" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"resolve\",\"channel_kind\":\"discord\",\"address\":\"tim#1\"}' | grep -q '\"resolved\": *true'"
# Non-destructive merge, then split reverts it (channels never move).
elanus bus pub in/package/phonebook/identity '{"id":"timothy","kind":"human","canonical":"Timothy"}' --qos 1 >/dev/null || fail "pub identity2"
elanus bus pub in/package/phonebook/channel '{"channel_kind":"email","address":"t@x","identity":"timothy","confidence":1.0}' --qos 1 >/dev/null || fail "pub email"
elanus bus pub in/package/phonebook/merge '{"from":"timothy","into":"tim"}' --qos 1 >/dev/null || fail "pub merge"
wait_for "a merged identity's channel resolves to the survivor" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"resolve\",\"channel_kind\":\"email\",\"address\":\"t@x\"}' | grep -q '\"id\": *\"tim\"'"
elanus bus pub in/package/phonebook/split '{"id":"timothy"}' --qos 1 >/dev/null || fail "pub split"
wait_for "split reverts the channel to its origin (merge was non-destructive)" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"resolve\",\"channel_kind\":\"email\",\"address\":\"t@x\"}' | grep -q '\"id\": *\"timothy\"'"
# Names: alias + the human-facing whois lookup + the full identity record.
elanus bus pub in/package/phonebook/alias '{"identity":"tim","name":"tk"}' --qos 1 >/dev/null || fail "pub alias"
wait_for "whois finds an identity by a (non-unique) name" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"whois\",\"name\":\"tk\"}' | grep -q '\"canonical\": *\"tim\"'"
curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"identity","id":"tim"}' | grep -q '"name": *"tk"' \
  && ok "identity record lists channels and aliases" || fail "identity record missing alias"
# The matcher's work queue: an unresolved sighting shows in channels{resolved:false}.
elanus bus pub in/package/phonebook/channel '{"channel_kind":"sms","address":"+1555"}' --qos 1 >/dev/null || fail "pub sighting"
wait_for "an unresolved sighting appears in the work queue" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"channels\",\"resolved\":false}' | grep -q '\"+1555\"'"
# Regression (data integrity): a bare re-sighting of an already-linked channel
# must NOT wipe the prior link's confidence (bluesky/@tim was linked at 1.0).
elanus bus pub in/package/phonebook/channel '{"channel_kind":"bluesky","address":"@tim"}' --qos 1 >/dev/null || fail "pub re-sighting"
sleep 0.5
curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"resolve","channel_kind":"bluesky","address":"@tim"}' | grep -q '"confidence": *1' \
  && ok "bare re-sighting preserves the prior link's confidence" || fail "re-sighting wiped the link's confidence"
# Regression (crash-DoS): a malformed write is answered with an error, and the
# daemon stays up — a single bad publish must not crash-loop the service.
( elanus bus sub obs/package/phonebook/result --count 1 --timeout 6 > "$TMP/pb_result.out" 2>/dev/null ) &
PBSUB=$!
sleep 0.5
elanus bus pub in/package/phonebook/link '{"channel_kind":"x","address":"y","identity":"tim","confidence":{}}' --qos 1 >/dev/null || fail "pub malformed"
wait "$PBSUB" 2>/dev/null
grep -q '"ok": *false' "$TMP/pb_result.out" \
  && ok "malformed write answered with an error result" || fail "no error result for a malformed write"
curl -s -m 2 "http://127.0.0.1:$PBPORT/healthz" | grep -q '"ok": *true' \
  && ok "daemon survived the malformed write (no crash-loop)" || fail "daemon died on a malformed write"
curl -s "http://127.0.0.1:$PBPORT/query" -d '{"kind":"nope"}' | grep -q '"ok": *false' \
  && ok "unknown query kind rejected, not executed" || fail "unknown kind not rejected"
[ -s "$TMP/run/pkg-phonebook/phonebook.db" ] \
  && ok "phonebook owns its store (scratch, not harness.db)" || fail "phonebook.db missing in scratch"

echo "== 19. recall: the unified cross-channel frame (resolve-at-recall, provenance-gated) =="
# A resident context stage: when a message arrives on one channel, pull the
# whole conversation with that PERSON across every channel the phonebook knows
# them by (docs/identity.md). Topics stay channel-faithful; unification is a
# query-time join over the phonebook (HTTP) and the ledger. The correspondent
# is taken ONLY from the broker-verified topic, never a body field, and never
# from an event the agent emitted itself — so a prompt-injected agent cannot
# pull another person's history. (Phonebook from section 18 is serving.)
[ -f "$TMP/packages/recall/elanus.toml" ] || cp -R "$REPO/packages/recall" "$TMP/packages/"
elanus approve recall >/dev/null 2>&1 || fail "approve recall"
wait_for "recall daemon serving (parked -> approved)" "grep -q 'recall\] serving' '$TMP/run/pkg-recall/stderr.log'"
# A body-capturing LLM (reused from section 14) so we can prove the unified
# frame reaches the ACTUAL prompt, plus a profile that points at it.
python3 "$TMP2/fake_llm2.py" "$TMP/llm3.port" "$TMP/cmd3.txt" "$TMP/llm3.body" &
LLM6_PID=$!
wait_for "recall fake LLM bound" "[ -s '$TMP/llm3.port' ]"
printf 'true' > "$TMP/cmd3.txt"
mkdir -p "$TMP/profiles/recalltest"
cat > "$TMP/profiles/recalltest/profile.toml" <<EOF
agent = "main"
owner = "owner"
[model]
model = "claude-3-5-haiku-latest"
max_turns = 4
base_url = "http://127.0.0.1:$(cat "$TMP/llm3.port")"
api_key_env = "FAKE_LLM_KEY"
EOF
# Cara is one person on two channels; record her and seed a message on each.
elanus bus pub in/package/phonebook/identity '{"id":"cara","kind":"human","canonical":"Cara"}' --qos 1 >/dev/null || fail "pub cara"
elanus bus pub in/package/phonebook/channel '{"channel_kind":"bluesky","address":"@cara","identity":"cara","confidence":1.0}' --qos 1 >/dev/null || fail "link cara bluesky"
elanus bus pub in/package/phonebook/channel '{"channel_kind":"discord","address":"cara#7","identity":"cara","confidence":1.0}' --qos 1 >/dev/null || fail "link cara discord"
elanus bus pub "in/dm/bluesky/@cara" '{"text":"hello from bluesky"}' --qos 1 >/dev/null || fail "ingress bluesky"
elanus bus pub "in/dm/discord/cara%237" '{"text":"and from discord"}' --qos 1 >/dev/null || fail "ingress discord"
wait_for "phonebook resolved the correspondent" \
  "curl -s 'http://127.0.0.1:$PBPORT/query' -d '{\"kind\":\"resolve\",\"channel_kind\":\"bluesky\",\"address\":\"@cara\"}' | grep -q '\"id\": *\"cara\"'"
# (a) Genuine ingress (verified sender is a bridge, not the agent): recall
# unifies BOTH channels into the real prompt — proves the query-time join and
# that the percent-encoded discord address actually matched the ledger.
EVR=$(elanus emit obs/e2e/recall-anchor)
printf '{"id":%s,"type":"in/dm/bluesky/@cara","sender":"bluesky-bridge","payload":{"prompt":"hi","profile":"recalltest","session":"recall1"}}' "$EVR" | \
  HARNESS_EVENT_ID="$EVR" FAKE_LLM_KEY=dummy elanus handle-exec >/dev/null 2>&1
grep -q "hello from bluesky" "$TMP/llm3.body" && grep -q "and from discord" "$TMP/llm3.body" \
  && ok "recall unified BOTH channels into the prompt (resolve-at-recall; encoding matched)" \
  || fail "recall did not unify both channels into the prompt"
# (b) SECURITY: a self-forged dispatch (verified sender == the running agent)
# must recall NOTHING — an injected agent cannot name a correspondent and pull
# their confidential history into its own prompt.
: > "$TMP/llm3.body"
EVR2=$(elanus emit obs/e2e/recall-anchor2)
printf '{"id":%s,"type":"in/dm/bluesky/@cara","sender":"main","payload":{"prompt":"hi","profile":"recalltest","session":"recall2"}}' "$EVR2" | \
  HARNESS_EVENT_ID="$EVR2" FAKE_LLM_KEY=dummy elanus handle-exec >/dev/null 2>&1
grep -q "hello from bluesky" "$TMP/llm3.body" \
  && fail "recall leaked history on a self-forged (sender==agent) dispatch" \
  || ok "self-forged dispatch recalls nothing (provenance gate holds)"
# (c) DEGRADE: an unresolved channel (no phonebook link) still recalls its own
# single thread — best-effort, never a failed run.
elanus bus pub "in/dm/sms/%2B99" '{"text":"lone sms message"}' --qos 1 >/dev/null || fail "ingress sms"
: > "$TMP/llm3.body"
EVR3=$(elanus emit obs/e2e/recall-anchor3)
printf '{"id":%s,"type":"in/dm/sms/%%2B99","sender":"sms-bridge","payload":{"prompt":"hi","profile":"recalltest","session":"recall3"}}' "$EVR3" | \
  HARNESS_EVENT_ID="$EVR3" FAKE_LLM_KEY=dummy elanus handle-exec >/dev/null 2>&1
grep -q "lone sms message" "$TMP/llm3.body" \
  && ok "unresolved channel still recalls its own thread (degrade, not fail)" \
  || fail "degrade path did not recall the single channel"
kill -9 "$LLM6_PID" 2>/dev/null

echo "== 20. egress doctrine: a direct send + an obs record (no out/ plane) =="
# Egress is command-shaped (docs/actors.md): it goes DIRECT (here an HTTP POST),
# not through the bus as transport — but the send emits an obs/ record so the
# flight recorder stays whole. The webhook package is the worked exemplar.
cat > "$TMP/webhook_stub.py" <<'EOF'
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        open(sys.argv[2], "wb").write(self.rfile.read(n))   # record what was delivered
        self.send_response(200); self.end_headers()
    def log_message(self, *a): pass
srv = HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(srv.server_address[1]))
srv.serve_forever()
EOF
python3 "$TMP/webhook_stub.py" "$TMP/wh.port" "$TMP/wh.received" &
WH_PID=$!
wait_for "webhook stub bound" "[ -s '$TMP/wh.port' ]"
# webhook is a DAEMON bridge (its own identity), so its receipt attributes to
# IT, not the owner — that is the provenance the egress record carries.
[ -f "$TMP/packages/webhook/elanus.toml" ] || cp -R "$REPO/packages/webhook" "$TMP/packages/"
elanus approve webhook >/dev/null 2>&1 || fail "approve webhook"
wait_for "webhook bridge serving (parked -> approved)" "grep -q 'webhook\] serving' '$TMP/run/pkg-webhook/stderr.log'"
# Capture the egress record off the bus, then trigger a send (correlated).
( elanus bus sub obs/channel/webhook/sent --count 1 --timeout 10 > "$TMP/wh.receipt" 2>/dev/null ) &
WHSUB=$!
sleep 0.5
elanus emit "in/package/webhook/send" --correlation wh-corr-1 --payload "{\"url\":\"http://127.0.0.1:$(cat "$TMP/wh.port")/hook\",\"text\":\"ping-egress\"}" >/dev/null 2>&1
wait_for "the send was delivered DIRECTLY (off the bus)" "grep -q ping-egress '$TMP/wh.received' 2>/dev/null"
ok "egress went direct (HTTP POST), not relayed over the bus"
wait "$WHSUB" 2>/dev/null
grep -q '"ok": *true' "$TMP/wh.receipt" \
  && ok "the send emitted an obs record (bus stays the record plane)" || fail "no egress obs record"
grep -q '"sender": *"webhook"' "$TMP/wh.receipt" \
  && ok "the receipt is attributed to the bridge (sender=webhook, not the owner)" || fail "egress receipt misattributed: $(cat "$TMP/wh.receipt")"
grep -q "wh-corr-1" "$TMP/wh.receipt" \
  && ok "the receipt is causally linked to the request (correlation threaded)" || fail "receipt not correlated to the request"
# out/ stays absent: sending is an inbox + a direct send + an obs record.
[ -z "$(sql "SELECT id FROM events WHERE type LIKE 'out/%' LIMIT 1")" ] \
  && ok "no out/ plane anywhere (the egress asymmetry holds)" || fail "an out/ event exists"
kill -9 "$WH_PID" 2>/dev/null

echo
if [ "$FAILS" -eq 0 ]; then
  echo "ALL PASS (root: $TMP)"
else
  echo "$FAILS FAILURE(S) (root kept for inspection: $TMP)"
  exit 1
fi

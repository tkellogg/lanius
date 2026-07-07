#!/usr/bin/env bash
# M1 reproduction probes for docs/handoffs/bus-resilience.md.
#
# Two failure reports, both root-cause-first (the fix is NOT in this script —
# this is the reproduction that decided M2/M3 scope):
#
#   (a) "if the coding agent can't reach the MQTT broker it dies."
#       -> NOT reproducible: the coding agent is already soft on broker-down.
#          Every bus publish funnels through publish_obs, which swallows the
#          error; mint + capture are local sqlite; the session runs to its own
#          exit status and its work is recorded to disk. This probe PROVES that
#          (exit 0, claim row present) AND exhibits the real defect the report
#          was feeling: the stderr DRIP (one "obs publish ... failed" line per
#          publish — five for a one-turn echo). M2 collapses that drip into one
#          once-per-session warning + a reconnect line, and adds a typed
#          down-vs-denied distinction (today both surface as the same untyped
#          "connection failed (daemon running?)").
#
#   (b) "a fresh session replays a prior prompt."  QoS1/retained is exonerated
#       by construction (ledger-driven delivery, session-scoped topics, no
#       persistent broker sessions). The live mechanism is ledger-side: a
#       pending in/agent/* delivery has NO time bound, so old mail fires the
#       instant its target session id resolves again. That mechanism is pinned
#       down deterministically in the Rust regression test
#       `drive_holds_stale_delivery_and_drives_fresh` (src/dispatcher.rs) — a
#       ledger seed needs no live TUI, so it lives with the other dispatcher
#       tests rather than here.
#
# Containment: everything runs under a scratch ELANUS_ROOT; no daemon is left
# running; the live root (~/.lanius) is never touched.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/bus-resilience-repro.XXXXXX")"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

BIN="$REPO/target/debug/lanius"
ECHO="$REPO/target/debug/examples/harness_echo"
if [[ ! -x "$BIN" || ! -x "$ECHO" ]]; then
  echo "building lanius + harness_echo ..."
  ( cd "$REPO" && cargo build --bin lanius --example harness_echo ) || exit 1
fi

ROOT="$SCRATCH/root"; WORK="$SCRATCH/work"
mkdir -p "$ROOT/packages/harness-echo/bin" "$WORK"
cp "$ECHO" "$ROOT/packages/harness-echo/bin/adapter"
chmod +x "$ROOT/packages/harness-echo/bin/adapter"
cat > "$ROOT/packages/harness-echo/lanius.toml" <<'TOML'
[[harness]]
name = "echo"
aliases = ["ec"]
run = "bin/adapter"
TOML

export ELANUS_ROOT="$ROOT"
fail=0

echo "=== (a) coding agent with the broker DOWN (no daemon) ==="
( cd "$WORK" && "$BIN" code echo --headless "hello broker-down" --no-brief ) \
  >"$SCRATCH/a.out" 2>"$SCRATCH/a.err"
rc=$?
echo "exit=$rc"
if [[ $rc -ne 0 ]]; then
  echo "FAIL(a): the session did NOT survive broker-down (exit $rc)"; fail=1
else
  echo "ok(a): session survived broker-down, exited by the tool's own status"
fi
if command -v sqlite3 >/dev/null 2>&1; then
  claim="$(sqlite3 "$ROOT/lanius.db" \
    "SELECT c.path FROM code_claims c JOIN code_sessions s ON s.elanus_session=c.session LIMIT 1;" 2>/dev/null)"
  if [[ -n "$claim" ]]; then
    echo "ok(a): work captured to disk despite broker-down ($claim)"
  else
    echo "FAIL(a): nothing captured to disk"; fail=1
  fi
fi
# M2 contract: the stderr drip (pre-M2: one "obs publish ... failed" line per
# publish) is collapsed to exactly ONE once-per-session warning, and it names an
# uncaptured session (not "is the daemon running?" — that wording is reserved for
# the loud auth arm).
drip=$(grep -c "obs publish .* failed" "$SCRATCH/a.err" 2>/dev/null); drip=${drip:-0}
warns=$(grep -c "can't reach the message bus" "$SCRATCH/a.err" 2>/dev/null); warns=${warns:-0}
if [[ "$drip" -eq 0 && "$warns" -eq 1 ]]; then
  echo "ok(a): exactly one soft-degrade warning, no per-publish drip (M2)"
else
  echo "FAIL(a): expected 1 warning + 0 drip, got warns=$warns drip=$drip"; fail=1
fi

echo
echo "=== (b) is a ledger-seed reproduction; see the dispatcher regression test ==="
echo "    cargo test -p lanius drive_holds_stale_delivery_and_drives_fresh"

exit $fail

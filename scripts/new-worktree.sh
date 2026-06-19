#!/usr/bin/env bash
# Create an isolated elanus dev worktree on shifted ports, so a second agent can
# run the full stack (daemon + web relay + Vite UI) without colliding with the
# stack in the main checkout.
#
# Isolation boundary is the elanus *root* (its own elanus.db, bus.toml, secrets,
# config repo). Each slot N shifts three ports: web relay, Vite, and the MQTT
# broker bind (the last lives in <root>/bus.toml, not a dev flag). Slot 0 is the
# main stack (7180 / 5173 / 1883).
#
# Usage:  scripts/new-worktree.sh <name> [slot]
#   <name>   branch + worktree suffix, e.g. coding-agents
#   [slot]   port-shift slot, default 1 (use 2, 3, ... for more worktrees)
#
# Tear down later:
#   git worktree remove ../elanus-<name> && rm -rf ~/.elanus/wt-<name>
set -euo pipefail

NAME="${1:?usage: new-worktree.sh <name> [slot]}"
N="${2:-1}"

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
PARENT="$(dirname "$REPO")"
WT="$PARENT/elanus-$NAME"
BRANCH="$NAME"
ROOT="$HOME/.elanus/wt-$NAME"
ELANUS_BIN="$REPO/target/debug/elanus"

WEB=$((7180 + N * 100))
VITE=$((5173 + N * 100))
BROKER=$((1883 + N * 10))

echo "→ worktree: $WT  (branch $BRANCH)"
echo "→ root:     $ROOT"
echo "→ ports:    web $WEB · vite $VITE · broker $BROKER  (slot $N)"

# 1. the worktree itself, on a fresh branch off current HEAD
git -C "$REPO" worktree add "$WT" -b "$BRANCH"

# 2. carry the gitignored local files an agent + the daemon need
for f in .env CLAUDE.md; do
  if [ -e "$REPO/$f" ]; then cp "$REPO/$f" "$WT/$f" && echo "  copied $f"; fi
done

# 3. web deps: reuse the main checkout's node_modules (same lockfile/commit)
if [ -d "$REPO/ui/web/node_modules" ]; then
  ln -s "$REPO/ui/web/node_modules" "$WT/ui/web/node_modules"
  echo "  linked ui/web/node_modules"
else
  (cd "$WT/ui/web" && npm ci)
fi

# 4. an isolated root on a shifted broker port. NOTE: `init` takes the root as a
# POSITIONAL arg (positional > $ELANUS_ROOT > ~/.elanus/root); the global --root
# flag does NOT apply to init, so `--root X init` would scaffold the default root.
mkdir -p "$ROOT"
if [ -x "$ELANUS_BIN" ]; then
  "$ELANUS_BIN" init "$ROOT"
else
  (cd "$REPO" && cargo run --quiet -- init "$ROOT")
fi
# shift the broker bind — patch if init wrote bus.toml, else create it
if [ -f "$ROOT/bus.toml" ] && grep -qE '^[[:space:]]*bind[[:space:]]*=' "$ROOT/bus.toml"; then
  sed -i.bak -E "s|^[[:space:]]*bind[[:space:]]*=.*|bind = \"127.0.0.1:$BROKER\"|" "$ROOT/bus.toml"
  rm -f "$ROOT/bus.toml.bak"
else
  printf 'bind = "127.0.0.1:%s"\n' "$BROKER" >"$ROOT/bus.toml"
fi
# creds for the daemon's model calls (dotenv loads <root>/.env)
[ -e "$REPO/.env" ] && cp "$REPO/.env" "$ROOT/.env"

cat <<EOF

✔ ready. start the stack:
    cd "$WT" && cargo run -- --root "$ROOT" dev --web-port $WEB --vite-port $VITE
  then open  http://127.0.0.1:$VITE   (first run does a fresh cargo build)

  tear down later:
    git -C "$REPO" worktree remove "$WT" && rm -rf "$ROOT"
EOF

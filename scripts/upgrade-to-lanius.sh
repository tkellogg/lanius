#!/usr/bin/env bash
set -euo pipefail

log() {
  printf '%s\n' "$*"
}

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ARG="${1:-}"
if [ -n "${REPO_ARG:-}" ]; then
  REPO="$(CDPATH= cd -- "$REPO_ARG" && pwd)"
else
  REPO="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
fi

HOME_DIR="${HOME:?HOME must be set}"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME_DIR/.cargo}/bin"
DRY_RUN="${LANIUS_UPGRADE_DRY_RUN:-0}"
LANIUS_BIN="$CARGO_BIN_DIR/lanius"
ELANUS_BIN="$CARGO_BIN_DIR/elanus"
OLD_ROOT="${ELANUS_ROOT:-$HOME_DIR/.elanus/root}"
NEW_ROOT="${LANIUS_ROOT:-$HOME_DIR/.lanius/root}"
OLD_KITS="$HOME_DIR/.elanus/kits"
NEW_KITS="$HOME_DIR/.lanius/kits"
TS="$(date +%s)"
stopped_any=0
ACTIVE_ROOT="$NEW_ROOT"
ACTIVE_KITS="$NEW_KITS"

maybe() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: $*"
    return 0
  fi
  "$@"
}

maybe_mv() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: mv $1 $2"
    return 0
  fi
  mv "$1" "$2"
}

maybe_cp() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: cp $1 $2"
    return 0
  fi
  cp "$1" "$2"
}

maybe_ln() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: ln -s $1 $2"
    return 0
  fi
  ln -s "$1" "$2"
}

maybe_rm() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: rm -f $*"
    return 0
  fi
  rm -f "$@"
}

maybe_mkdir_p() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: mkdir -p $1"
    return 0
  fi
  mkdir -p "$1"
}

backup_db_tree() {
  local root="$1"
  local found=0
  for db in "$root/lanius.db" "$root/elanus.db" "$root/harness.db"; do
    [ -f "$db" ] || continue
    found=1
    for suffix in "" "-wal" "-shm"; do
      local src="${db}${suffix}"
      [ -e "$src" ] || continue
      local bak="${src}.bak-${TS}"
      if [ "$DRY_RUN" -eq 1 ]; then
        log "dry-run: back up $(basename "$src") -> ${bak}"
      else
        cp -p "$src" "$bak"
        log "backed up $(basename "$src") -> ${bak}"
      fi
    done
  done
  if [ "$found" -eq 0 ]; then
    log "no database found in $root — skipping backup"
  fi
}

stop_pid() {
  local pid="$1"
  local cmd
  cmd="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  if [ -n "$cmd" ]; then
    log "stopping pid $pid: $cmd"
  else
    log "stopping pid $pid"
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: would send SIGTERM to $pid"
    return 0
  fi
  kill -TERM "$pid" 2>/dev/null || true
  for _ in 1 2 3 4 5; do
    if ! kill -0 "$pid" 2>/dev/null; then
      log "stopped pid $pid"
      return 0
    fi
    sleep 1
  done
  log "pid $pid still alive after SIGTERM; sending SIGKILL"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "dry-run: would send SIGKILL to $pid"
    return 0
  fi
  kill -KILL "$pid" 2>/dev/null || true
}

stop_matching_processes() {
  local pids=""
  local pid cmd root_reason
  while IFS= read -r pid cmd; do
    [ -n "$pid" ] || continue
    case "$cmd" in
      *"$OLD_ROOT"*) root_reason="$OLD_ROOT" ;;
      *"$NEW_ROOT"*) root_reason="$NEW_ROOT" ;;
      *) root_reason="" ;;
    esac
    case "$cmd" in
      *"lanius daemon --interval-ms"*|*"elanus daemon --interval-ms"*|*"lanius serve"*|*"elanus serve"*|*"ui/web/server.mjs"*)
        if [ -n "$root_reason" ]; then
          case " $pids " in
            *" $pid "*) ;;
            *) pids="$pids $pid" ;;
          esac
        else
          log "skipping candidate pid $pid (no intended root marker): $cmd"
        fi
        ;;
    esac
  done < <(ps -axo pid=,command= 2>/dev/null || true)
  if [ -z "${pids// }" ]; then
    log "daemon not running — nothing to stop"
    return 0
  fi
  stopped_any=1
  for pid in $pids; do
    stop_pid "$pid"
  done
}

rename_manifest_files() {
  local base="$1"
  [ -d "$base" ] || return 0
  local path dir target
  while IFS= read -r -d '' path; do
    dir="$(dirname "$path")"
    target="$dir/lanius.toml"
    if [ -e "$target" ]; then
      log "already migrated manifest in $dir — skipping"
      continue
    fi
    maybe_mv "$path" "$target"
    log "renamed $(basename "$path") -> lanius.toml in $dir"
  done < <(find "$base" -type f -name 'elanus.toml' -print0)
}

rewrite_stale_root_refs() {
  local base="$1"
  [ -d "$base" ] || return 0
  local old_prefix="$HOME_DIR/.elanus"
  local file tmp
  while IFS= read -r -d '' file; do
    case "$file" in
      *.db|*.sqlite|*.sqlite3|*.png|*.jpg|*.jpeg|*.webp|*.gif)
        log "flagging binary-ish stale reference in $file for manual review"
        continue
        ;;
    esac
    tmp="${file}.upgrade.$$"
    if sed "s|$old_prefix|$HOME_DIR/.lanius|g" "$file" >"$tmp"; then
      maybe_mv "$tmp" "$file"
      log "rewrote stale root path in $file"
    else
      maybe_rm "$tmp"
      log "could not safely rewrite $file — inspect manually"
    fi
  done < <(grep -rl -- "$old_prefix" "$base" 2>/dev/null || true)
}

log "repo: $REPO"
log "old root: $OLD_ROOT"
log "new root: $NEW_ROOT"
log "old kits: $OLD_KITS"
log "new kits: $NEW_KITS"

log "step 1: back up ledgers"
backup_db_tree "$NEW_ROOT"
if [ "$OLD_ROOT" != "$NEW_ROOT" ]; then
  backup_db_tree "$OLD_ROOT"
fi

log "step 2: stop running daemon/web processes"
stop_matching_processes

log "step 3: install the new binary"
if [ "$DRY_RUN" -eq 1 ]; then
  log "dry-run: skipping cargo install --path \"$REPO\" --force"
elif [ -x "$LANIUS_BIN" ]; then
  cargo install --path "$REPO" --force
else
  cargo install --path "$REPO" --force
fi
if [ -x "$LANIUS_BIN" ]; then
  "$LANIUS_BIN" --version
else
  log "binary not yet present at $LANIUS_BIN — version check skipped"
fi

log "step 4: migrate the root"
if [ -d "$OLD_ROOT" ] && [ ! -e "$NEW_ROOT" ]; then
  maybe_mkdir_p "$(dirname "$NEW_ROOT")"
  maybe_mv "$OLD_ROOT" "$NEW_ROOT"
  log "moved $OLD_ROOT -> $NEW_ROOT"
elif [ -e "$OLD_ROOT" ] && [ -e "$NEW_ROOT" ]; then
  log "both $OLD_ROOT and $NEW_ROOT exist — refusing to clobber"
elif [ -d "$NEW_ROOT" ]; then
  log "already migrated root at $NEW_ROOT"
else
  log "no root found at $OLD_ROOT or $NEW_ROOT"
fi

if [ "$DRY_RUN" -eq 1 ] && [ -d "$OLD_ROOT" ] && [ ! -e "$NEW_ROOT" ]; then
  ACTIVE_ROOT="$OLD_ROOT"
fi
if [ "$DRY_RUN" -eq 1 ] && [ -d "$OLD_KITS" ] && [ ! -e "$NEW_KITS" ]; then
  ACTIVE_KITS="$OLD_KITS"
fi
log "effective root for remaining steps: $ACTIVE_ROOT"
log "effective kits for remaining steps: $ACTIVE_KITS"

log "step 5: migrate the ledger filename"
if [ -d "$ACTIVE_ROOT" ]; then
  if [ -e "$ACTIVE_ROOT/lanius.db" ]; then
    log "already migrated ledger in $ACTIVE_ROOT"
  else
    for legacy in "$ACTIVE_ROOT/elanus.db" "$ACTIVE_ROOT/harness.db"; do
      if [ -e "$legacy" ]; then
        maybe_mv "$legacy" "$ACTIVE_ROOT/lanius.db"
        log "renamed $(basename "$legacy") -> lanius.db"
        for suffix in -wal -shm; do
          if [ -e "$legacy$suffix" ]; then
            maybe_mv "$legacy$suffix" "$ACTIVE_ROOT/lanius.db$suffix"
            log "renamed $(basename "$legacy$suffix") -> lanius.db$suffix"
          fi
        done
        break
      fi
    done
  fi
fi

log "step 6: migrate manifest filenames"
rename_manifest_files "$ACTIVE_ROOT"

log "step 6b: migrate user-level kits"
if [ -d "$OLD_KITS" ] && [ ! -e "$NEW_KITS" ]; then
  maybe_mkdir_p "$(dirname "$NEW_KITS")"
  maybe_mv "$OLD_KITS" "$NEW_KITS"
  log "moved $OLD_KITS -> $NEW_KITS"
elif [ -e "$OLD_KITS" ] && [ -e "$NEW_KITS" ]; then
  log "both $OLD_KITS and $NEW_KITS exist — refusing to merge automatically"
elif [ -d "$NEW_KITS" ]; then
  log "already migrated user kits at $NEW_KITS"
fi
rename_manifest_files "$ACTIVE_KITS"

log "step 7: refresh adapters"
if [ -d "$ACTIVE_ROOT/packages" ]; then
  while IFS= read -r -d '' adapter; do
    pkg="$(basename "$(dirname "$(dirname "$adapter")")")"
    src="$CARGO_BIN_DIR/$pkg"
    if [ ! -x "$src" ]; then
      log "missing source adapter $src — skipping $adapter"
      continue
    fi
    before_inode="$(ls -id "$adapter" 2>/dev/null | awk '{print $1}' || true)"
    maybe_rm "$adapter"
    maybe_cp "$src" "$adapter"
    maybe chmod +x "$adapter"
    after_inode="$(ls -id "$adapter" 2>/dev/null | awk '{print $1}' || true)"
    log "refreshed $adapter (inode ${before_inode:-missing} -> ${after_inode:-missing})"
    if [ "$DRY_RUN" -ne 1 ] && [ -n "$before_inode" ] && [ "$before_inode" != "missing" ] && [ "$before_inode" = "$after_inode" ]; then
      log "adapter inode did not change for $adapter"
      exit 1
    fi
  done < <(find "$ACTIVE_ROOT/packages" -path '*/harness-*/bin/adapter' -type f -print0 2>/dev/null || true)
fi

log "step 8: rewrite stale generated configs"
rewrite_stale_root_refs "$ACTIVE_ROOT"
rewrite_stale_root_refs "$ACTIVE_KITS"

log "step 9: create the transition alias"
maybe_mkdir_p "$CARGO_BIN_DIR"
if [ -L "$ELANUS_BIN" ]; then
  target="$(readlink "$ELANUS_BIN" || true)"
  if [ "$target" = "lanius" ] || [ "$target" = "$LANIUS_BIN" ]; then
    log "already aliased $ELANUS_BIN -> $target"
  else
    log "alias exists but points to $target; leaving it in place"
  fi
elif [ -e "$ELANUS_BIN" ]; then
  log "$ELANUS_BIN already exists as a real file; leaving it alone"
else
  maybe_ln lanius "$ELANUS_BIN"
  log "created alias $ELANUS_BIN -> lanius"
fi

if [ "$stopped_any" -eq 1 ]; then
  log "step 10: auto-restart intentionally skipped"
  log "restart manually with: lanius -C \"$NEW_ROOT\" serve"
else
  log "step 10: nothing was stopped, so nothing to restart"
fi

log "step 11: verification checklist"
log "  - lanius --version"
log "  - $NEW_ROOT exists"
log "  - $NEW_ROOT/lanius.db exists after first open"
log "  - adapters under $NEW_ROOT/packages/harness-*/bin/adapter are executable"
log "  - daemon/web are up only if you intentionally restarted them"
log "  - backup copies live beside the original db files with .bak-$TS"
log "  - the elanus alias is deprecated and will be removed next cycle"

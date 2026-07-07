---
status: planned
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# Small fix: refresh stale stock-harness adapters on seed

## The bug

When lanius seeds its stock harness packages, it copies the adapter binary
(`harness-claude`, `harness-codex`, `harness-opencode` from the running lanius's
`exe_dir`, e.g. `~/.cargo/bin/`) into each package's `bin/adapter` — but **only when
`bin/adapter` is missing.** So once an adapter is installed, a rebuilt/re-installed
lanius never refreshes it: you `cargo install` a newer lanius, the fresh
`~/.cargo/bin/harness-claude` is on disk, but the already-seeded `bin/adapter` keeps
running the OLD code forever.

Evidence — `seed_stock_harness_packages`, `src/initcmd.rs:735-746`:

```rust
let adapter = bin_dir.join("adapter");
let source = exe_dir.join(format!("{}{}", pkg.binary, std::env::consts::EXE_SUFFIX));
if source.is_file() {
    if !adapter.exists() {            // <-- only copies when MISSING
        std::fs::copy(&source, &adapter)...?;
    }
    set_executable(&adapter)?;
} else if !adapter.exists() {
    eprintln!("[init] warning: missing stock harness binary ...");
}
```

The `if !adapter.exists()` guard is the bug: an existing adapter is never re-copied.

## The fix (Tim's option (b): refresh when the source is newer)

Re-copy the adapter when the SOURCE binary (`exe_dir/harness-<tool>`,
`src/initcmd.rs:736`) has a **newer mtime** than the installed `bin/adapter`. An
adapter that is missing OR older-than-source is stale and gets refreshed; an
up-to-date one (source mtime ≤ adapter mtime) is left alone — no needless re-copy.

### CRITICAL macOS caveat — bake this into the copy

On macOS you **cannot overwrite a running/signed Mach-O in place** — copying new
bytes over a mapped, code-signed executable invalidates its signature and the next
launch gets **SIGKILL**'d. The refresh MUST `rm -f` the old adapter first, then `cp`
to a **fresh inode**. This is the exact rule `scripts/upgrade-to-lanius.sh` step 7
already follows (`src/../scripts/upgrade-to-lanius.sh:289-308`): it does
`maybe_rm "$adapter"` then `maybe_cp "$src" "$adapter"`, then asserts the inode
actually changed (`before_inode` vs `after_inode`, lines 298-307). Mirror that in the
Rust seeder: `std::fs::remove_file(&adapter)` (ignore not-found), THEN
`std::fs::copy(&source, &adapter)` — never copy-over-in-place.

### Touch-points

- `src/initcmd.rs:735-746` — the `if source.is_file()` / `if !adapter.exists()` block
  in `seed_stock_harness_packages`. Replace the `!adapter.exists()` guard with a
  staleness check:
  - stale = `!adapter.exists()` OR `mtime(source) > mtime(adapter)`
    (via `std::fs::metadata(&source)?.modified()` and the same for `adapter`; on any
    metadata error, treat as stale and refresh — fail toward a correct binary).
  - when stale: `let _ = std::fs::remove_file(&adapter);` then
    `std::fs::copy(&source, &adapter)?` (fresh inode), then `set_executable(&adapter)`
    (already at `src/initcmd.rs:743`, and `set_executable` is defined at
    `src/initcmd.rs:755`).
- Leave the `else if !adapter.exists()` warning branch (missing source) as-is.

This is confined to `seed_stock_harness_packages`; no other file changes.

## Milestone (single)

### Refresh stale adapters, fresh-inode, macOS-safe

**Acceptance:** after `cargo install` produces a newer `harness-claude` (mtime later
than the installed `bin/adapter`), the next `lanius init` / seed
(`seed_stock_harness_packages`) re-copies it to a **NEW inode** for that package's
`bin/adapter` (verify the inode changed, as the upgrade script asserts); and a seed
where the source mtime is ≤ the adapter's leaves the adapter **untouched** (same
inode, no re-copy). The refreshed adapter stays executable (`0o755`).

**Regression test note:** unit-test `seed_stock_harness_packages` (or a small helper
extracted from it) against a temp root: (1) seed a fake `exe_dir/harness-claude`,
record `bin/adapter`'s inode; (2) re-seed with a source whose mtime is bumped newer
→ assert inode changed and bytes match the new source; (3) re-seed with an unchanged
(older-or-equal mtime) source → assert inode is stable and no re-copy happened. Use
`std::fs`/`filetime` to control the mtimes deterministically.

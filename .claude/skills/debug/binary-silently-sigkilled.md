# Binary dies instantly with no output (silent SIGKILL at exec)

## Symptom

`lanius serve` (or any lanius invocation — even `lanius --version`) exits
immediately, prints **nothing**, and can't be restarted. Timed, the process
"runs" for 0.00 seconds; the exit status is 137 (128+9 = SIGKILL) or a shell
reports "killed". Nothing appears in any log — the process never lived long
enough to open one. Confusingly, an instance started *before* the breakage may
have kept running fine for hours (it was already in memory).

Same signature applies to headless coding workers: if `lanius code codex
--headless` (or similar) reports workers being SIGKILLed with no output, check
this before blaming the environment.

## What it means

The installed binary's macOS code signature no longer validates, and the
kernel refuses to execute it — SIGKILL before the first instruction. The usual
cause: someone **copied a new build over the installed binary in place**
(`cp target/release/lanius ~/.cargo/bin/lanius`). On Apple Silicon, every
executable carries an ad-hoc signature stamped by the linker at build time;
`cp` rewrites the *contents* of the existing file (same inode), and macOS's
per-inode signature-validation cache now sees tampered content → kill.

This actually happened 2026-07-08/09: a `cp`-installed binary silently killed
`lanius serve` restarts for hours and masqueraded as a mystery outage.

## Diagnose

```sh
# 1. Does it die in zero time with no output?
/usr/bin/time <binary> --version      # "terminated abnormally", 0.00 real

# 2. Kernel-level confirmation (look for a crash/AMFI note):
log show --last 5m --predicate 'eventMessage CONTAINS "taskgated" OR eventMessage CONTAINS "CODE SIGNING"' 2>/dev/null | tail -20
```

`codesign -vv <binary>` may STILL SAY "valid on disk" — the on-disk blob can
verify while the kernel's cached vnode state is poisoned. Don't let a clean
codesign output talk you out of this diagnosis; the 0.00s silent SIGKILL is
the authoritative tell.

## Fix

```sh
codesign -f -s - ~/.cargo/bin/lanius   # force a fresh ad-hoc signature
```

Verified working immediately — no rebuild needed. Then restart the stack
(`lanius serve`).

## Prevent

- **Never `cp` a build over an installed macOS binary.** Install with
  `cargo install --path .` — it replaces the destination with a NEW file (new
  inode), so the freshly linker-signed binary validates cleanly. (`mv` is also
  safe; in-place `cp` is the only trap.)
- cargo itself doesn't sign anything; Apple's linker ad-hoc signs every binary
  at link time. Any *new-file* install path keeps that signature valid.
- Bonus: since the web-embed-freshness handoff, `cargo install --path .` also
  rebuilds `ui/web/dist` automatically (build.rs), so it's the one-step correct
  install on every axis.

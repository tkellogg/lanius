//! kb-groundskeeper cadence throttle (docs/handoffs/kb-groundskeeper.md):
//! the hourly pipeline cron kick must honor the operator's configured `cadence`
//! — a daily cadence gets ONE pass per day, not twenty-four. The dispatch script
//! keeps a last-pass timestamp under `<root>/run/` and interval-compares it.
//!
//! Drives `kits/core/packages/kb-groundskeeper/scripts/dispatch` directly with a
//! scratch root and a stub `lanius` (the LANIUS_BIN seam), so the throttle is
//! tested at the level it lives: the script, not the kernel verb.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn dispatch_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("kits/core/packages/kb-groundskeeper/scripts/dispatch")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("el-gk-cadence-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stub `elanus`: logs every invocation to $STUB_LOG; answers `config get
/// kb-groundskeeper cadence` with a DAILY cadence (raw TOML, quoted — what the
/// real verb prints) and `kb groundskeep` with the contents of $STUB_REPLY.
fn write_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("elanus-stub");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
echo "$@" >> "$STUB_LOG"
case "$*" in
  *"config get kb-groundskeeper cadence"*) printf '%s\n' '"0 3 * * *"' ;;
  *"kb groundskeep"*) cat "$STUB_REPLY" ;;
esac
exit 0
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    stub
}

/// Deliver one pipeline cron kick to the dispatch script. Returns (exit ok, stderr).
fn kick(root: &Path, stub: &Path, log: &Path, reply: &Path) -> (bool, String) {
    let mut child = Command::new("python3")
        .arg(dispatch_script())
        .env("LANIUS_ROOT", root)
        .env("LANIUS_BIN", stub)
        .env("STUB_LOG", log)
        .env("STUB_REPLY", reply)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 runs the dispatch script");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"type":"in/package/kb-groundskeeper/pipeline"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// How many times the stub saw `kb groundskeep`.
fn groundskeep_calls(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("kb groundskeep"))
        .count()
}

fn last_pass_file(root: &Path) -> PathBuf {
    root.join("run").join("kb-groundskeeper.last-pass")
}

#[test]
fn daily_cadence_gets_one_pass_per_interval() {
    let root = scratch("daily");
    let stub = write_stub(&root);
    let log = root.join("stub.log");
    let reply = root.join("reply");
    // The verb dispatched a real pass (a spawn descriptor, not "inert: ...").
    std::fs::write(&reply, "{\"mailbox\":\"in/agent/kb-compactor\"}\n").unwrap();

    // Kick 1: no last-pass recorded yet → the pass runs and is stamped.
    let (ok, err) = kick(&root, &stub, &log, &reply);
    assert!(ok, "first kick succeeds: {err}");
    assert_eq!(groundskeep_calls(&log), 1, "first kick reaches the verb");
    assert!(
        last_pass_file(&root).exists(),
        "a real pass records its timestamp"
    );

    // Kick 2, immediately (the next hourly cron fire): the daily cadence
    // ('0 3 * * *') has not elapsed → skipped, the verb is NOT called again.
    let (ok, err) = kick(&root, &stub, &log, &reply);
    assert!(ok, "a skipped kick still exits clean: {err}");
    assert_eq!(
        groundskeep_calls(&log),
        1,
        "an hourly kick inside the daily cadence must not run a second pass"
    );
    assert!(err.contains("skipped"), "the skip says why: {err}");

    // Backdate the last pass beyond a day → the next kick runs again.
    let yesterday = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        - 86_500.0;
    std::fs::write(last_pass_file(&root), yesterday.to_string()).unwrap();
    let (ok, err) = kick(&root, &stub, &log, &reply);
    assert!(ok, "a due kick succeeds: {err}");
    assert_eq!(
        groundskeep_calls(&log),
        2,
        "once the cadence elapses the pass runs"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn inert_result_does_not_consume_the_cadence() {
    // Before setup completes the verb prints "inert: ..."; that must NOT stamp
    // the last-pass timestamp, so the first kick after setup runs immediately
    // instead of waiting out a cadence a pass never used.
    let root = scratch("inert");
    let stub = write_stub(&root);
    let log = root.join("stub.log");
    let reply = root.join("reply");
    std::fs::write(&reply, "inert: kb-groundskeeper is not set up\n").unwrap();

    let (ok, _) = kick(&root, &stub, &log, &reply);
    assert!(ok);
    let (ok, _) = kick(&root, &stub, &log, &reply);
    assert!(ok);
    assert_eq!(
        groundskeep_calls(&log),
        2,
        "inert kicks keep reaching the verb (the gate, not the throttle, holds them)"
    );
    assert!(
        !last_pass_file(&root).exists(),
        "an inert result records no pass"
    );

    let _ = std::fs::remove_dir_all(&root);
}

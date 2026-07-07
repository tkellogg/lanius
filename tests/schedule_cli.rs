use anyhow::{Context, Result};
use lanius::paths::Root;
use std::path::PathBuf;
use std::process::Command;

// M3 (docs/handoffs/timers.md): the `lanius schedule` CLI surface. A trusted
// operator gesture inserts a one-shot `scheduled_events` row targeting any named
// agent's mailbox (unlike the self-only tool). This drives the real binary and
// reads the ledger it wrote — the firing/idempotency half is a dispatcher unit
// test (tick_schedules is crate-internal).
#[test]
fn schedule_cli_inserts_a_one_shot_row() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new("cargo")
        .args(["build", "--bin", "lanius"])
        .current_dir(&repo)
        .output()
        .context("cargo build lanius")?;
    assert!(
        build.status.success(),
        "cargo build failed\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let target_debug = target_debug_dir()?;
    let elanus_bin = target_debug.join(format!("lanius{}", std::env::consts::EXE_SUFFIX));

    let root_dir = unique_temp_dir("lanius-sched-root")?;

    let out = Command::new(&elanus_bin)
        .args([
            "schedule",
            "--agent",
            "main",
            "--in",
            "2",
            "--message",
            "ping",
        ])
        .env("LANIUS_ROOT", &root_dir)
        .output()
        .context("running lanius schedule")?;
    assert!(
        out.status.success(),
        "lanius schedule failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Read the row the CLI wrote straight off the ledger.
    let root = Root {
        dir: root_dir.clone(),
    };
    let conn = lanius::db::open(&root)?;
    let (emit_type, payload, created_by, fired): (String, String, String, i64) = conn.query_row(
        "SELECT emit_type, payload, created_by, fired FROM scheduled_events",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(emit_type, lanius::topic::agent_mailbox("main"));
    assert_eq!(created_by, "cli");
    assert_eq!(fired, 0);
    let payload: serde_json::Value = serde_json::from_str(&payload)?;
    assert_eq!(payload["prompt"], "ping");

    let _ = std::fs::remove_dir_all(&root_dir);
    Ok(())
}

fn target_debug_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir).join("debug"));
    }
    let mut dir = std::env::current_exe().context("resolving current test executable")?;
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    Ok(dir)
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

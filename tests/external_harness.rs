use anyhow::{Context, Result};
use rusqlite::OptionalExtension as _;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn code_echo_dispatches_to_external_sdk_adapter() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new("cargo")
        .args(["build", "--bin", "elanus", "--example", "harness_echo"])
        .current_dir(&repo)
        .output()
        .context("running cargo build for elanus and harness_echo")?;
    assert!(
        build.status.success(),
        "cargo build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let target_debug = target_debug_dir()?;
    let elanus_bin = target_debug.join(format!("elanus{}", std::env::consts::EXE_SUFFIX));
    let example_bin = target_debug
        .join("examples")
        .join(format!("harness_echo{}", std::env::consts::EXE_SUFFIX));

    let root_dir = unique_temp_dir("elanus-ext-root")?;
    let workdir = unique_temp_dir("elanus-ext-work")?;
    let pkg = root_dir.join("packages").join("harness-echo");
    let bin_dir = pkg.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    std::fs::write(
        pkg.join("elanus.toml"),
        r#"
[[harness]]
name = "echo"
aliases = ["ec"]
run = "bin/adapter"
"#,
    )?;
    let adapter = bin_dir.join("adapter");
    std::fs::copy(&example_bin, &adapter).with_context(|| {
        format!(
            "copying example adapter {} -> {}",
            example_bin.display(),
            adapter.display()
        )
    })?;
    make_executable(&adapter)?;

    let output = Command::new(&elanus_bin)
        .args(["code", "echo", "--headless", "hello world"])
        .env("ELANUS_ROOT", &root_dir)
        .current_dir(&workdir)
        .output()
        .with_context(|| format!("running {}", elanus_bin.display()))?;
    assert!(
        output.status.success(),
        "elanus code echo failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let root = elanus::paths::Root {
        dir: root_dir.clone(),
    };
    let conn = elanus::db::open(&root)?;
    elanus::db::init_schema(&conn)?;
    let claim: Option<(String, String)> = conn
        .query_row(
            "SELECT c.session, c.path
               FROM code_claims c
               JOIN code_sessions s ON s.elanus_session = c.session
              WHERE s.tool = 'echo'
                AND s.agent_noun = 'echo'
                AND c.path LIKE ?1
              LIMIT 1",
            [format!("%{}ECHO_ADAPTER_RAN", std::path::MAIN_SEPARATOR)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let (session, path) = claim.context("external harness did not record the SDK claim proof")?;
    assert!(
        session.starts_with("code-"),
        "claim should belong to an elanus code session, got {session}"
    );
    assert!(
        path.ends_with("ECHO_ADAPTER_RAN"),
        "claim path should prove the adapter ran, got {path}"
    );

    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&workdir);
    Ok(())
}

/// Build elanus + the echo adapter, install a `harness-echo` package into a fresh
/// root, and return `(elanus_bin, root_dir, workdir)`. Shared by the detached-spawn
/// e2e tests below (cross-harness-death M2 — the stock echo proxy stands in for the
/// real claude/codex/opencode adapters, which need credentials this sandbox lacks;
/// the detached spawn → edge → structured completion → settle path is
/// harness-uniform, so the proxy exercises the exact code under test).
fn build_echo_root() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new("cargo")
        .args(["build", "--bin", "elanus", "--example", "harness_echo"])
        .current_dir(&repo)
        .output()
        .context("running cargo build for elanus and harness_echo")?;
    assert!(
        build.status.success(),
        "cargo build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let target_debug = target_debug_dir()?;
    let elanus_bin = target_debug.join(format!("elanus{}", std::env::consts::EXE_SUFFIX));
    let example_bin = target_debug
        .join("examples")
        .join(format!("harness_echo{}", std::env::consts::EXE_SUFFIX));

    let root_dir = unique_temp_dir("elanus-spawn-root")?;
    let workdir = unique_temp_dir("elanus-spawn-work")?;
    let pkg = root_dir.join("packages").join("harness-echo");
    let bin_dir = pkg.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    std::fs::write(
        pkg.join("elanus.toml"),
        "\n[[harness]]\nname = \"echo\"\naliases = [\"ec\"]\nrun = \"bin/adapter\"\n",
    )?;
    let adapter = bin_dir.join("adapter");
    std::fs::copy(&example_bin, &adapter)?;
    make_executable(&adapter)?;
    Ok((elanus_bin, root_dir, workdir))
}

/// cross-harness-death M1/M2 e2e — a DETACHED `elanus code spawn` worker records a
/// durable spawn edge, and on a clean exit its own completion mail carries the
/// structured `failed:false` contract and settles the edge. Real end-to-end through
/// the detached wrapper (the echo adapter stands in for a real harness).
#[test]
fn spawn_worker_delivers_structured_completion_and_settles_edge() -> Result<()> {
    let (elanus_bin, root_dir, workdir) = build_echo_root()?;
    let root = elanus::paths::Root {
        dir: root_dir.clone(),
    };
    // The spawner must be a recorded coding session so its completion mailbox
    // resolves (mailbox_for_actor reads the durable record for its noun).
    elanus::codesession::upsert_record(
        &root,
        &elanus::codesession::SessionRecord {
            elanus_session: "code-spawner1".into(),
            native_session: "n".into(),
            tool: "claude".into(),
            agent_noun: "claude-code".into(),
            workdir: workdir.display().to_string(),
            room: None,
        },
    )?;

    let out = Command::new(&elanus_bin)
        .args(["code", "spawn", "echo", "hello from spawn"])
        .env("ELANUS_ROOT", &root_dir)
        .env("ELANUS_CODE_SESSION", "code-spawner1")
        .current_dir(&workdir)
        .output()
        .context("running elanus code spawn echo")?;
    assert!(
        out.status.success(),
        "elanus code spawn failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The detached worker runs asynchronously; poll for its completion mail.
    let conn = elanus::db::open(&root)?;
    elanus::db::init_schema(&conn)?;
    let payload = poll_completion(&conn, "in/agent/claude-code/code-spawner1")
        .context("detached worker never delivered its completion")?;
    let pv: serde_json::Value = serde_json::from_str(&payload)?;
    assert_eq!(pv["failed"], serde_json::json!(false), "clean exit → failed:false");
    assert_eq!(pv["exit_code"], serde_json::json!(0));
    assert!(pv["worker"].as_str().unwrap().starts_with("code-"));
    assert!(pv["prompt"].as_str().unwrap().contains("finished"));

    // The spawn edge exists and is settled by the worker's own completion.
    let settled: Option<String> = conn
        .query_row(
            "SELECT settled_at FROM code_spawn_edges WHERE spawner='code-spawner1'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    assert!(settled.is_some(), "the spawn edge is settled after delivery");

    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&workdir);
    Ok(())
}

/// cross-harness-death M1 e2e — a detached worker that exits NONZERO delivers the
/// structured failure contract (`failed:true` + the exit code) to its spawner,
/// through the real detached wrapper.
#[test]
fn spawn_worker_that_exits_nonzero_mails_structured_failure() -> Result<()> {
    let (elanus_bin, root_dir, workdir) = build_echo_root()?;
    let root = elanus::paths::Root {
        dir: root_dir.clone(),
    };
    elanus::codesession::upsert_record(
        &root,
        &elanus::codesession::SessionRecord {
            elanus_session: "code-spawner2".into(),
            native_session: "n".into(),
            tool: "claude".into(),
            agent_noun: "claude-code".into(),
            workdir: workdir.display().to_string(),
            room: None,
        },
    )?;

    let out = Command::new(&elanus_bin)
        .args(["code", "spawn", "echo", "this will fail"])
        .env("ELANUS_ROOT", &root_dir)
        .env("ELANUS_CODE_SESSION", "code-spawner2")
        // The echo adapter honors this and exits nonzero (test hook).
        .env("ELANUS_HARNESS_ECHO_EXIT", "5")
        .current_dir(&workdir)
        .output()
        .context("running elanus code spawn echo (failure)")?;
    assert!(out.status.success(), "the SPAWN command itself still returns 0");

    let conn = elanus::db::open(&root)?;
    elanus::db::init_schema(&conn)?;
    let payload = poll_completion(&conn, "in/agent/claude-code/code-spawner2")
        .context("failing worker never delivered its completion")?;
    let pv: serde_json::Value = serde_json::from_str(&payload)?;
    assert_eq!(pv["failed"], serde_json::json!(true), "nonzero exit → failed:true");
    assert_eq!(pv["exit_code"], serde_json::json!(5), "the exit code is carried");

    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&workdir);
    Ok(())
}

/// Poll a mailbox topic for a delivered completion event's payload, up to ~15s.
fn poll_completion(conn: &rusqlite::Connection, topic: &str) -> Option<String> {
    for _ in 0..150 {
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM events WHERE type = ?1 ORDER BY id DESC LIMIT 1",
                [topic],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if payload.is_some() {
            return payload;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
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

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

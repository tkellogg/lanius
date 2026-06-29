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

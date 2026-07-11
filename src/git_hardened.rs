//! Shared hardened-git discipline (docs/security.md entries 18, 19). Both the
//! config repo (`config_repo.rs`) and the KB write path (`kb.rs`) shell out to the
//! system `git` to commit AGENT-AUTHORED content into a kernel-owned tree, so both
//! need the same untrusted-input hardening: hooks off, no signing, no fsmonitor,
//! no operator global/system gitconfig (which can carry a `[filter] smudge=…`,
//! alias, or pager that would execute under our flags — a real, reproduced leak),
//! no system attributes file, no ambient `GIT_DIR`/`GIT_WORK_TREE` hijack, never
//! prompt. Extracted here (kb-core.md wonky bit 3) so the discipline lives in ONE
//! place and cannot drift between the two callers.
//!
//! The commit author is a fixed, non-human kernel identity: the trustworthy
//! "who did this" is the ledger/obs trail and the git log's own record, never a
//! forged commit author.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// The committer identity stamped on every kernel-owned commit (config + kb).
pub const COMMITTER_NAME: &str = "lanius";
pub const COMMITTER_EMAIL: &str = "lanius@localhost";

/// Apply the untrusted-input hardening every kernel git invocation shares.
pub fn harden(c: &mut Command) {
    let null = crate::platform::NULL_DEVICE; // "/dev/null" on Unix, "NUL" on Windows
    c.arg("-c")
        .arg(format!("core.hooksPath={null}"))
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg(format!("user.name={COMMITTER_NAME}"))
        .arg("-c")
        .arg(format!("user.email={COMMITTER_EMAIL}"))
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_SYSTEM", null)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0");
}

/// A hardened `git` invocation with no working directory bound (e.g. `clone`,
/// which takes src + dest as arguments).
pub fn git_bare() -> Command {
    let mut c = Command::new("git");
    harden(&mut c);
    c
}

/// A hardened `git` invocation rooted (`-C`) at `dir`.
pub fn git_in(dir: &Path) -> Command {
    let mut c = git_bare();
    c.arg("-C").arg(dir);
    c
}

/// Run a hardened git command, capturing stdout; a nonzero exit is an error.
pub fn run_git(mut c: Command, what: &str) -> Result<String> {
    let out = c
        .output()
        .with_context(|| format!("running git {what} (is git installed?)"))?;
    if !out.status.success() {
        bail!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a hardened git command in `dir` for its exit status only — no output, no
/// bail. For probes where a non-zero exit is a normal answer ("no such branch").
pub fn ok_in(dir: &Path, args: &[&str]) -> bool {
    git_in(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a hardened git command in `dir`, capturing stdout.
pub fn run_in(dir: &Path, args: &[&str], what: &str) -> Result<String> {
    let mut c = git_in(dir);
    c.args(args);
    run_git(c, what)
}

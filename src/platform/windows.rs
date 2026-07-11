//! Windows implementation of the platform shims. **M1 = compile + honestly
//! fence** (docs/handoffs/windows-support.md). Process groups have no direct
//! Windows equivalent; the load-bearing Job-Object port (`CreateJobObject` +
//! `AssignProcessToJobObject` + `TerminateJobObject`) is M2. Here the child is
//! spawned normally and `kill_group` reaches only the direct child — a
//! documented degrade (a grandchild such as a shell descendant may briefly
//! outlive a timeout/reload until M2), consistent with how the existing
//! `#[cfg(not(unix))]` branches already behaved. `is_alive` IS real so lease/
//! token reaping stays correct.

use std::path::Path;
use std::process::Command;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};

/// The null device path used when hardening `git` invocations.
pub const NULL_DEVICE: &str = "NUL";

/// No-op on Windows for M1: process groups map to Job Objects, wired in M2.
/// The child is spawned in the caller's default group.
pub fn set_process_group(_cmd: &mut Command) {}

/// M1 degrade: `TerminateProcess` the direct child only (no Job Object tree
/// yet — M2). Equivalent-intent to SIGKILL for the immediate process.
pub fn kill_group(pid: i32) {
    terminate_pid(pid, 1);
}

/// M1 degrade: same as `kill_group` — Windows has no graceful group SIGTERM
/// without either a Job Object or a console-ctrl broadcast (M2).
pub fn terminate_group(pid: i32) {
    terminate_pid(pid, 1);
}

fn terminate_pid(pid: i32, exit_code: u32) {
    if pid <= 0 {
        return;
    }
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if h.is_null() {
            return;
        }
        TerminateProcess(h, exit_code);
        CloseHandle(h);
    }
}

/// Is `pid` alive? Open the process and test whether its wait handle is still
/// unsignaled (`WAIT_TIMEOUT` => running). A process we cannot open because it
/// no longer exists (`ERROR_INVALID_PARAMETER`) is dead; a process we cannot
/// open for any other reason (e.g. access-denied but existing) is treated as
/// ALIVE — fail-safe toward keeping a live session, never wrongly reaping.
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if h.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let waited = WaitForSingleObject(h, 0);
        CloseHandle(h);
        waited == WAIT_TIMEOUT
    }
}

/// No-op on Windows: executability is not a file-mode bit. Run-ability of
/// package scripts is handled by the M3 "require a POSIX shell" strategy.
pub fn set_executable(_p: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Copy instead of symlink (Windows symlinks need Developer Mode or the
/// `SeCreateSymbolicLink` privilege). The skill / codex-home links this backs
/// are ephemeral — regenerated each session and removed at exit — so losing
/// live-reflect is acceptable (handoff decision #5). Directories are copied
/// recursively; files are copied directly.
pub fn link_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        copy_dir_all(src, dst)
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Whether `p` is writable. Windows has no `access(W_OK)`; approximate with the
/// read-only attribute for files (meaningful there) and assume directories are
/// writable (the read-only attribute on a directory is not a reliable ACL
/// check). Close enough to `fs.accessSync(W_OK)` for the UI's path-check probe.
pub fn is_writable(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => true,
        Ok(m) => !m.permissions().readonly(),
        Err(_) => false,
    }
}

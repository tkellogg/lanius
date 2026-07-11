//! Unix implementation of the platform shims (libc-backed). See
//! `platform/mod.rs` for the portable contract each function upholds.

use std::path::Path;
use std::process::Command;

/// The null device path used when hardening `git` invocations
/// (`core.hooksPath`, `GIT_CONFIG_GLOBAL`, ...).
pub const NULL_DEVICE: &str = "/dev/null";

/// Put a child in its own process group *before* spawn, so the parent can
/// signal the whole tree (the sandbox-exec wrapper and any shell descendants),
/// not just the direct child. Because the child becomes the group leader, its
/// pgid equals its pid — which is what `kill_group`/`terminate_group` rely on.
pub fn set_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
}

/// SIGKILL the whole process group whose pgid == `pid` (the child was spawned
/// as its own group leader via `set_process_group`).
pub fn kill_group(pid: i32) {
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

/// SIGTERM the whole process group (graceful stop). Negating the pid targets
/// the group; a fallback to the bare pid keeps a lone child reachable.
pub fn terminate_group(pid: i32) {
    unsafe {
        if libc::kill(-pid, libc::SIGTERM) == -1 {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// Is `pid` alive? Signal-0 is an existence probe with no effect. `ESRCH` means
/// no such process (dead); anything else (notably `EPERM` — alive but owned by
/// another uid) is treated as ALIVE, so a live cross-uid session is never
/// wrongly reaped (fail-safe toward keeping authority, not dropping it).
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Mark a file executable (`chmod +rwx` on the owner bits, `| 0o755`).
pub fn set_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(p, perms)
}

/// Reflect `src` into `dst` as a symlink, so edits to the source reflect live
/// and skill-relative script paths resolve back into the real package tree.
pub fn link_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Whether the current process can write `p` — the `access(W_OK)` probe (mirrors
/// Node's `fs.accessSync(path, W_OK)`).
pub fn is_writable(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

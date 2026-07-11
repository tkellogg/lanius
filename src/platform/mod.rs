//! Cross-platform shims for the handful of OS process-lifecycle primitives the
//! kernel needs: spawn-a-child-in-its-own-group, kill/terminate that group,
//! is-this-pid-alive, mark-executable, symlink-or-copy, is-writable, and the
//! null-device path. One module owns "how we kill a tree" (kernel discipline)
//! so the Windows impl is reviewable in isolation instead of sprinkled across a
//! dozen inline cfg-gates. See docs/handoffs/windows-support.md.
//!
//! Unix uses libc (real POSIX process groups + signals). Windows is honestly
//! degraded for **M1** (compile + fence): `set_process_group` is a no-op and
//! `kill_group` reaches only the direct child — the load-bearing Job-Object
//! implementation (`CreateJobObject` + `TerminateJobObject`) is M2. `is_alive`
//! is real on both platforms (M1 needs it correct so lease/token reaping does
//! not wrongly drop authority).

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

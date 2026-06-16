//! Environment-variable naming compatibility. The product's canonical env vars
//! are `ELANUS_*`; the old `HARNESS_*` names (from when the binary was `harness`)
//! are kept working as a fallback so existing shells and any custom package
//! scripts don't break. Two helpers keep that promise in one place:
//!
//! - `read(suffix)` — what the kernel READS: prefer `ELANUS_<suffix>`, fall back
//!   to `HARNESS_<suffix>`.
//! - `Command::env_dual(suffix, val)` — what the kernel SETS on a child: write
//!   the canonical `ELANUS_<suffix>` AND a legacy `HARNESS_<suffix>` alias, so a
//!   script reading either name finds it.

use std::ffi::OsStr;
use std::process::Command;

/// Read a kernel env var by suffix, preferring canonical `ELANUS_<suffix>` and
/// falling back to legacy `HARNESS_<suffix>`.
pub fn read(suffix: &str) -> Option<String> {
    std::env::var(format!("ELANUS_{suffix}"))
        .ok()
        .or_else(|| std::env::var(format!("HARNESS_{suffix}")).ok())
}

/// Set both the canonical `ELANUS_<suffix>` and the legacy `HARNESS_<suffix>`
/// alias on a child command.
pub trait EnvDual {
    fn env_dual(&mut self, suffix: &str, val: impl AsRef<OsStr>) -> &mut Self;
}

impl EnvDual for Command {
    fn env_dual(&mut self, suffix: &str, val: impl AsRef<OsStr>) -> &mut Self {
        let v = val.as_ref();
        self.env(format!("ELANUS_{suffix}"), v);
        self.env(format!("HARNESS_{suffix}"), v)
    }
}

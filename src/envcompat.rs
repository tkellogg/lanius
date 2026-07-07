//! Environment-variable naming compatibility. The product's canonical env vars
//! are `LANIUS_*`; reads keep honoring `ELANUS_*` and `HARNESS_*` so existing
//! shells and package scripts keep working. One helper keeps that promise in one
//! place:
//!
//! - `read(suffix)` — what the kernel READS: prefer `LANIUS_<suffix>`, then
//!   `ELANUS_<suffix>`, then `HARNESS_<suffix>`.
//! - `Command::env_dual(suffix, val)` — what the kernel SETS on a child: write
//!   the canonical `LANIUS_<suffix>` and the legacy `ELANUS_<suffix>` alias, so
//!   a pre-rename script still sees the old spelling.

use std::ffi::OsStr;
use std::process::Command;

/// Read a kernel env var by suffix, preferring canonical `LANIUS_<suffix>`,
/// then legacy `ELANUS_<suffix>`, then the older `HARNESS_<suffix>`.
pub fn read(suffix: &str) -> Option<String> {
    std::env::var(format!("LANIUS_{suffix}"))
        .ok()
        .or_else(|| std::env::var(format!("ELANUS_{suffix}")).ok())
        .or_else(|| std::env::var(format!("HARNESS_{suffix}")).ok())
}

/// Set both the canonical `LANIUS_<suffix>` and the legacy `ELANUS_<suffix>`
/// alias on a child command.
pub trait EnvDual {
    fn env_dual(&mut self, suffix: &str, val: impl AsRef<OsStr>) -> &mut Self;
}

impl EnvDual for Command {
    fn env_dual(&mut self, suffix: &str, val: impl AsRef<OsStr>) -> &mut Self {
        let v = val.as_ref();
        self.env(format!("LANIUS_{suffix}"), v);
        self.env(format!("ELANUS_{suffix}"), v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn read_prefers_lanius_then_elanus_then_harness() {
        let _guard = env_lock().lock().unwrap();
        for key in ["LANIUS_FOO", "ELANUS_FOO", "HARNESS_FOO"] {
            std::env::remove_var(key);
        }
        std::env::set_var("HARNESS_FOO", "harness");
        assert_eq!(read("FOO").as_deref(), Some("harness"));
        std::env::set_var("ELANUS_FOO", "elanus");
        assert_eq!(read("FOO").as_deref(), Some("elanus"));
        std::env::set_var("LANIUS_FOO", "lanius");
        assert_eq!(read("FOO").as_deref(), Some("lanius"));
        for key in ["LANIUS_FOO", "ELANUS_FOO", "HARNESS_FOO"] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn env_dual_sets_canonical_and_legacy() {
        let _guard = env_lock().lock().unwrap();
        let output = Command::new("sh")
            .arg("-c")
            .arg("printf '%s|%s|%s' \"$LANIUS_BAR\" \"$ELANUS_BAR\" \"$HARNESS_BAR\"")
            .env_dual("BAR", "value")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "value|value|");
    }
}

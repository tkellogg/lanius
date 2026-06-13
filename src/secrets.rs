//! Kernel-minted credentials for the identity model (docs/identity.md).
//!
//! These live in the fenced secret store (Root::secrets()), which the cage
//! denies actors from reading (src/sandbox.rs `Protect`). So the kernel and
//! the uncaged human-facing surfaces (the command line, the web server) can
//! read them; a caged agent cannot. That asymmetry is the whole point: a
//! credential an agent cannot read is a credential it cannot present.
//!
//! Two reserved identities sit alongside the package actors: "human" (the
//! person, via their surfaces) and "kernel" (the kernel's own machinery
//! running in a forked process — the mirror, the resident-hook consult).
//! Both carry full authority; package actors stay scoped to their grants.

use crate::paths::Root;
use std::io::Write;
use std::path::Path;

/// Reserved sender identities. Package names can never collide: a package
/// name is one topic level, and these are checked first in the handshake.
pub const HUMAN: &str = "human";
pub const KERNEL: &str = "kernel";

/// Ensure the human and kernel secrets exist, minting any that are missing.
/// Idempotent; called at daemon startup before the broker binds so a secret
/// always exists by the time anything can connect.
pub fn ensure(root: &Root) -> std::io::Result<()> {
    std::fs::create_dir_all(root.secrets())?;
    for name in [HUMAN, KERNEL] {
        let p = root.secrets().join(name);
        if !p.exists() {
            // 256 bits of url-safe randomness is plenty for a loopback token.
            let secret = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            write_0600(&p, &secret)?;
        }
    }
    Ok(())
}

/// Read a minted secret, trimmed. None if absent or unreadable — which is
/// exactly where a caged actor reading the fenced store lands, and the
/// caller treats "no secret" as "connect anonymously" (refused once the
/// deny-by-default flip is live).
pub fn read(root: &Root, name: &str) -> Option<String> {
    std::fs::read_to_string(root.secrets().join(name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_0600(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(contents.as_bytes())
}

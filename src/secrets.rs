//! Kernel-minted credentials for the identity model (docs/identity.md).
//!
//! These live in the fenced secret store (Root::secrets()), which the cage
//! denies actors from reading (src/sandbox.rs `Protect`). So the kernel and
//! the uncaged human-facing surfaces (the command line, the web server) can
//! read them; a caged agent cannot. That asymmetry is the whole point: a
//! credential an agent cannot read is a credential it cannot present.
//!
//! A fenced secret file *is* what names a full-authority principal. The two
//! shipped by default are the human owner (named "owner" — a person is an
//! identity, not the role "human"; docs/identity.md "a name, not a role") and
//! "kernel" (the kernel's own machinery in a forked process — the mirror, the
//! resident-hook consult). More human identities are model-ready: drop another
//! fenced secret and that name authenticates too. Package actors stay
//! grant-scoped via their per-spawn tokens; only a fenced secret confers full
//! authority, and only the human/kernel can place one (the cage fences the
//! store from agents).
//!
//! The owner's NAME has a single source of truth — the default profile's
//! `owner` field — and `.owner-name` is a cache of it that the surfaces read
//! (so the JS surfaces need not parse TOML). `ensure` keeps the cache in sync
//! and, when the owner is renamed, moves the existing secret to the new name
//! so the auth identity, the `in/human/<owner>` mailbox, and the credential
//! always agree and nothing is orphaned. `ELANUS_OWNER` is a runtime override.

use crate::paths::Root;
use std::io::Write;
use std::path::Path;

/// The default owner identity name. Only a default — the first person on a
/// fresh single-person install reads as "owner"; they rename themselves by
/// setting the default profile's `owner` (see the init nudge).
pub const OWNER: &str = "owner";
/// The kernel's own machinery.
pub const KERNEL: &str = "kernel";
/// The pre-rename principal name; a legacy `.secrets/human` is folded into the
/// owner identity on upgrade so no orphaned full-authority credential lingers.
const LEGACY_OWNER: &str = "human";
/// Cache of the owner's chosen name for the surfaces. Dot-prefixed so it can
/// never itself be a principal (`read`/`valid_principal` reject leading dots),
/// keeping this config file from doubling as an authentication secret.
const OWNER_NAME_FILE: &str = ".owner-name";

/// A principal name must be a single safe segment: no path separators (so a
/// username can never traverse out of the store), no leading dot (so the cache
/// file above is not a principal), non-empty, bounded. The JS surfaces
/// (ui/web/server.mjs, ui/tui/app.js) replicate this exactly; keep them in
/// sync — a divergence means a surface presents a principal the broker would
/// never resolve under that name.
pub fn valid_principal(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
}

/// The configured owner identity name a surface should authenticate as:
/// `ELANUS_OWNER` env override, else the persisted `.owner-name` cache, else
/// the default "owner". The cache is kept equal to the default profile's
/// `owner` by `ensure`, so this also matches the `in/human/<owner>` mailbox.
pub fn owner_name(root: &Root) -> String {
    if let Ok(o) = std::env::var("ELANUS_OWNER") {
        let o = o.trim().to_string();
        if valid_principal(&o) {
            return o;
        }
    }
    std::fs::read_to_string(root.secrets().join(OWNER_NAME_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| valid_principal(s))
        .unwrap_or_else(|| OWNER.to_string())
}

/// The single source of truth for the owner's name: the default profile's
/// `owner` (which itself defaults to "owner"), validated, else "owner".
fn seed_owner_name(root: &Root) -> String {
    crate::profile::load(root, "default")
        .ok()
        .map(|(p, _)| p.owner)
        .filter(|o| valid_principal(o))
        .unwrap_or_else(|| OWNER.to_string())
}

/// Ensure the owner and kernel secrets exist, minting any that are missing,
/// and keep the `.owner-name` cache equal to the configured owner. When the
/// owner is renamed (or an old `.secrets/human` is found on upgrade), move the
/// existing secret to the new name rather than minting a stranger and leaving
/// the old credential live — so the auth identity and the mailbox always agree
/// and no orphaned full-authority secret lingers. Idempotent; called at daemon
/// startup before the broker binds so a secret always exists by connect time.
pub fn ensure(root: &Root) -> std::io::Result<()> {
    std::fs::create_dir_all(root.secrets())?;
    let desired = seed_owner_name(root);
    let name_file = root.secrets().join(OWNER_NAME_FILE);
    // The previous owner: the cached name, or — on a pre-rename root with no
    // cache — the legacy "human" secret.
    let previous = std::fs::read_to_string(&name_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| valid_principal(s))
        .or_else(|| {
            root.secrets()
                .join(LEGACY_OWNER)
                .exists()
                .then(|| LEGACY_OWNER.to_string())
        });
    if previous.as_deref() != Some(desired.as_str()) {
        if let Some(prev) = &previous {
            if prev != KERNEL && prev != &desired {
                let from = root.secrets().join(prev);
                let to = root.secrets().join(&desired);
                if from.exists() {
                    if to.exists() {
                        let _ = std::fs::remove_file(&from); // target present; prev is a redundant orphan
                    } else {
                        let _ = std::fs::rename(&from, &to); // reuse the secret under the new name
                    }
                }
            }
        }
        write_0600(&name_file, &desired)?;
    }
    let owner = owner_name(root);
    for name in [owner.as_str(), KERNEL] {
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

/// Read a minted secret, trimmed. None if the name is not a valid principal
/// (rejected before any file access, so a crafted username cannot traverse the
/// store or read the cache file), or if absent/unreadable — which is exactly
/// where a caged actor reading the fenced store lands, and the caller treats
/// "no secret" as "not this identity".
pub fn read(root: &Root, name: &str) -> Option<String> {
    if !valid_principal(name) {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_root() -> Root {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "elanus-sectest-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    #[test]
    fn principal_validation() {
        for ok in ["owner", "kernel", "alice", "recent-history", "a.b"] {
            assert!(valid_principal(ok), "{ok} should be valid");
        }
        for bad in ["", "..", ".owner-name", "a/b", "a\\b"] {
            assert!(!valid_principal(bad), "{bad} should be invalid");
        }
        assert!(!valid_principal(&"x".repeat(65)));
    }

    #[test]
    fn read_rejects_unsafe_names() {
        let root = tmp_root();
        std::fs::create_dir_all(root.secrets()).unwrap();
        std::fs::write(root.secrets().join(OWNER_NAME_FILE), "owner").unwrap();
        // the cache file exists, but a path-unsafe / dot-prefixed name never reads it
        assert_eq!(read(&root, OWNER_NAME_FILE), None);
        assert_eq!(read(&root, "../elanus.db"), None);
        assert_eq!(read(&root, ".."), None);
    }

    #[test]
    fn migration_folds_legacy_human_into_owner_without_orphan() {
        let root = tmp_root();
        std::fs::create_dir_all(root.secrets()).unwrap();
        // a pre-rename root: a live "human" secret, no .owner-name cache
        std::fs::write(root.secrets().join("human"), "the-old-secret").unwrap();
        ensure(&root).unwrap();
        // the secret moved to "owner" with its value preserved; no orphan left
        assert_eq!(read(&root, "owner").as_deref(), Some("the-old-secret"));
        assert!(
            !root.secrets().join("human").exists(),
            "the legacy human secret must be retired, not left as a live credential"
        );
        assert_eq!(owner_name(&root), "owner");
        assert!(read(&root, "kernel").is_some());
    }

    #[test]
    fn ensure_is_idempotent_and_keeps_the_owner_secret() {
        let root = tmp_root();
        ensure(&root).unwrap();
        let first = read(&root, "owner").unwrap();
        ensure(&root).unwrap();
        assert_eq!(read(&root, "owner").as_deref(), Some(first.as_str()));
    }
}

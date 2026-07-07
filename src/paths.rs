use anyhow::{bail, Result};
use std::path::PathBuf;

/// A lanius root: the directory holding lanius.db, trace.jsonl, packages/,
/// config/, run/. Identity is the path; there is no registry.
#[derive(Clone, Debug)]
pub struct Root {
    pub dir: PathBuf,
}

impl Root {
    pub fn db(&self) -> PathBuf {
        self.dir.join("lanius.db")
    }
    /// The legacy db filenames (pre-rename). Only db::open's one-time migration
    /// references them; everything else uses db().
    pub fn legacy_db(&self) -> PathBuf {
        self.dir.join("harness.db")
    }
    pub fn legacy_db_candidates(&self) -> [PathBuf; 2] {
        [self.dir.join("elanus.db"), self.legacy_db()]
    }
    pub fn trace_file(&self) -> PathBuf {
        self.dir.join("trace.jsonl")
    }
    pub fn recorder_file(&self) -> PathBuf {
        self.dir.join("recorder.toml")
    }
    pub fn bus_file(&self) -> PathBuf {
        self.dir.join("bus.toml")
    }
    pub fn packages(&self) -> PathBuf {
        self.dir.join("packages")
    }
    pub fn profiles(&self) -> PathBuf {
        self.dir.join("profiles")
    }
    pub fn profile_dir(&self, name: &str) -> PathBuf {
        let agents = self.config_agents();
        if agents.exists() || self.profiles().is_symlink() {
            agents.join(name)
        } else {
            self.profiles().join(name)
        }
    }
    pub fn run_dir(&self) -> PathBuf {
        self.dir.join("run")
    }
    /// The fenced secret store (docs/identity.md): kernel-minted credentials
    /// and, later, the ledger-integrity key. The kernel (uncaged) reads and
    /// writes it; every actor's cage denies it both ways, so a secret here is
    /// not readable by another actor. Empty today; the credential increments
    /// fill it.
    pub fn secrets(&self) -> PathBuf {
        self.dir.join(".secrets")
    }
    /// The configuration repository (docs/config.md): a kernel-owned Git repo
    /// whose `live` branch is the materialized truth for package configuration.
    /// Kernel-only-writable; the cage fences it (and its `.git`) from actors,
    /// the way it fences profiles and the secret store.
    pub fn config(&self) -> PathBuf {
        self.dir.join("config")
    }
    /// Per-package config files live here: config/packages/<name>.toml.
    pub fn config_packages(&self) -> PathBuf {
        self.config().join("packages")
    }
    /// Per-agent profile files live here on the config repo's `live` branch.
    /// `<root>/profiles` is kept as a compatibility symlink on Unix so older
    /// scripts and tests still use the familiar path while the tracked truth is
    /// `config/agents/<name>/profile.toml`.
    pub fn config_agents(&self) -> PathBuf {
        self.config().join("agents")
    }
}

/// The default lanius root: ~/.lanius/root. One predictable place, no
/// cwd-dependence — running a daemon from the repo must not quietly make the
/// repo a root (it did, before this).
pub fn default_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(default_root_from(PathBuf::from(home)))
}

fn default_root_from(home: PathBuf) -> PathBuf {
    home.join(".lanius/root")
}

fn root_has_marker(dir: &std::path::Path) -> bool {
    dir.join("lanius.db").exists()
        || dir.join("elanus.db").exists()
        || dir.join("harness.db").exists()
}

/// Resolution order: explicit flag > $LANIUS_ROOT (or legacy $HARNESS_ROOT) >
/// ~/.lanius/root. The old "walk up from cwd looking for the db" rule is gone:
/// it made the active root a function of where you happened to be standing.
pub fn resolve(cli: Option<PathBuf>) -> Result<Root> {
    let home = std::env::var("HOME")?;
    resolve_impl(
        cli,
        crate::envcompat::read("ROOT"),
        default_root_from(PathBuf::from(&home)),
        PathBuf::from(home).join(".elanus/root"),
    )
}

fn resolve_impl(
    cli: Option<PathBuf>,
    root_env: Option<String>,
    default_root: PathBuf,
    legacy_root: PathBuf,
) -> Result<Root> {
    if let Some(dir) = cli {
        return Ok(Root { dir: canon(dir)? });
    }
    if let Some(dir) = root_env {
        return Ok(Root {
            dir: canon(PathBuf::from(dir))?,
        });
    }
    let new = default_root;
    // Either the current db name or a legacy one marks an existing root.
    if root_has_marker(&new) {
        return Ok(Root { dir: canon(new)? });
    }
    if root_has_marker(&legacy_root) {
        return Ok(Root {
            dir: canon(legacy_root)?,
        });
    }
    bail!(
        "no lanius root at {} — run `lanius init` to create it, or override with $LANIUS_ROOT / -C <dir>",
        new.display()
    )
}

fn canon(p: PathBuf) -> Result<PathBuf> {
    if p.exists() {
        Ok(p.canonicalize()?)
    } else {
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_lanius_root_then_legacy_root() {
        let base = std::env::temp_dir().join(format!("el-paths-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let lanius = base.join(".lanius/root");
        let elanus = base.join(".elanus/root");
        std::fs::create_dir_all(&elanus).unwrap();
        std::fs::write(elanus.join("elanus.db"), "").unwrap();
        let resolved =
            resolve_impl(None, None, default_root_from(base.clone()), elanus.clone()).unwrap();
        assert_eq!(resolved.dir, elanus.canonicalize().unwrap());

        std::fs::create_dir_all(&lanius).unwrap();
        std::fs::write(lanius.join("lanius.db"), "").unwrap();
        let resolved = resolve_impl(None, None, default_root_from(base.clone()), elanus).unwrap();
        assert_eq!(resolved.dir, lanius.canonicalize().unwrap());
        std::fs::remove_dir_all(&base).ok();
    }
}

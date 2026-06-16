use anyhow::{bail, Result};
use std::path::PathBuf;

/// An elanus root: the directory holding elanus.db, trace.jsonl, skills/,
/// handlers.d/, profiles/, run/. Identity is the path; there is no registry.
#[derive(Clone, Debug)]
pub struct Root {
    pub dir: PathBuf,
}

impl Root {
    pub fn db(&self) -> PathBuf {
        self.dir.join("elanus.db")
    }
    /// The legacy db filename (pre-rename). Only db::open's one-time migration
    /// references it; everything else uses db().
    pub fn legacy_db(&self) -> PathBuf {
        self.dir.join("harness.db")
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
        self.profiles().join(name)
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
}

/// The default harness root: ~/.elanus/root. One predictable place, no
/// cwd-dependence — running a daemon from the repo must not quietly make the
/// repo a root (it did, before this).
pub fn default_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".elanus/root"))
}

/// Resolution order: explicit flag > $ELANUS_ROOT (or legacy $HARNESS_ROOT) >
/// ~/.elanus/root. The old "walk up from cwd looking for the db" rule is gone:
/// it made the active root a function of where you happened to be standing.
pub fn resolve(cli: Option<PathBuf>) -> Result<Root> {
    if let Some(dir) = cli {
        return Ok(Root { dir: canon(dir)? });
    }
    if let Some(dir) = crate::envcompat::read("ROOT") {
        return Ok(Root { dir: canon(PathBuf::from(dir))? });
    }
    let def = default_root()?;
    // Either the current db name or the legacy one marks an existing root (an
    // old root is migrated to elanus.db on first open).
    if def.join("elanus.db").exists() || def.join("harness.db").exists() {
        return Ok(Root { dir: canon(def)? });
    }
    bail!(
        "no elanus root at {} — run `elanus init` to create it, or override with $ELANUS_ROOT / -C <dir>",
        def.display()
    )
}

fn canon(p: PathBuf) -> Result<PathBuf> {
    if p.exists() {
        Ok(p.canonicalize()?)
    } else {
        Ok(p)
    }
}

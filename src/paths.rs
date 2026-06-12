use anyhow::{bail, Result};
use std::path::PathBuf;

/// A harness root: the directory holding harness.db, trace.jsonl, skills/,
/// handlers.d/, profiles/, run/. Identity is the path; there is no registry.
#[derive(Clone, Debug)]
pub struct Root {
    pub dir: PathBuf,
}

impl Root {
    pub fn db(&self) -> PathBuf {
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
}

/// The default harness root: ~/.elanus/root. One predictable place, no
/// cwd-dependence — running a daemon from the repo must not quietly make the
/// repo a root (it did, before this).
pub fn default_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".elanus/root"))
}

/// Resolution order: explicit flag > HARNESS_ROOT env > ~/.elanus/root.
/// The old "walk up from cwd looking for harness.db" rule is gone: it made
/// the active root a function of where you happened to be standing.
pub fn resolve(cli: Option<PathBuf>) -> Result<Root> {
    if let Some(dir) = cli {
        return Ok(Root { dir: canon(dir)? });
    }
    if let Ok(dir) = std::env::var("HARNESS_ROOT") {
        return Ok(Root { dir: canon(PathBuf::from(dir))? });
    }
    let def = default_root()?;
    if def.join("harness.db").exists() {
        return Ok(Root { dir: canon(def)? });
    }
    bail!(
        "no harness root at {} — run `elanus init` to create it, or override with HARNESS_ROOT / -C <dir>",
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

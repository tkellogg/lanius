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
    pub fn skills(&self) -> PathBuf {
        self.dir.join("skills")
    }
    pub fn handlers(&self) -> PathBuf {
        self.dir.join("handlers.d")
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

/// Resolution order: explicit flag > HARNESS_ROOT env > walk up from cwd
/// looking for harness.db.
pub fn resolve(cli: Option<PathBuf>) -> Result<Root> {
    if let Some(dir) = cli {
        return Ok(Root { dir: canon(dir)? });
    }
    if let Ok(dir) = std::env::var("HARNESS_ROOT") {
        return Ok(Root { dir: canon(PathBuf::from(dir))? });
    }
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("harness.db").exists() {
            return Ok(Root { dir: cur });
        }
        if !cur.pop() {
            break;
        }
    }
    bail!("no harness root found: run `elanus init [dir]`, set HARNESS_ROOT, or pass -C <dir>")
}

fn canon(p: PathBuf) -> Result<PathBuf> {
    if p.exists() {
        Ok(p.canonicalize()?)
    } else {
        Ok(p)
    }
}

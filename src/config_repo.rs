//! The configuration repository — `<root>/config`, a kernel-owned Git repo
//! whose `live` branch is the materialized truth for PACKAGE and AGENT
//! configuration (docs/config.md). Live config is a branch; a human-direct write is a commit
//! on `live`; acceptance provenance lives in the ledger, not the commit (the
//! commit author is a fixed kernel identity — git holds content, the ledger
//! holds "who accepted this").
//!
//! This is the first git usage in the codebase. Every invocation shells out to
//! the system `git` with hooks off, ambient config neutralized, and the
//! attribute machinery disabled (D2): the repo is kernel territory today, but
//! treating its machinery as untrusted from the start is what makes the
//! agent-clone round-trip (increment 3 — a clone the agent commits into) safe to
//! add on this same code path. We disable hooks (`core.hooksPath=/dev/null`),
//! refuse to read the operator's global/system gitconfig
//! (`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM=/dev/null`, so an ambient
//! `[filter] smudge=…` or alias can't run), turn off attribute-driven filters
//! (`GIT_ATTR_NOSYSTEM`), and neutralize a stray ambient `GIT_DIR`/`GIT_WORK_TREE`.
//! NOTE (docs/security.md): increment 2 never *checks out* untrusted content, so
//! repo-LOCAL `.gitattributes`-driven clean/smudge filters cannot fire here. The
//! increment-3 path that materializes an agent's fetched tree must additionally
//! read content via plumbing (`git cat-file`/`git diff`), never a working-tree
//! checkout that would run a smudge filter.

use crate::paths::Root;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// The committer identity stamped on every config commit. Deliberately a fixed,
/// non-human name: the trustworthy "who accepted this" is the broker-stamped
/// ledger event that references the commit SHA, never the commit author
/// (docs/config.md "provenance from the ledger, not the commit").
const COMMITTER_NAME: &str = "elanus";
const COMMITTER_EMAIL: &str = "elanus@localhost";
/// The materialized-truth branch (docs/config.md). Readers read the working
/// tree, which is always a checkout of this branch.
const LIVE: &str = "live";
/// Upper bound on a single proposal's added objects (package config is tiny);
/// bounds what an agent can copy into the kernel-owned object store, and the
/// per-proposal changed-file count, against a disk/ledger DoS.
const MAX_PROPOSAL_BYTES: u64 = 1 << 20; // 1 MiB
const MAX_PROPOSAL_FILES: usize = 256;

const CONFIG_README: &str = "\
# config — package configuration (docs/config.md)

This is a kernel-owned Git repository. Its `live` branch is the materialized
truth: per-package settings live at `packages/<name>.toml`. Do not edit by hand
while the daemon runs — use `elanus config set <package> <key> <value>` or
`elanus profile set <agent> <key=value>`, which commit changes on `live` and
record who accepted them in the ledger. Package settings live at
`packages/<name>.toml`; agent profiles live at `agents/<name>/profile.toml`.
Agents never write live config; they only propose (a `proposal/<id>` branch).
";

/// Apply the untrusted-input hardening every git invocation in this module
/// shares (D2 / docs/security.md entry 18): no hooks, no signing, no fsmonitor,
/// no operator global/system gitconfig (which can carry a `[filter] smudge=…`,
/// alias, or pager that would execute under our flags — a real, reproduced
/// leak), no system attributes file, no ambient GIT_DIR/GIT_WORK_TREE hijack,
/// never prompt. Used for BOTH the live-repo ops and the clone of the untrusted
/// agent tree — the clone round-trip is the reason this discipline exists.
fn harden(c: &mut Command) {
    c.arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg(format!("user.name={COMMITTER_NAME}"))
        .arg("-c")
        .arg(format!("user.email={COMMITTER_EMAIL}"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0");
}

/// A hardened `git` invocation rooted in the config (live) repo.
fn git(root: &Root) -> Command {
    let mut c = Command::new("git");
    harden(&mut c);
    c.arg("-C").arg(root.config());
    c
}

/// A hardened `git` invocation NOT rooted in the live repo (e.g. `git clone`,
/// which takes src + dest as arguments).
fn git_bare() -> Command {
    let mut c = Command::new("git");
    harden(&mut c);
    c
}

fn run_git(mut c: Command, what: &str) -> Result<String> {
    let out = c
        .output()
        .with_context(|| format!("running git {what} (is git installed?)"))?;
    if !out.status.success() {
        bail!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_run(root: &Root, args: &[&str], what: &str) -> Result<String> {
    let mut c = git(root);
    c.args(args);
    run_git(c, what)
}

/// Run a git command for its exit status only (no output, no bail). Used for
/// the probes where a non-zero exit is a normal answer ("no such branch",
/// "nothing staged") rather than an error.
fn git_ok(root: &Root, args: &[&str]) -> bool {
    git(root)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create the config repo if absent: `<root>/config` with `packages/` and
/// `agents/` dirs and
/// an initial commit on the `live` branch. Idempotent — a root that already has
/// the repo is left untouched. Called from `elanus init`.
pub fn init(root: &Root) -> Result<()> {
    let dir = root.config();
    // "Already initialized" means a real repo with a commit on `live`, not just
    // a `.git` directory: an init interrupted after `git init` but before the
    // first commit leaves an unborn HEAD, and guarding on `.git` alone would
    // call that done forever. `git init` is idempotent, so re-entering here
    // safely completes a half-built repo. (-C a non-existent dir fails the
    // probe, which is exactly what we want on a truly fresh root.)
    if git_ok(
        root,
        &["rev-parse", "--verify", "--quiet", "refs/heads/live"],
    ) {
        ensure_agent_store(root)?;
        let _ = commit_path(root, "agents", "config: migrate agent profiles into live");
        return Ok(());
    }
    std::fs::create_dir_all(root.config_packages())?;
    std::fs::create_dir_all(root.config_agents())?;
    ensure_agent_store(root)?;
    // 0700: package config can name external accounts and endpoints; keep it
    // owner-only on disk even though the cage is the real fence (mirrors how
    // init treats the secret store).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    git_run(root, &["init", "-b", LIVE], "init")?;
    // A README and a tracked, empty packages/ so the initial commit is real and
    // the layout is self-documenting.
    let keep = root.config_packages().join(".gitkeep");
    if !keep.exists() {
        std::fs::write(&keep, "")?;
    }
    let keep = root.config_agents().join(".gitkeep");
    if !keep.exists() {
        std::fs::write(&keep, "")?;
    }
    let readme = dir.join("README.md");
    if !readme.exists() {
        std::fs::write(&readme, CONFIG_README)?;
    }
    git_run(root, &["add", "-A"], "add (init)")?;
    // Only commit if there's something staged — re-entering a half-built repo
    // whose files were already committed must not error on an empty commit.
    if !git_ok(root, &["diff", "--cached", "--quiet"]) {
        git_run(
            root,
            &["commit", "-m", "config: initial live branch"],
            "commit (init)",
        )?;
    }
    Ok(())
}

/// Fold the old `<root>/profiles` tree into `config/agents` and keep the old
/// path as a compatibility symlink on Unix. This lets existing scripts keep
/// reading/writing `<root>/profiles/<name>/profile.toml` while Git tracks the
/// canonical file under `config/agents/<name>/profile.toml`.
fn ensure_agent_store(root: &Root) -> Result<()> {
    std::fs::create_dir_all(root.config_agents())?;
    let legacy = root.profiles();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if std::fs::symlink_metadata(&legacy)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let target = std::fs::read_link(&legacy)
                .with_context(|| format!("reading {} symlink", legacy.display()))?;
            let target_abs = if target.is_absolute() {
                target
            } else {
                legacy
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(target)
            };
            if target_abs == root.config_agents() {
                return Ok(());
            }
            bail!(
                "{} is a symlink to {}, not config/agents",
                legacy.display(),
                target_abs.display()
            );
        }
        if legacy.exists() {
            copy_tree_checked(&legacy, &root.config_agents())?;
            std::fs::remove_dir_all(&legacy).with_context(|| {
                format!("replacing {} with config/agents symlink", legacy.display())
            })?;
        }
        symlink(root.config_agents(), &legacy)
            .with_context(|| format!("linking {} -> config/agents", legacy.display()))?;
    }

    #[cfg(not(unix))]
    {
        if legacy.exists() {
            copy_tree_checked(&legacy, &root.config_agents())?;
        } else {
            std::fs::create_dir_all(&legacy)?;
        }
    }
    Ok(())
}

fn copy_tree_checked(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let src = e.path();
        let dst = to.join(e.file_name());
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_tree_checked(&src, &dst)?;
        } else if ft.is_file() {
            if dst.exists() {
                let src_bytes =
                    std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?;
                let dst_bytes =
                    std::fs::read(&dst).with_context(|| format!("reading {}", dst.display()))?;
                if src_bytes != dst_bytes {
                    bail!(
                        "profile migration conflict: {} and {} differ; resolve before starting",
                        src.display(),
                        dst.display()
                    );
                }
            } else {
                std::fs::copy(&src, &dst)
                    .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
            }
        } else {
            bail!(
                "profile migration refuses non-regular entry {}",
                src.display()
            );
        }
    }
    Ok(())
}

/// Set a dotted `key` in `config/packages/<pkg>.toml`, creating the file if
/// absent, and commit the change on `live`. Comments are preserved (toml_edit).
/// The value parses as TOML when it can (ints, bools, arrays, quoted strings),
/// else a bare string — the same rule as `profile set`. Returns `(sha, changed)`:
/// the resulting commit SHA, and whether this set actually changed anything (an
/// idempotent set is a no-op — no commit, `changed=false`).
///
/// Crash-resistant: the working-tree file is updated atomically (write-temp +
/// rename, so the daemon's fingerprint reader never sees a torn file), and if
/// any git step fails the prior file content is restored — the daemon must never
/// reload onto a change that was never accepted/committed.
pub fn set_key(root: &Root, pkg: &str, key: &str, value: &str) -> Result<(String, bool)> {
    valid_pkg(pkg)?;
    let rel = format!("packages/{pkg}.toml");
    let path = root.config().join(&rel);
    let prior = std::fs::read_to_string(&path).ok();
    let mut doc: toml_edit::DocumentMut = prior
        .clone()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("parsing existing config {}", path.display()))?;
    set_dotted(&mut doc, key, value)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&path, &doc.to_string())?;
    match commit_path(root, &rel, &format!("config: set {pkg}.{key}")) {
        Ok(changed) => Ok((current_sha(root)?, changed)),
        Err(e) => {
            // Roll the working tree back so the running daemon never reloads onto
            // an un-accepted change, and drop the now-poisoned staged entry.
            match &prior {
                Some(p) => {
                    let _ = write_atomic(&path, p);
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            let _ = git_run(root, &["reset", "-q", "--", &rel], "reset (rollback)");
            Err(e)
        }
    }
}

/// Stage and commit exactly one path. Returns whether a commit happened: an
/// identical (no-op) set stages nothing for the path, so there is nothing to
/// commit. Both the change check and the commit are PATH-SCOPED, so an unrelated
/// dirty/staged file in the repo can neither mask a real change nor ride into
/// this commit (it matters once increment 3 stages fetched proposal content).
fn commit_path(root: &Root, rel: &str, msg: &str) -> Result<bool> {
    git_run(root, &["add", "--", rel], "add")?;
    // `git diff --cached --quiet -- <rel>` exits 0 (success) when nothing is
    // staged for that path — i.e. a no-op set.
    if git_ok(root, &["diff", "--cached", "--quiet", "--", rel]) {
        return Ok(false);
    }
    git_run(root, &["commit", "-m", msg, "--", rel], "commit")?;
    Ok(true)
}

/// Commit one agent profile subtree (`config/agents/<name>/...`) on `live`.
/// Returns `(sha, changed)` so callers can emit a ledger event only for real
/// changes. `name` is already profile-name validated by the caller.
pub fn commit_agent(root: &Root, name: &str, msg: &str) -> Result<(String, bool)> {
    let rel = format!("agents/{name}");
    let changed = commit_path(root, &rel, msg)?;
    Ok((current_sha(root)?, changed))
}

/// Drop any staged profile content for one agent. Used after a failed profile
/// write rolls the working tree back, so the index cannot carry the rejected
/// candidate into a later config commit.
pub fn reset_agent(root: &Root, name: &str) {
    let rel = format!("agents/{name}");
    let _ = git_run(root, &["reset", "-q", "--", &rel], "reset agent");
}

/// Write a file atomically: a sibling temp + rename (atomic on the same
/// filesystem), so a concurrent reader — the daemon's fingerprint sweep — sees
/// either the whole old file or the whole new one, never a truncated one.
fn write_atomic(path: &std::path::Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The raw TOML for a package's config, or None if it has none.
pub fn read_package(root: &Root, pkg: &str) -> Result<Option<String>> {
    valid_pkg(pkg)?;
    let path = root.config().join(format!("packages/{pkg}.toml"));
    // Defense in depth: never FOLLOW a symlink here. Path-discipline already
    // refuses a symlink proposal from ever merging, but a config read must not
    // become an arbitrary-file disclosure even if one reached the live tree
    // another way (e.g. a hand edit). A symlinked config file reads as absent.
    if std::fs::symlink_metadata(&path)
        .map(|m| m.is_symlink())
        .unwrap_or(false)
    {
        return Ok(None);
    }
    Ok(std::fs::read_to_string(path).ok())
}

/// One dotted key's value as a TOML fragment (e.g. `["a", "b"]` or `3`), or None
/// if the package or key is absent.
pub fn get_key(root: &Root, pkg: &str, key: &str) -> Result<Option<String>> {
    let Some(raw) = read_package(root, pkg)? else {
        return Ok(None);
    };
    let doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parsing config for {pkg}"))?;
    let mut item: &toml_edit::Item = doc.as_item();
    for seg in key.split('.') {
        match item.get(seg) {
            Some(next) => item = next,
            None => return Ok(None),
        }
    }
    Ok(item.as_value().map(|v| v.to_string().trim().to_string()))
}

/// The names of every package that has a config file (config/packages/*.toml).
pub fn packages_with_config(root: &Root) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.config_packages()) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// A cheap content fingerprint of a package's config file. The supervisor
/// compares this across ticks to decide a running daemon needs a restart with
/// fresh config (docs/config.md D3). An absent file is a stable empty marker, so
/// creating the first config triggers exactly one reload.
pub fn fingerprint(root: &Root, pkg: &str) -> String {
    let path = root.config().join(format!("packages/{pkg}.toml"));
    match std::fs::read(&path) {
        Ok(bytes) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            format!("{:x}", h.finish())
        }
        Err(_) => String::new(),
    }
}

// ── Agent proposals: the Git round-trip (docs/config.md, increment 3) ────────

/// A proposed configuration change harvested from an agent's clone: a kernel ref
/// (`refs/proposals/<id>`) plus the metadata to show and decide it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Proposal {
    pub id: String,
    /// The proposing agent (self-reported, like every exec-origin event); the
    /// trustworthy identity is the ledger acceptance event, not this.
    pub by: String,
    /// The branch the agent committed in its clone (e.g. "proposal/accounts").
    pub branch: String,
    /// Files changed vs `live` — what the path-discipline + key allowlist judge.
    pub files: Vec<String>,
    /// The proposal's tip commit (content provenance; the ledger holds identity).
    pub commit: String,
}

/// Place a disposable clone of the config repo at `dest` for an agent to edit and
/// commit a `proposal/<id>` branch into, inside its cage. `--no-local` copies
/// objects over the regular transport so the agent-writable clone shares NO
/// object inodes with the kernel-owned live repo (a hostile shell could otherwise
/// truncate a shared object file and corrupt `live`).
pub fn clone_for_agent(root: &Root, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut c = git_bare();
    c.args(["clone", "--no-local", "--quiet", "--"]);
    c.arg(root.config()).arg(dest);
    run_git(c, "clone for agent")?;
    Ok(())
}

/// Reap an agent's clone at run end: harvest every `proposal/*` branch into the
/// live repo under `refs/proposals/<id>` and return one record per proposal,
/// diffed vs `live`. The clone is UNTRUSTED (docs/security.md entry 18): we
/// neutralize its `.git/config` first (so no planted `uploadpack.*` hook or
/// alias runs when we read from it), fetch objects + the one named ref — never a
/// checkout — and read the diff via plumbing. Empty if there is no proposal.
pub fn reap_proposals(root: &Root, clone: &Path, by: &str) -> Result<Vec<Proposal>> {
    let gitdir = clone.join(".git");
    // The clone is untrusted FILESYSTEM, not just untrusted git input: .git must
    // be a REAL directory, not a symlink the agent pointed at kernel territory
    // (symlink_metadata does not follow — `is_dir()` alone would follow a
    // .git→/elsewhere link and let the neutralization below clobber it).
    match std::fs::symlink_metadata(&gitdir) {
        Ok(m) if m.is_dir() => {}
        _ => return Ok(vec![]),
    }
    // Neutralize the untrusted clone's config BEFORE reading from it. A local
    // fetch can spawn the clone's upload-pack, which would honor a planted
    // `uploadpack.packObjectsHook`/alias/fsmonitor. REMOVE the file first — an
    // agent can make .git/config a symlink to an arbitrary path, and a plain
    // write would follow it and overwrite that target (an uncaged kernel write);
    // remove_file deletes the link itself, then we write a real, minimal config.
    // If neutralization fails, do NOT read from the clone (fail closed).
    let cfg = gitdir.join("config");
    let _ = std::fs::remove_file(&cfg);
    if std::fs::write(
        &cfg,
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
    )
    .is_err()
    {
        return Ok(vec![]);
    }
    let mut lc = git_bare();
    lc.arg("--git-dir").arg(&gitdir).args([
        "for-each-ref",
        "--format=%(refname)",
        "refs/heads/proposal/",
    ]);
    let listing = run_git(lc, "list proposal branches").unwrap_or_default();
    let mut out = Vec::new();
    for refname in listing.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let Some(short) = refname.strip_prefix("refs/heads/") else {
            continue;
        };
        if !valid_proposal_branch(short) {
            continue;
        }
        // Bound what an agent can copy into the kernel-owned live object store:
        // measure the proposal's added objects IN THE CLONE first and skip an
        // oversized one before fetching (package config is tiny — KB, not MB).
        if clone_branch_bytes(&gitdir, short) > MAX_PROPOSAL_BYTES {
            eprintln!("[config] proposal {short} exceeds the size cap — skipped");
            continue;
        }
        let id = new_proposal_id();
        let dest_ref = format!("refs/proposals/{id}");
        // Fetch JUST this branch into the live repo under the kernel ref: objects
        // + the one ref, no tags, no checkout, hooks/filters off (harden).
        let refspec = format!("+{short}:{dest_ref}");
        let mut f = git(root);
        f.args(["fetch", "--no-tags", "--quiet", "--"])
            .arg(clone)
            .arg(&refspec);
        if run_git(f, "fetch proposal").is_err() {
            continue;
        }
        let commit =
            git_run(root, &["rev-parse", &dest_ref], "rev-parse proposal").unwrap_or_default();
        let files = changed_files(root, &dest_ref).unwrap_or_default();
        out.push(Proposal {
            id,
            by: by.to_string(),
            branch: short.to_string(),
            files,
            commit,
        });
    }
    Ok(out)
}

/// Files a committish changed relative to `live` (only the proposal's own
/// contribution — three-dot, against the merge-base). Plumbing: no checkout, no
/// textconv (a `.gitattributes` diff driver would otherwise be a code path).
fn changed_files(root: &Root, tip: &str) -> Result<Vec<String>> {
    Ok(changed_entries(root, tip)?
        .into_iter()
        .map(|(_, p)| p)
        .collect())
}

/// Bytes of objects unique to `branch` (vs `live`) IN THE CLONE — measured
/// before fetching so an oversized proposal never lands in the live store.
/// Best-effort: any failure (old git, missing live) reports 0 (don't block).
fn clone_branch_bytes(gitdir: &Path, branch: &str) -> u64 {
    let mut c = git_bare();
    c.arg("--git-dir").arg(gitdir).args([
        "rev-list",
        "--objects",
        "--disk-usage",
        &format!("{LIVE}..{branch}"),
    ]);
    run_git(c, "rev-list disk-usage")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Changed paths AND their resulting (destination) git tree modes, vs `live`
/// (three-dot). `--raw` carries the mode that `--name-only` hides; this is what
/// lets path-discipline reject a symlink/gitlink/exec entry that a by-name check
/// waves through (the symlink-exfil class both adversarial reviews found).
/// `--no-renames` keeps the format a simple per-path `:srcmode dstmode … status`.
/// Returns (dstmode, path); a deletion has dstmode "000000".
fn changed_entries(root: &Root, tip: &str) -> Result<Vec<(String, String)>> {
    let range = format!("{LIVE}...{tip}");
    let out = git_run(
        root,
        &["diff", "--raw", "--no-renames", "--no-textconv", &range],
        "diff --raw",
    )?;
    let mut entries = Vec::new();
    for line in out.lines() {
        // ":<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>"
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let dstmode = fields[1].trim_start_matches(':').to_string();
        let path = path.trim().to_string();
        if !path.is_empty() {
            entries.push((dstmode, path));
        }
    }
    Ok(entries)
}

/// Every recorded proposal (`refs/proposals/*`). `by`/`branch` are not in git
/// (the ledger holds those); callers that need them read the obs/config/proposed
/// event. Listing exists so a human can see what is waiting.
pub fn list_proposals(root: &Root) -> Result<Vec<Proposal>> {
    let listing = git_run(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/proposals/"],
        "list proposals",
    )
    .unwrap_or_default();
    let mut out = Vec::new();
    for refname in listing.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let Some(id) = refname.strip_prefix("refs/proposals/") else {
            continue;
        };
        let commit = git_run(root, &["rev-parse", refname], "rev-parse").unwrap_or_default();
        let files = changed_files(root, refname).unwrap_or_default();
        out.push(Proposal {
            id: id.to_string(),
            by: String::new(),
            branch: String::new(),
            files,
            commit,
        });
    }
    Ok(out)
}

/// One proposal's unified diff vs `live` (plumbing, no checkout, no textconv).
pub fn proposal_diff(root: &Root, id: &str) -> Result<String> {
    let dest_ref = proposal_ref(id)?;
    git_run(
        root,
        &["diff", "--no-textconv", &format!("{LIVE}...{dest_ref}")],
        "proposal diff",
    )
}

/// Validate a proposal id and return its full ref. Guards against ref injection
/// (the id reaches `git` as a ref component).
fn proposal_ref(id: &str) -> Result<String> {
    if id.is_empty() || id.len() > 40 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("bad proposal id {id:?}");
    }
    Ok(format!("refs/proposals/{id}"))
}

fn new_proposal_id() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(12)
        .collect()
}

/// A proposal branch is exactly "proposal/<seg>" with one safe segment.
fn valid_proposal_branch(short: &str) -> bool {
    match short.strip_prefix("proposal/") {
        Some(seg) => {
            !seg.is_empty()
                && seg.len() <= 64
                && !seg.contains('/')
                && !seg.contains("..")
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        }
        None => false,
    }
}

// ── Acceptance: merge a proposal into live (docs/config.md, increment 4) ─────

/// Path-discipline — the core safety gate. A proposal may ONLY change package
/// settings files `config/packages/<pkg>.toml`: nothing else (no `.gitattributes`,
/// no `.git/`, no files outside the package surface, no subdirs). Enforced before
/// any merge, for human and auto-accept alike, so the proposal surface is the
/// only thing acceptance can ever apply (it also closes the entry-18 smuggle:
/// a tree carrying a filter `.gitattributes` is refused before any checkout).
/// Returns the changed package names.
pub fn proposal_packages(root: &Root, id: &str) -> Result<Vec<String>> {
    let dest_ref = proposal_ref(id)?;
    if !git_ok(root, &["rev-parse", "--verify", "--quiet", &dest_ref]) {
        bail!("no proposal {id:?}");
    }
    let entries = changed_entries(root, &dest_ref)?;
    if entries.len() > MAX_PROPOSAL_FILES {
        bail!(
            "proposal {id} changes {} files — far more than any package config \
             change (max {MAX_PROPOSAL_FILES}); refusing",
            entries.len()
        );
    }
    let mut pkgs = Vec::new();
    for (mode, f) in entries {
        // Tree MODE first: only a regular non-executable blob (100644) — or a
        // deletion (000000) — may ride a proposal. A symlink (120000) would
        // merge a link into kernel-owned live and read through to an arbitrary
        // file; a gitlink (160000) a submodule; an exec bit (100755) a runnable
        // file. Refuse them before path/name checks (the always-stop gate).
        if mode != "100644" && mode != "000000" {
            bail!(
                "proposal {id} changes {f:?} with git mode {mode} — only regular \
                 config files are allowed (no symlinks, submodules, or exec bits)"
            );
        }
        match f
            .strip_prefix("packages/")
            .and_then(|r| r.strip_suffix(".toml"))
        {
            Some(p) if valid_pkg(p).is_ok() => {
                if !pkgs.contains(&p.to_string()) {
                    pkgs.push(p.to_string());
                }
            }
            _ => bail!(
                "proposal {id} changes {f:?}, which is not a package config file \
                 (config/packages/<name>.toml) — refusing"
            ),
        }
    }
    if pkgs.is_empty() {
        bail!("proposal {id} changes nothing");
    }
    Ok(pkgs)
}

/// The dotted config keys a proposal changes for one package (live vs the
/// proposal), for the "assisted" agent-tunable allowlist check. Reads both blobs
/// via plumbing (`cat-file`, no checkout, no filters).
pub fn proposal_changed_keys(root: &Root, id: &str, pkg: &str) -> Result<Vec<String>> {
    let dest_ref = proposal_ref(id)?;
    let rel = format!("packages/{pkg}.toml");
    let live_toml = git_run(
        root,
        &["cat-file", "-p", &format!("{LIVE}:{rel}")],
        "cat live",
    )
    .unwrap_or_default();
    let prop_toml = git_run(
        root,
        &["cat-file", "-p", &format!("{dest_ref}:{rel}")],
        "cat proposal",
    )
    .unwrap_or_default();
    Ok(changed_dotted_keys(&live_toml, &prop_toml))
}

/// Accept a proposal: merge `refs/proposals/<id>` into `live` (hooks off; updates
/// the working tree so the supervisor reload fires) and delete the proposal ref.
/// Enforces path-discipline FIRST, so a proposal touching anything but package
/// config is never merged — by a human or by autonomy. Returns the merge commit
/// SHA. A conflict aborts cleanly and errors (resolved out of band; the conflict
/// UI is still open, docs/config.md).
pub fn accept_proposal(root: &Root, id: &str) -> Result<String> {
    proposal_packages(root, id)?; // path-discipline, unconditional
    let dest_ref = proposal_ref(id)?;
    // Be on live with the working tree current, then merge the proposal.
    git_run(root, &["checkout", "--quiet", LIVE], "checkout live")?;
    let msg = format!("config: accept proposal {id}");
    let mut m = git(root);
    m.args(["merge", "--no-ff", "--no-edit", "-m", &msg, &dest_ref]);
    if run_git(m, "merge proposal").is_err() {
        let _ = git_run(root, &["merge", "--abort"], "merge --abort");
        bail!("proposal {id} conflicts with the current live config — resolve it manually");
    }
    let sha = current_sha(root)?;
    let _ = git_run(
        root,
        &["update-ref", "-d", &dest_ref],
        "delete proposal ref",
    );
    Ok(sha)
}

/// Drop a proposal (delete its ref). The objects become unreachable (gc prunes
/// them); live is untouched.
pub fn decline_proposal(root: &Root, id: &str) -> Result<()> {
    let dest_ref = proposal_ref(id)?;
    if !git_ok(root, &["rev-parse", "--verify", "--quiet", &dest_ref]) {
        bail!("no proposal {id:?}");
    }
    git_run(
        root,
        &["update-ref", "-d", &dest_ref],
        "delete proposal ref",
    )?;
    Ok(())
}

/// Dotted keys that differ between two TOML documents (added, removed, or
/// changed). Leaf granularity (an array or scalar is one key); a changed nested
/// table reports its leaf paths.
fn changed_dotted_keys(live: &str, prop: &str) -> Vec<String> {
    let lv: toml::Value =
        toml::from_str(live).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let pv: toml::Value =
        toml::from_str(prop).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let mut lm = BTreeMap::new();
    let mut pm = BTreeMap::new();
    flatten_toml(&lv, "", &mut lm);
    flatten_toml(&pv, "", &mut pm);
    let mut changed: Vec<String> = Vec::new();
    for (k, pval) in &pm {
        if lm.get(k) != Some(pval) {
            changed.push(k.clone());
        }
    }
    for k in lm.keys() {
        if !pm.contains_key(k) {
            changed.push(k.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

fn flatten_toml(v: &toml::Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        toml::Value::Table(t) => {
            for (k, val) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_toml(val, &key, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

fn current_sha(root: &Root) -> Result<String> {
    git_run(root, &["rev-parse", "HEAD"], "rev-parse")
}

fn set_dotted(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<()> {
    let segs: Vec<&str> = key.split('.').collect();
    if segs.iter().any(|s| s.is_empty()) {
        bail!("bad key path {key:?}");
    }
    let val = parse_value(value);
    let mut item: &mut toml_edit::Item = doc.as_item_mut();
    for seg in &segs[..segs.len() - 1] {
        match item.get(seg) {
            None => item[seg] = toml_edit::Item::Table(toml_edit::Table::new()),
            // Descending into an existing scalar would panic toml_edit's Index
            // ("index not found"); refuse with a clear error instead.
            Some(it) if !it.is_table() && !it.is_inline_table() => {
                bail!("cannot set {key:?}: {seg:?} is already a value, not a table");
            }
            Some(_) => {}
        }
        item = &mut item[seg];
    }
    item[segs[segs.len() - 1]] = toml_edit::Item::Value(val);
    Ok(())
}

/// Parse a value as TOML through a scratch doc so arrays/ints/bools/strings all
/// work; fall back to a bare string. (Mirrors profilecli::parse_value.)
fn parse_value(raw: &str) -> toml_edit::Value {
    let trimmed = raw.trim();
    if let Ok(doc) = format!("x = {trimmed}").parse::<toml_edit::DocumentMut>() {
        if let Some(v) = doc["x"].as_value() {
            return v.clone();
        }
    }
    toml_edit::Value::from(trimmed)
}

/// A package name is a path segment, a topic level, and a SQL key — keep it safe.
/// Canonical form is lowercase ASCII: a mixed-case name (e.g. `History`) would
/// case-fold to a DIFFERENT string than the protected set (`history`) yet collide
/// with it on a case-insensitive filesystem, sneaking a protected-package change
/// past the always-stop. Rejecting non-lowercase here is the single chokepoint
/// that closes that class for every downstream exact-string comparison.
fn valid_pkg(pkg: &str) -> Result<()> {
    if pkg.is_empty()
        || pkg.len() > 64
        || !pkg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        bail!("bad package name {pkg:?} (lowercase letters, digits, dash, underscore)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-cfgrepo-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    #[test]
    fn init_set_read_roundtrip() {
        let root = scratch("rt");
        init(&root).unwrap();
        assert!(root.config().join(".git").exists());
        // Fresh package config is created on first set; changed=true.
        let (sha1, changed1) = set_key(&root, "watcher", "accounts", r#"["alice","bob"]"#).unwrap();
        assert!(!sha1.is_empty());
        assert!(changed1, "first set is a real change");
        // toml_edit preserves the value's input formatting verbatim.
        assert_eq!(
            get_key(&root, "watcher", "accounts").unwrap().as_deref(),
            Some(r#"["alice","bob"]"#)
        );
        // A real change commits a new SHA; the fingerprint moves with it.
        let fp1 = fingerprint(&root, "watcher");
        let (sha2, changed2) = set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        assert!(changed2);
        assert_ne!(sha1, sha2, "a content change is a new commit");
        assert_ne!(
            fp1,
            fingerprint(&root, "watcher"),
            "fingerprint tracks content"
        );
        // An idempotent set is a no-op: no commit, changed=false.
        let (sha3, changed3) = set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        assert_eq!(sha2, sha3, "no-op set must not create a commit");
        assert!(!changed3, "no-op set reports changed=false");
        // A no-op even when an UNRELATED file is dirty in the repo (path-scoped
        // check) — and that dirty file must NOT ride into a config commit.
        std::fs::write(root.config().join("README.md"), "tampered\n").unwrap();
        let (_, changed4) = set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        assert!(
            !changed4,
            "unrelated dirt must not look like a config change"
        );
        std::fs::write(root.config().join("README.md"), CONFIG_README).unwrap(); // restore
                                                                                 // Comments survive a later set (toml_edit).
        std::fs::write(
            root.config().join("packages/watcher.toml"),
            "# keep me\naccounts = [\"alice\"]\n",
        )
        .unwrap();
        set_key(&root, "watcher", "interval", "30").unwrap();
        let raw = read_package(&root, "watcher").unwrap().unwrap();
        assert!(raw.contains("# keep me"));
        assert!(raw.contains("interval = 30"));
        // Unknown key reads as None, not an error.
        assert!(get_key(&root, "watcher", "nope").unwrap().is_none());
        assert!(get_key(&root, "ghost", "x").unwrap().is_none());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn rejects_bad_package_name() {
        let root = scratch("bad");
        init(&root).unwrap();
        assert!(set_key(&root, "../escape", "k", "1").is_err());
        assert!(set_key(&root, "a/b", "k", "1").is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn dotted_descent_through_scalar_errs_not_panics() {
        let root = scratch("descent");
        init(&root).unwrap();
        set_key(&root, "watcher", "poll", "30").unwrap(); // poll is a scalar
                                                          // Descending into the scalar must be a clean error, not a panic.
        assert!(set_key(&root, "watcher", "poll.interval", "5").is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn init_is_idempotent_on_reinit() {
        let root = scratch("reinit");
        init(&root).unwrap();
        let head1 = current_sha(&root).unwrap();
        init(&root).unwrap(); // second init on an existing root: clean no-op
        assert_eq!(
            head1,
            current_sha(&root).unwrap(),
            "re-init must not rewrite history"
        );
        assert!(git_ok(
            &root,
            &["rev-parse", "--verify", "--quiet", "refs/heads/live"]
        ));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // Simulate an agent working in its clone: raw git, the way the agent's shell
    // would, with its own (decoration-only) author identity.
    fn agent_git(clone: &std::path::Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(clone)
            .arg("-c")
            .arg("user.name=agent")
            .arg("-c")
            .arg("user.email=agent@local")
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn agent_proposal_roundtrip() {
        let root = scratch("prop");
        init(&root).unwrap();
        set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        // The agent gets a clone, branches, edits, and commits — never touching live.
        let clone = root.dir.join("agent-clone");
        clone_for_agent(&root, &clone).unwrap();
        assert!(agent_git(
            &clone,
            &["checkout", "-b", "proposal/x", "--quiet"]
        ));
        std::fs::write(
            clone.join("packages/watcher.toml"),
            "accounts = [\"alice\",\"bob\"]\n",
        )
        .unwrap();
        // A non-proposal branch and stray edits must NOT become proposals.
        assert!(agent_git(&clone, &["add", "-A"]));
        assert!(agent_git(
            &clone,
            &["commit", "-m", "propose bob", "--quiet"]
        ));

        let props = reap_proposals(&root, &clone, "scout").unwrap();
        assert_eq!(props.len(), 1, "exactly one proposal harvested");
        assert_eq!(props[0].by, "scout");
        assert_eq!(props[0].branch, "proposal/x");
        assert_eq!(props[0].files, vec!["packages/watcher.toml".to_string()]);

        // Listable + diffable; live is untouched (the proposal is held aside).
        assert_eq!(list_proposals(&root).unwrap().len(), 1);
        let diff = proposal_diff(&root, &props[0].id).unwrap();
        assert!(diff.contains("bob"), "diff shows the proposed change");
        assert_eq!(
            get_key(&root, "watcher", "accounts").unwrap().as_deref(),
            Some(r#"["alice"]"#),
            "live config must be unchanged by a mere proposal"
        );
        // A bad proposal id is refused (ref-injection guard).
        assert!(proposal_diff(&root, "../live").is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn accept_merges_into_live() {
        let root = scratch("accept");
        init(&root).unwrap();
        set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        let clone = root.dir.join("c");
        clone_for_agent(&root, &clone).unwrap();
        assert!(agent_git(
            &clone,
            &["checkout", "-b", "proposal/p", "--quiet"]
        ));
        std::fs::write(
            clone.join("packages/watcher.toml"),
            "accounts = [\"alice\",\"bob\"]\n",
        )
        .unwrap();
        assert!(agent_git(&clone, &["add", "-A"]));
        assert!(agent_git(&clone, &["commit", "-m", "x", "--quiet"]));
        let id = reap_proposals(&root, &clone, "scout").unwrap()[0]
            .id
            .clone();
        // Path-discipline passes; the only changed key is "accounts".
        assert_eq!(
            proposal_packages(&root, &id).unwrap(),
            vec!["watcher".to_string()]
        );
        assert_eq!(
            proposal_changed_keys(&root, &id, "watcher").unwrap(),
            vec!["accounts".to_string()]
        );
        // Accept merges into live and clears the pending ref.
        accept_proposal(&root, &id).unwrap();
        assert_eq!(
            get_key(&root, "watcher", "accounts").unwrap().as_deref(),
            Some(r#"["alice","bob"]"#)
        );
        assert!(
            list_proposals(&root).unwrap().is_empty(),
            "accepted proposal is no longer pending"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn path_escape_proposal_is_unacceptable() {
        let root = scratch("escape");
        init(&root).unwrap();
        let clone = root.dir.join("c");
        clone_for_agent(&root, &clone).unwrap();
        assert!(agent_git(
            &clone,
            &["checkout", "-b", "proposal/evil", "--quiet"]
        ));
        // A proposal that reaches outside the package-settings surface.
        std::fs::write(clone.join("evil.txt"), "x").unwrap();
        std::fs::write(clone.join(".gitattributes"), "* filter=evil\n").unwrap();
        assert!(agent_git(&clone, &["add", "-A"]));
        assert!(agent_git(&clone, &["commit", "-m", "escape", "--quiet"]));
        let id = reap_proposals(&root, &clone, "scout").unwrap()[0]
            .id
            .clone();
        // It is still harvested (visible) but can never be merged — by anyone.
        assert!(
            proposal_packages(&root, &id).is_err(),
            "path-discipline refuses it"
        );
        assert!(
            accept_proposal(&root, &id).is_err(),
            "accept refuses it before any merge"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn symlink_proposal_cannot_merge_or_leak() {
        let root = scratch("symlink");
        init(&root).unwrap();
        set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        // A secret OUTSIDE the config surface that the symlink would target.
        let secret = root.dir.join("secret.txt");
        std::fs::write(&secret, "TOPSECRET\n").unwrap();
        let clone = root.dir.join("c");
        clone_for_agent(&root, &clone).unwrap();
        assert!(agent_git(
            &clone,
            &["checkout", "-b", "proposal/x", "--quiet"]
        ));
        // Commit packages/leak.toml as a SYMLINK (git mode 120000) to the secret.
        std::os::unix::fs::symlink(&secret, clone.join("packages/leak.toml")).unwrap();
        assert!(agent_git(&clone, &["add", "-A"]));
        assert!(agent_git(&clone, &["commit", "-m", "leak", "--quiet"]));
        let id = reap_proposals(&root, &clone, "scout").unwrap()[0]
            .id
            .clone();
        // Path-discipline rejects the non-regular mode; accept refuses to merge.
        assert!(
            proposal_packages(&root, &id).is_err(),
            "symlink mode is rejected"
        );
        assert!(
            accept_proposal(&root, &id).is_err(),
            "a symlink proposal never merges"
        );
        // And even if a symlink reached live, read_package won't follow it.
        std::fs::create_dir_all(root.config_packages()).ok();
        let planted = root.config_packages().join("planted.toml");
        let _ = std::fs::remove_file(&planted);
        std::os::unix::fs::symlink(&secret, &planted).unwrap();
        assert!(
            read_package(&root, "planted").unwrap().is_none(),
            "read never follows a symlink"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn mixed_case_package_name_is_refused() {
        // History.toml would case-collide with the protected "history" on a
        // case-insensitive fs; the canonical-lowercase rule refuses it outright.
        assert!(valid_pkg("History").is_err());
        assert!(valid_pkg("WATCHER").is_err());
        assert!(valid_pkg("watcher").is_ok());
    }

    #[test]
    fn exec_bit_proposal_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("execbit");
        init(&root).unwrap();
        set_key(&root, "watcher", "accounts", r#"["alice"]"#).unwrap();
        let clone = root.dir.join("c");
        clone_for_agent(&root, &clone).unwrap();
        assert!(agent_git(
            &clone,
            &["checkout", "-b", "proposal/x", "--quiet"]
        ));
        let p = clone.join("packages/watcher.toml");
        std::fs::write(&p, "accounts = [\"x\"]\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(agent_git(&clone, &["add", "-A"]));
        assert!(agent_git(&clone, &["commit", "-m", "x", "--quiet"]));
        let id = reap_proposals(&root, &clone, "scout").unwrap()[0]
            .id
            .clone();
        // An executable blob (mode 100755) is not a regular config file → refused.
        assert!(
            proposal_packages(&root, &id).is_err(),
            "exec-bit (100755) is rejected"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn reap_does_not_follow_a_gitconfig_symlink() {
        let root = scratch("neutralize");
        init(&root).unwrap();
        let clone = root.dir.join("c");
        clone_for_agent(&root, &clone).unwrap();
        // A victim OUTSIDE the clone the hostile agent would target.
        let victim = root.dir.join("victim.txt");
        std::fs::write(&victim, "IMPORTANT\n").unwrap();
        // Hostile: make the clone's .git/config a symlink to the victim. The
        // neutralization must remove the link (not write through it).
        let cfg = clone.join(".git").join("config");
        std::fs::remove_file(&cfg).unwrap();
        std::os::unix::fs::symlink(&victim, &cfg).unwrap();
        let _ = reap_proposals(&root, &clone, "scout");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "IMPORTANT\n",
            "the victim file must NOT be overwritten through the symlink"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

//! The knowledge-base substrate (docs/handoffs/kb-core.md). A KB is a `kb/`
//! subfolder inside a package that carries the `[kb]` manifest marker (D1/D6) —
//! plain, greppable markdown, one topic per file, file+line anchors. There is no
//! `kb` table and no new topic plane: the kernel gains only the manifest marker
//! (manifest.rs `KbDecl`); everything else composes from substrates that already
//! ship (packages, the cage, git).
//!
//! This module owns two things: enumerating the enabled KBs (`elanus kb list`)
//! and the write path (D2) — a plain file write into a package's `kb/` tree
//! followed by one git commit using the shared hardened-git discipline
//! (`git_hardened`), the same untrusted-content hardening `config_repo` uses.
//! Authority is the agent's ordinary sandbox `fs_write` grant on that tree (the
//! cage, `sandbox.rs`, nothing custom); provenance is the git commit log plus the
//! ordinary obs trail — no provenance footers (D2).

use crate::git_hardened;
use crate::packages::{self, Package};
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};

/// The conventional subfolder that holds a package's knowledge base.
pub const KB_DIR: &str = "kb";

/// One enabled knowledge base, for `elanus kb list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KbInfo {
    /// The package name (also the topic/grant/sql key).
    pub package: String,
    /// The `[kb] title`, if declared.
    pub title: Option<String>,
    /// The `[kb] description`, if declared.
    pub description: Option<String>,
    /// The resolved `kb/` directory on disk.
    pub path: PathBuf,
    /// How many files the `kb/` tree carries (recursive).
    pub files: usize,
}

/// A package's `kb/` directory (whether or not it exists on disk yet).
pub fn kb_dir(pkg: &Package) -> PathBuf {
    pkg.dir.join(KB_DIR)
}

/// Every KB visible to `profile`: the discovered packages that carry the `[kb]`
/// marker (presence of the marker, not merely a `kb/` dir on disk — D1 wonky bit
/// 1). Name-sorted (discovery already sorts).
pub fn enumerate(root: &Root, profile: &str) -> Result<Vec<KbInfo>> {
    let mut out = Vec::new();
    for pkg in packages::discover_for_profile(root, profile)? {
        let Some(lm) = &pkg.manifest else { continue };
        let Some(kb) = &lm.manifest.kb else { continue };
        let dir = kb_dir(&pkg);
        out.push(KbInfo {
            package: pkg.name.clone(),
            title: kb.title.clone(),
            description: kb.description.clone(),
            files: count_files(&dir),
            path: dir,
        });
    }
    Ok(out)
}

/// Count regular files under a `kb/` tree, recursively. A missing dir is 0.
fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in entries.filter_map(|e| e.ok()) {
        // symlink_metadata: never follow a link (a KB tree holds agent-written
        // content, and a followed link could count/escape outside the tree).
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            n += count_files(&e.path());
        } else if ft.is_file() {
            n += 1;
        }
    }
    n
}

/// Result of a KB write: the package's tip commit and whether it changed.
pub struct WriteOutcome {
    pub package: String,
    pub rel: String,
    pub commit: String,
    pub changed: bool,
}

/// Write `content` to `kb/<rel>` inside `pkg`'s tree and commit exactly that path
/// on the package-dir git repo (initializing the repo on first write — the
/// package directory IS the git boundary, kb-core.md wonky bit 2). The write is a
/// plain file write: the cage is what actually gates it (an agent without the
/// `fs_write` grant on this tree is refused by seatbelt before this ever runs).
/// The commit uses the fixed kernel identity via the shared hardened-git helper;
/// who-wrote-what-when is the commit log plus the obs trail (D2, no footers).
pub fn write(root: &Root, pkg: &str, rel: &str, content: &str) -> Result<WriteOutcome> {
    let package = packages::find(root, pkg)
        .with_context(|| format!("resolving package {pkg:?} for a kb write"))?;
    let safe_rel = safe_kb_rel(rel)?;
    let kb = kb_dir(&package);
    let target = kb.join(&safe_rel);

    // Refuse to write THROUGH a symlink anywhere on the resolved chain: a KB tree
    // holds agent-written content, and a planted link could redirect the write
    // outside the package. Any existing ancestor that is a symlink is rejected.
    reject_symlink_chain(&kb, &safe_rel)?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(&target, content)?;

    ensure_repo(&package.dir)?;
    // The committed path is relative to the package-dir repo root: `kb/<rel>`.
    let rel_in_repo = format!("{KB_DIR}/{}", safe_rel.to_string_lossy());
    git_hardened::run_in(&package.dir, &["add", "--", &rel_in_repo], "kb add")?;
    // A no-op write stages nothing for the path — nothing to commit.
    if git_hardened::ok_in(
        &package.dir,
        &["diff", "--cached", "--quiet", "--", &rel_in_repo],
    ) {
        let commit = head_sha(&package.dir).unwrap_or_default();
        return Ok(WriteOutcome {
            package: package.name,
            rel: rel_in_repo,
            commit,
            changed: false,
        });
    }
    let msg = format!("kb: write {rel_in_repo}");
    git_hardened::run_in(
        &package.dir,
        &["commit", "-m", &msg, "--", &rel_in_repo],
        "kb commit",
    )?;
    let commit = head_sha(&package.dir)?;
    Ok(WriteOutcome {
        package: package.name,
        rel: rel_in_repo,
        commit,
        changed: true,
    })
}

/// Validate a KB-relative path: a normal relative path under `kb/`, no absolute
/// root, no `..`, no empty/`.` components. Returns the cleaned relative path,
/// which is ALWAYS relative to the `kb/` dir (not the package root). A leading
/// `kb/` is accepted and stripped, so both `role-verifier.md` and the
/// package-root form `kb/role-verifier.md` (the shape pointer `meta.path` uses)
/// resolve to the same file — no doubled `kb/kb/`.
fn safe_kb_rel(rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().trim_start_matches("./");
    let rel = rel
        .strip_prefix("kb/")
        .or_else(|| (rel == "kb").then_some(""))
        .unwrap_or(rel);
    if rel.is_empty() {
        bail!("kb path is empty");
    }
    let p = Path::new(rel);
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if s.is_empty() {
                    bail!("kb path {rel:?} has an empty segment");
                }
                clean.push(seg);
            }
            Component::CurDir => {}
            Component::ParentDir => bail!("kb path {rel:?} may not contain '..'"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("kb path {rel:?} must be relative to kb/")
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("kb path {rel:?} resolves to nothing");
    }
    Ok(clean)
}

/// Refuse if any existing ancestor of `kb/<rel>` (including `kb` itself) is a
/// symlink — a planted link could redirect the write outside the package tree.
fn reject_symlink_chain(kb: &Path, rel: &Path) -> Result<()> {
    if is_symlink(kb) {
        bail!("kb directory {} is a symlink — refusing to write", kb.display());
    }
    let mut cur = kb.to_path_buf();
    // Walk every intermediate directory of `rel` (not the final leaf file, which
    // may not exist yet, and which write_atomic replaces via rename).
    let comps: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    for seg in &comps[..comps.len().saturating_sub(1)] {
        cur = cur.join(seg);
        if is_symlink(&cur) {
            bail!("kb path component {} is a symlink — refusing", cur.display());
        }
    }
    // The leaf, if it already exists, must be a regular file we can replace.
    let leaf = kb.join(rel);
    if is_symlink(&leaf) {
        bail!("kb target {} is a symlink — refusing to overwrite", leaf.display());
    }
    Ok(())
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Ensure a git repo exists at the package dir (the kb git boundary). Idempotent:
/// a dir that already has `.git` is left untouched. Uses the shared hardened git.
fn ensure_repo(pkg_dir: &Path) -> Result<()> {
    if pkg_dir.join(".git").exists() {
        return Ok(());
    }
    git_hardened::run_in(pkg_dir, &["init", "-b", "main"], "kb repo init")?;
    Ok(())
}

fn head_sha(pkg_dir: &Path) -> Result<String> {
    git_hardened::run_in(pkg_dir, &["rev-parse", "HEAD"], "kb rev-parse")
}

/// Write a file atomically (sibling temp + rename), so a concurrent reader — a
/// grep or an indexer sweep — sees the whole old or whole new file, never a torn
/// one. Mirrors config_repo's write discipline.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("kbtmp");
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-kb-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    /// Install a minimal `[kb]` package under <root>/packages/<name> with one
    /// seed kb file, matching what `discover_for_profile("default")` walks.
    fn install_kb_pkg(root: &Root, name: &str, title: &str) {
        let pdir = root.packages().join(name);
        std::fs::create_dir_all(pdir.join("kb")).unwrap();
        std::fs::write(
            pdir.join("elanus.toml"),
            format!("[kb]\ntitle = \"{title}\"\n"),
        )
        .unwrap();
        std::fs::write(pdir.join("kb/seed.md"), "# seed\n").unwrap();
    }

    #[test]
    fn enumerate_keys_on_the_marker_not_the_dir() {
        let root = scratch("enum");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        // A package with a kb/ dir but NO [kb] marker must NOT be listed.
        let plain = root.packages().join("plain");
        std::fs::create_dir_all(plain.join("kb")).unwrap();
        std::fs::write(plain.join("elanus.toml"), "[request]\nsubscribe=[]\n").unwrap();
        std::fs::write(plain.join("kb/private.md"), "x").unwrap();

        let kbs = enumerate(&root, "default").unwrap();
        assert_eq!(kbs.len(), 1, "only the marked package is a KB");
        assert_eq!(kbs[0].package, "kb-demo");
        assert_eq!(kbs[0].title.as_deref(), Some("Demo KB"));
        assert_eq!(kbs[0].files, 1);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_commits_with_kernel_committer_and_reconstructs() {
        let root = scratch("write");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        let out = write(&root, "kb-demo", "notes/topic.md", "first\n").unwrap();
        assert!(out.changed);
        assert_eq!(out.rel, "kb/notes/topic.md");
        let pkg_dir = root.packages().join("kb-demo");
        assert!(pkg_dir.join("kb/notes/topic.md").exists());
        // The commit records the change under the fixed kernel committer.
        let log = git_hardened::run_in(
            &pkg_dir,
            &["log", "--format=%cn <%ce>%n%s", "--", "kb/notes/topic.md"],
            "log",
        )
        .unwrap();
        assert!(log.contains("elanus <elanus@localhost>"), "kernel committer: {log}");
        assert!(log.contains("kb: write kb/notes/topic.md"), "commit subject: {log}");

        // A second write is a new commit; the git log reconstructs who-what-when.
        let out2 = write(&root, "kb-demo", "notes/topic.md", "second\n").unwrap();
        assert!(out2.changed);
        assert_ne!(out.commit, out2.commit);
        // An identical re-write is a no-op — no new commit.
        let out3 = write(&root, "kb-demo", "notes/topic.md", "second\n").unwrap();
        assert!(!out3.changed, "idempotent write makes no commit");
        assert_eq!(out2.commit, out3.commit);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_accepts_optional_kb_prefix_without_doubling() {
        let root = scratch("prefix");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        // The package-root form (as pointer meta.path uses) must NOT double kb/.
        let a = write(&root, "kb-demo", "kb/role-verifier.md", "x\n").unwrap();
        assert_eq!(a.rel, "kb/role-verifier.md");
        // The kb-relative form resolves to the SAME file (idempotent second write).
        let b = write(&root, "kb-demo", "role-verifier.md", "x\n").unwrap();
        assert_eq!(b.rel, "kb/role-verifier.md");
        assert!(!b.changed, "same file, same content — no second commit");
        assert!(root
            .packages()
            .join("kb-demo/kb/role-verifier.md")
            .exists());
        assert!(!root.packages().join("kb-demo/kb/kb").exists(), "no kb/kb/");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_rejects_path_traversal() {
        let root = scratch("traverse");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        assert!(write(&root, "kb-demo", "../escape.md", "x").is_err());
        assert!(write(&root, "kb-demo", "/abs.md", "x").is_err());
        assert!(write(&root, "kb-demo", "a/../../b.md", "x").is_err());
        assert!(write(&root, "kb-demo", "", "x").is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    /// Recursively copy a directory tree (test helper — installs the shipped
    /// package onto a scratch root the way init materializes it).
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().filter_map(|e| e.ok()) {
            let src = e.path();
            let dst = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_tree(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    #[test]
    fn seeded_kb_llm_strengths_installs_and_lists() {
        // M2 acceptance: the shipped kb-llm-strengths package installs on a scratch
        // root and shows in `elanus kb list`; its role/model files exist and
        // cross-link; the verifier facts are grep-able with a file; the invariants
        // are encoded verbatim.
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("kits/stdlib/packages/kb-llm-strengths");
        assert!(shipped.join("elanus.toml").exists(), "package ships in stdlib");

        let root = scratch("seed");
        copy_tree(&shipped, &root.packages().join("kb-llm-strengths"));

        // Shows in the list with its title and file count.
        let kbs = enumerate(&root, "default").unwrap();
        let e = kbs
            .iter()
            .find(|k| k.package == "kb-llm-strengths")
            .expect("kb-llm-strengths is listed");
        assert_eq!(e.title.as_deref(), Some("LLM strengths"));
        assert!(e.files >= 8, "one file per model + one per role");

        // One file per model and one per role, all present.
        let kb = e.path.clone();
        for f in [
            "claude.md", "fable.md", "opus.md", "gpt-5.5.md", "glm-5.2.md",
            "role-planner.md", "role-implementer.md", "role-verifier.md",
        ] {
            assert!(kb.join(f).exists(), "kb/{f} exists");
        }

        // Cross-linking: a role file links a model file by relative path.
        let planner = std::fs::read_to_string(kb.join("role-planner.md")).unwrap();
        assert!(planner.contains("(claude.md)"), "role links model by rel path");
        assert!(planner.contains("(fable.md)"));
        let opus = std::fs::read_to_string(kb.join("opus.md")).unwrap();
        assert!(opus.contains("role-implementer.md") && opus.contains("role-verifier.md"));

        // The verifier facts are discoverable by grep ("who verifies") and encode
        // the invariant verbatim: verify = Opus/GPT-5.5 high, Fable for the hardest.
        let verifier = std::fs::read_to_string(kb.join("role-verifier.md")).unwrap();
        assert!(
            verifier.to_lowercase().contains("who verifies"),
            "grep -ri 'who verifies' finds the verifier facts"
        );
        assert!(verifier.contains("Opus on high"));
        assert!(verifier.contains("GPT-5.5 on high/xhigh"));
        assert!(verifier.contains("Fable for the hardest"));

        // Planning never flexes; plan = Claude/Fable only.
        assert!(planner.contains("Only Claude or Fable plan"));
        assert!(planner.to_lowercase().contains("never")
            && planner.contains("GPT-5.5"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn shipped_architect_pointer_meta_matches_the_kb_file() {
        // M3 acceptance: the seeded architect pointer block's meta resolves to a
        // real file + line range + a MATCHING sha (content-sha256 of the target
        // kb file). Staleness is acceptable (kb-core.md wonky bit 4), but the
        // shipped snapshot must be consistent at ship time so B5's checker starts
        // green — a wrong sha here is a real bug, not deferred drift.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let block = std::fs::read_to_string(
            manifest_dir.join("kits/core/profiles/architect/blocks/10-kb-llm-strengths.md"),
        )
        .unwrap();
        // Extract the JSON frontmatter (between the first two `---` lines).
        let front = block
            .strip_prefix("---\n")
            .and_then(|r| r.split("\n---\n").next())
            .expect("pointer block has JSON frontmatter");
        let meta: serde_json::Value = serde_json::from_str(front).unwrap();
        assert_eq!(meta["kb"], "kb-llm-strengths");
        let rel = meta["path"].as_str().unwrap();
        assert_eq!(rel, "kb/role-verifier.md");

        let target = manifest_dir
            .join("kits/stdlib/packages/kb-llm-strengths")
            .join(rel);
        let bytes = std::fs::read(&target).expect("the referenced kb file exists");
        let sha = crate::context_blocks::sha256_hex(&bytes);
        assert_eq!(
            meta["sha"].as_str().unwrap(),
            sha,
            "pointer sha must match the kb file's content-sha256"
        );
        // The line range is well-formed and within the file.
        let (lo, hi) = meta["lines"]
            .as_str()
            .unwrap()
            .split_once('-')
            .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
            .unwrap();
        let line_count = String::from_utf8_lossy(&bytes).lines().count();
        assert!(lo >= 1 && hi >= lo && hi <= line_count.max(1) + 1);
    }

    // M4 acceptance: the write is gated by the ordinary sandbox fs_write grant on
    // the package tree (the cage, nothing custom). A *copied* KB lives inside the
    // root (the agent's own writable world); a *linked* KB lives OUTSIDE the root,
    // so writing it needs an explicit fs_write grant — that is the gate. This test
    // exercises the linked case: without the grant the cage refuses the external
    // kb write; with it, the write succeeds. macOS-only (the enforcement
    // mechanism); a no-op elsewhere — the property under test is the cage's.
    #[test]
    #[cfg(target_os = "macos")]
    fn kb_write_is_gated_by_the_fs_write_grant() {
        use crate::profile::SandboxCfg;
        use crate::sandbox::Cage;
        if !std::path::Path::new("/usr/bin/sandbox-exec").exists() {
            return;
        }
        let root = scratch("cage");
        // canonicalize: seatbelt subpath rules match the real (inode) path.
        let root = Root {
            dir: root.dir.canonicalize().unwrap(),
        };
        std::fs::create_dir_all(root.packages()).unwrap();
        // A LINKED package's kb tree living OUTSIDE the harness root — and outside
        // the temp write-holes seatbelt always allows, so under $HOME (the same
        // "definitely outside" trick seatbelt_actually_cages uses).
        let home = std::env::var("HOME").unwrap();
        let linked_root = std::path::Path::new(&home)
            .join(format!("el-kb-linked-{}", std::process::id()));
        let external_kb = linked_root.join("kb");
        std::fs::create_dir_all(&external_kb).unwrap();
        let external_kb = external_kb.canonicalize().unwrap();

        // NOT GRANTED: an enforcing cage whose only extra write root is an in-root
        // scratch dir — the external kb tree is outside every write root, so the
        // cage refuses the write. (A non-empty fs_write is what turns enforcement
        // on; naming an unrelated in-root path keeps the external tree ungranted.)
        let in_root_scratch = root.dir.join("scratch");
        std::fs::create_dir_all(&in_root_scratch).unwrap();
        let ungranted = Cage::from_profile(
            &root,
            &SandboxCfg {
                fs_write: vec![in_root_scratch.display().to_string()],
                ..Default::default()
            },
        );
        assert!(ungranted.enforcing());
        let denied = ungranted
            .shell_command(&format!("echo sneak > {}", external_kb.join("x.md").display()))
            .output()
            .unwrap();
        assert!(
            !denied.status.success(),
            "an external (linked) kb write without the grant must be refused by the cage"
        );

        // GRANTED: fs_write names the linked kb tree — the write now succeeds.
        let granted = Cage::from_profile(
            &root,
            &SandboxCfg {
                fs_write: vec![external_kb.display().to_string()],
                ..Default::default()
            },
        );
        assert!(granted.enforcing());
        let ok = granted
            .shell_command(&format!("echo knowledge > {}", external_kb.join("x.md").display()))
            .output()
            .unwrap();
        assert!(ok.status.success(), "granted external kb write must succeed: {ok:?}");

        std::fs::remove_dir_all(&linked_root).ok();
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn configured_remote_is_pushable() {
        let root = scratch("remote");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        write(&root, "kb-demo", "topic.md", "content\n").unwrap();
        let pkg_dir = root.packages().join("kb-demo");
        // A bare remote (the remote-backup property, D2): the KB repo pushes.
        let remote = root.dir.join("backup.git");
        git_hardened::run_in(
            &remote.parent().unwrap().to_path_buf(),
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
            "init bare",
        )
        .unwrap();
        git_hardened::run_in(
            &pkg_dir,
            &["remote", "add", "backup", remote.to_str().unwrap()],
            "remote add",
        )
        .unwrap();
        let pushed = git_hardened::run_in(&pkg_dir, &["push", "backup", "main"], "push");
        assert!(pushed.is_ok(), "kb repo must push to a configured remote: {pushed:?}");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

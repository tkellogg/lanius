//! Kits — starter packs of packages + profiles (kits/README.md).
//!
//! Two install modes. LINK (the default): the kit's packages/ dir is
//! appended to the default profile's `package_path`, so the packages stay
//! where the kit lives — shared dirs can be managed in one place, and a
//! copy into the root's packages/ shadows the link (first-hit-wins,
//! fork-to-customize). COPY (--copy): packages are vendored into the
//! root's packages/, the original behavior. Profiles are always copied
//! if missing — identity is meant to be edited, packages to be shared.
//!
//! Either way the grants ledger is the authority: kit packages are synced
//! and decided with provenance `kit:<name>`, and the manifest+code hash
//! pin means an upstream edit to a *linked* package re-enters review in
//! every root that links it. There is no runtime kit entity — "what did
//! kit X install" is a provenance query, not a registry lookup.

use crate::packages;
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Link,
    Copy,
}

/// Resolve a kit reference to its directory. A value containing '/' is a
/// path used directly. A bare name resolves against $ELANUS_KIT_PATH
/// (colon-separated directories), then against a `kits/` directory found by
/// walking up from the executable's location — dev convenience so a repo
/// build sees <repo>/kits; packaged installs should set ELANUS_KIT_PATH.
pub fn resolve(kit: &str) -> Result<PathBuf> {
    if kit.contains('/') {
        let p = PathBuf::from(kit);
        if p.is_dir() {
            return Ok(p.canonicalize()?);
        }
        bail!("kit path {kit:?} is not a directory");
    }
    let mut tried: Vec<String> = Vec::new();
    for dir in search_dirs() {
        let p = dir.join(kit);
        if p.is_dir() {
            return Ok(p.canonicalize()?);
        }
        tried.push(p.display().to_string());
    }
    bail!(
        "kit {kit:?} not found (tried: {}); set ELANUS_KIT_PATH or pass a path",
        if tried.is_empty() { "nothing — no ELANUS_KIT_PATH".into() } else { tried.join(", ") }
    )
}

/// The directories kits resolve against, in order: $ELANUS_KIT_PATH entries,
/// then every `kits/` dir found walking up from the executable.
fn search_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(kp) = std::env::var("ELANUS_KIT_PATH") {
        for entry in kp.split(':').filter(|s| !s.is_empty()) {
            out.push(PathBuf::from(entry));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors().skip(1) {
            let p = anc.join("kits");
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out
}

/// Install a kit into the root. Returns the kit's README contents (for the
/// caller to print — the kit's direction) if it has one. `grant = false` is
/// the STAGING path (HANDOFF phase 5): files land and requests register,
/// but every grant stays pending review — the commit gesture is a separate
/// `elanus approve`, which is what lets an un-trusted surface (the web UI,
/// an agent) compose installs without authority.
pub fn install(
    root: &Root,
    conn: &rusqlite::Connection,
    kit_dir: &Path,
    mode: Mode,
    grant: bool,
) -> Result<Option<String>> {
    let name = kit_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "kit".into());
    let by = format!("kit:{name}");

    let mut names: Vec<String> = Vec::new();
    // Canonical from the start: discovery returns canonicalized dirs (and
    // macOS aliases /var -> /private/var), so the shadow check below must
    // compare like with like.
    let mut pkgs = kit_dir.join("packages");
    if pkgs.is_dir() {
        pkgs = pkgs.canonicalize()?;
        match mode {
            Mode::Copy => {
                for e in sorted_dirs(&pkgs)? {
                    let pkg = e.file_name().unwrap().to_string_lossy().to_string();
                    copy_tree_if_missing(&e, &root.packages().join(&pkg))?;
                    names.push(pkg);
                }
            }
            Mode::Link => {
                link_package_path(root, &pkgs)?;
                for e in sorted_dirs(&pkgs)? {
                    names.push(e.file_name().unwrap().to_string_lossy().to_string());
                }
            }
        }
    }
    let profs = kit_dir.join("profiles");
    if profs.is_dir() {
        // Profiles are always copied-if-missing, never linked or clobbered:
        // a profile is the human's to edit, so the root owns its copy.
        for e in sorted_dirs(&profs)? {
            let pname = e.file_name().unwrap().to_string_lossy().to_string();
            copy_tree_if_missing(&e, &root.profile_dir(&pname))?;
        }
    }
    if !names.is_empty() {
        packages::sync(root, conn)?;
        for pkg in &names {
            // A linked package can be shadowed by an earlier path entry (a
            // local copy, a prior kit). Granting would then approve the
            // SHADOWING code under this kit's name — skip loudly instead.
            if mode == Mode::Link {
                let found = packages::find(root, pkg)?;
                if !found.dir.starts_with(&pkgs) {
                    eprintln!(
                        "[kit] {pkg}: shadowed by {} — linked copy is inert, no grant decided",
                        found.dir.display()
                    );
                    continue;
                }
            }
            if grant {
                packages::decide(root, conn, pkg, true, &by)?;
            } else {
                println!("staged {pkg} (grants pending — `elanus approve {pkg}` to commit)");
            }
        }
    }
    let readme = kit_dir.join("README.md");
    if readme.is_file() {
        return Ok(Some(std::fs::read_to_string(readme)?));
    }
    Ok(None)
}

/// Append a kit's packages dir to the default profile's `package_path`,
/// preserving comments and any existing entries. "packages" stays first so
/// a local copy always shadows the link (fork-to-customize).
fn link_package_path(root: &Root, pkgs_dir: &Path) -> Result<()> {
    let entry = pkgs_dir.canonicalize()?.display().to_string();
    let pdir = root.profile_dir("default");
    std::fs::create_dir_all(&pdir)?;
    let f = pdir.join("profile.toml");
    let raw = std::fs::read_to_string(&f).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut =
        raw.parse().with_context(|| format!("parsing {}", f.display()))?;
    if doc.get("package_path").is_none() {
        // The serde default is ["packages"]; making it explicit before
        // appending keeps the link from accidentally erasing local discovery.
        let mut arr = toml_edit::Array::new();
        arr.push("packages");
        doc.insert("package_path", toml_edit::value(arr));
    }
    let arr = doc["package_path"]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("package_path in {} is not an array", f.display()))?;
    if arr.iter().any(|v| v.as_str() == Some(entry.as_str())) {
        return Ok(()); // already linked
    }
    arr.push(entry.as_str());
    std::fs::write(&f, doc.to_string())?;
    Ok(())
}

/// Kits installable right now: (name, dir, first README line), resolution
/// order, first hit per name wins — same shadowing rule as resolve().
pub fn list() -> Result<Vec<(String, PathBuf, String)>> {
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if !p.is_dir() {
                continue;
            }
            // A kit is a dir with packages/, profiles/, or a README.
            if !(p.join("packages").is_dir()
                || p.join("profiles").is_dir()
                || p.join("README.md").is_file())
            {
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if out.iter().any(|(n, _, _)| *n == name) {
                continue; // shadowed by an earlier search dir
            }
            let hook = std::fs::read_to_string(p.join("README.md"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .map(|l| l.trim().trim_start_matches('#').trim().to_string())
                        .find(|l| !l.is_empty())
                })
                .unwrap_or_default();
            out.push((name, p, hook));
        }
    }
    Ok(out)
}

/// The kit's README, without installing.
pub fn show(kit: &str) -> Result<String> {
    let dir = resolve(kit)?;
    let readme = dir.join("README.md");
    if !readme.is_file() {
        bail!("kit {} has no README.md", dir.display());
    }
    Ok(std::fs::read_to_string(readme)?)
}

fn sorted_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    Ok(out)
}

/// Recursive copy that never overwrites: each file lands only if absent
/// (same contract as write_if_missing for the stock templates). fs::copy
/// preserves the exec bit, so kit hook scripts stay executable.
fn copy_tree_if_missing(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.filter_map(|e| e.ok()) {
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_tree_if_missing(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn scratch(tag: &str) -> (Root, PathBuf) {
        let base = std::env::temp_dir().join(format!("el-kit-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let root = Root { dir: base.join("root") };
        std::fs::create_dir_all(root.dir.join("packages")).unwrap();
        std::fs::create_dir_all(root.dir.join("profiles/default")).unwrap();
        // A kit with one package and one profile.
        let kit = base.join("mykit");
        std::fs::create_dir_all(kit.join("packages/kpkg/scripts")).unwrap();
        std::fs::write(
            kit.join("packages/kpkg/elanus.toml"),
            "[request]\nsubscribe=[\"in/package/kpkg/go\"]\n[process]\nmode=\"exec\"\nrun=\"scripts/main\"\n",
        )
        .unwrap();
        std::fs::write(kit.join("packages/kpkg/scripts/main"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::create_dir_all(kit.join("profiles/kprof")).unwrap();
        std::fs::write(kit.join("profiles/kprof/profile.toml"), "agent = \"k\"\n").unwrap();
        std::fs::write(kit.join("README.md"), "# my kit\n\ndirections\n").unwrap();
        (root, kit)
    }

    #[test]
    fn link_install_grants_with_kit_provenance() {
        let (root, kit) = scratch("link");
        std::fs::write(
            root.profile_dir("default").join("profile.toml"),
            "# a comment that must survive\nagent = \"main\"\n",
        )
        .unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let readme = install(&root, &conn, &kit, Mode::Link, true).unwrap();
        assert!(readme.unwrap().contains("directions"));
        // Not copied — discovered through the linked path.
        assert!(!root.packages().join("kpkg").exists());
        let found = packages::find(&root, "kpkg").unwrap();
        assert!(found.dir.starts_with(kit.join("packages").canonicalize().unwrap()));
        assert!(packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/go").unwrap());
        let by: String = conn
            .query_row(
                "SELECT decided_by FROM grants WHERE package='kpkg' AND state='approved' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(by, "kit:mykit");
        // Profile copied; default profile edit kept the comment and packages-first order.
        assert!(root.profile_dir("kprof").join("profile.toml").exists());
        let p = std::fs::read_to_string(root.profile_dir("default").join("profile.toml")).unwrap();
        assert!(p.contains("a comment that must survive"));
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert_eq!(prof.package_path[0], "packages");
        // Idempotent: re-link adds no second entry.
        install(&root, &conn, &kit, Mode::Link, true).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert_eq!(prof.package_path.len(), 2);
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn copy_install_vendors() {
        let (root, kit) = scratch("copy");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        install(&root, &conn, &kit, Mode::Copy, true).unwrap();
        assert!(root.packages().join("kpkg/elanus.toml").exists());
        assert!(packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/go").unwrap());
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert_eq!(prof.package_path, vec!["packages".to_string()]);
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn shadowed_link_is_not_granted() {
        let (root, kit) = scratch("shadow");
        // A local package with the same name exists BEFORE the link.
        let local = root.packages().join("kpkg");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("elanus.toml"), "[request]\nsubscribe=[\"in/package/kpkg/local\"]\n").unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        install(&root, &conn, &kit, Mode::Link, true).unwrap();
        // The local one shadows; the kit must not have approved it.
        assert!(!packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/local").unwrap());
        assert!(!packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/go").unwrap());
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }
}

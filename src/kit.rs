//! Kits — starter packs of packages + profiles (kits/README.md).
//!
//! Two install modes. LINK (the default): the kit's packages/ dir is
//! appended to the default profile's `elanus_path`, so the packages stay
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
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Link,
    Copy,
}

/// Resolve a kit reference to its directory. A value containing '/' is a
/// path used directly; a bare name resolves against search_dirs().
pub fn resolve(root: &Root, kit: &str) -> Result<PathBuf> {
    if kit.contains('/') {
        let p = PathBuf::from(kit);
        if p.is_dir() {
            return Ok(p.canonicalize()?);
        }
        bail!("kit path {kit:?} is not a directory");
    }
    let mut tried: Vec<String> = Vec::new();
    for dir in search_dirs(root) {
        let p = dir.join(kit);
        if p.is_dir() {
            return Ok(p.canonicalize()?);
        }
        tried.push(p.display().to_string());
    }
    bail!(
        "kit {kit:?} not found (tried: {}); drop it in <root>/kits or pass a path",
        if tried.is_empty() {
            "nothing".into()
        } else {
            tried.join(", ")
        }
    )
}

/// The directories kits resolve against, in order:
/// 1. $ELANUS_KIT_PATH entries (an override, not the mechanism);
/// 2. `<root>/kits` — THE configured home: init seeds the stock kits here,
///    and dropping a directory in is the whole install story;
/// 3. `~/.elanus/kits` — user-level, shared across roots;
/// 4. every `kits/` dir walking up from the executable (dev convenience so
///    a repo build sees <repo>/kits).
fn search_dirs(root: &Root) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(kp) = std::env::var("ELANUS_KIT_PATH") {
        for entry in kp.split(':').filter(|s| !s.is_empty()) {
            out.push(PathBuf::from(entry));
        }
    }
    out.push(root.dir.join("kits"));
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".elanus/kits"));
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
    crate::config_repo::init(root)?;

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
                link_elanus_path(root, kit_dir)?;
                let (sha, changed) = crate::config_repo::commit_agent(
                    root,
                    "default",
                    "config: update default agent package path",
                )?;
                if changed {
                    emit_agent_change(root, conn, "default", &sha, &by)?;
                }
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
            valid_profile_name(&pname)?;
            let dst = root.profile_dir(&pname);
            let existed = dst.join("profile.toml").exists();
            copy_tree_if_missing(&e, &dst)?;
            if !existed {
                let (sha, changed) = crate::config_repo::commit_agent(
                    root,
                    &pname,
                    "config: add kit agent profile",
                )?;
                if changed {
                    emit_agent_change(root, conn, &pname, &sha, &by)?;
                }
            }
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
            // Skill-only packages (no elanus.toml) carry no requests —
            // nothing to decide, nothing to stage; their SKILL.md is inert
            // content gated by profile visibility alone.
            if packages::find(root, pkg)?.manifest.is_none() {
                continue;
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

fn emit_agent_change(
    root: &Root,
    conn: &Connection,
    name: &str,
    sha: &str,
    by: &str,
) -> Result<()> {
    crate::events::emit(
        root,
        conn,
        crate::events::EmitOpts {
            payload: Some(json!({
                "agent": name,
                "commit": sha,
                "decided_by": by,
            })),
            sender: Some(by.to_string()),
            ..crate::events::EmitOpts::new("obs/config/changed")
        },
    )?;
    Ok(())
}

fn valid_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("bad kit profile name {name:?} (alphanumeric, dash, underscore)");
    }
    Ok(())
}

/// Append a kit dir to the default profile's `elanus_path`,
/// preserving comments and any existing entries. "packages" stays first so
/// a local copy always shadows the link (fork-to-customize).
fn link_elanus_path(root: &Root, kit_dir: &Path) -> Result<()> {
    let entry = kit_dir.canonicalize()?.display().to_string();
    let pdir = root.profile_dir("default");
    std::fs::create_dir_all(&pdir)?;
    let f = pdir.join("profile.toml");
    let raw = std::fs::read_to_string(&f).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parsing {}", f.display()))?;
    if doc.get("elanus_path").is_none() {
        if let Some(old) = doc.remove("package_path") {
            doc.insert("elanus_path", old);
        }
    } else {
        doc.remove("package_path");
    }
    if doc.get("elanus_path").is_none() {
        // The serde default is ["packages"]; making it explicit before
        // appending keeps the link from accidentally erasing local discovery.
        let mut arr = toml_edit::Array::new();
        arr.push("packages");
        doc.insert("elanus_path", toml_edit::value(arr));
    }
    let arr = doc["elanus_path"]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("elanus_path in {} is not an array", f.display()))?;
    if arr.iter().any(|v| v.as_str() == Some(entry.as_str())) {
        return Ok(()); // already linked
    }
    arr.push(entry.as_str());
    std::fs::write(&f, doc.to_string())?;
    Ok(())
}

/// Kits installable right now: (name, dir, first README line), resolution
/// order, first hit per name wins — same shadowing rule as resolve().
pub fn list(root: &Root) -> Result<Vec<(String, PathBuf, String)>> {
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
    for dir in search_dirs(root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
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

/// A kit's optional `kit.toml` `protected` flag (absent = false). A protected
/// kit is installed and auto-approved at init, and its packages refuse to be
/// revoked without `--force` (docs/config.md, "Stdlib"). Metadata only — the
/// grants ledger is still the authority for what is actually approved.
fn kit_is_protected(kit_dir: &Path) -> bool {
    std::fs::read_to_string(kit_dir.join("kit.toml"))
        .ok()
        .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|d| d.get("protected").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// The set of package names that belong to a protected kit, by directory-name
/// membership — independent of how (or whether) the package was installed into
/// this root, so the guard holds even for a package vendored or seeded loose.
/// Resolution order mirrors list(); a protected kit anywhere on the path
/// protects its packages. Best-effort: unreadable dirs are skipped.
pub fn protected_packages(root: &Root) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for dir in search_dirs(root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let kdir = e.path();
            if !kdir.is_dir() || !kit_is_protected(&kdir) {
                continue;
            }
            if let Ok(pkgs) = std::fs::read_dir(kdir.join("packages")) {
                for p in pkgs.filter_map(|p| p.ok()).filter(|p| p.path().is_dir()) {
                    out.insert(p.file_name().to_string_lossy().to_string());
                }
            }
        }
    }
    out
}

/// Refuse to unlink a protected kit without `--force`: unlinking silently
/// drops its packages off the path, the same off-switch `elanus revoke`
/// already guards at the package level (docs/config.md, "Stdlib").
pub fn guard_unlink_protected(kit_dir: &Path, name: &str, force: bool) -> Result<()> {
    if !force && kit_is_protected(kit_dir) {
        bail!(
            "⚠ {name} is a protected stdlib kit — the product depends on it \
             (docs/config.md). Unlinking it breaks things (e.g. the web UI's \
             transcripts). Re-run with --force if you really mean it."
        );
    }
    Ok(())
}

/// Remove a kit dir from the default profile's elanus_path.
/// Grants are NOT revoked here — they're inert without discovery, and
/// revocation is its own gesture (`elanus revoke <pkg>`); we say so.
pub fn unlink(root: &Root, kit_dir: &Path) -> Result<()> {
    let kit_entry = kit_dir
        .canonicalize()
        .unwrap_or_else(|_| kit_dir.to_path_buf());
    let package_entry = kit_entry.join("packages");
    let entries = [
        kit_entry.display().to_string(),
        package_entry.display().to_string(),
    ];
    let f = root.profile_dir("default").join("profile.toml");
    let raw = std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
    let mut doc: toml_edit::DocumentMut = raw.parse()?;
    if doc.get("elanus_path").is_none() {
        if let Some(old) = doc.remove("package_path") {
            doc.insert("elanus_path", old);
        }
    } else {
        doc.remove("package_path");
    }
    let Some(arr) = doc.get_mut("elanus_path").and_then(|i| i.as_array_mut()) else {
        bail!("nothing linked (no elanus_path in the default profile)");
    };
    let before = arr.len();
    arr.retain(|v| {
        let Some(s) = v.as_str() else { return true };
        !entries.iter().any(|entry| entry == s)
    });
    if arr.len() == before {
        bail!("{} is not on the elanus path", entries[0]);
    }
    std::fs::write(&f, doc.to_string())?;
    println!("unlinked {}", entries[0]);
    println!(
        "grants for its packages remain in the ledger (inert without discovery); `elanus revoke <pkg>` to retire them"
    );
    Ok(())
}

/// The kit's README, without installing.
pub fn show(root: &Root, kit: &str) -> Result<String> {
    let dir = resolve(root, kit)?;
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
        let root = Root {
            dir: base.join("root"),
        };
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
        std::fs::write(
            kit.join("packages/kpkg/scripts/main"),
            "#!/bin/sh\necho hi\n",
        )
        .unwrap();
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
            "# a comment that must survive\nagent = \"main\"\npackage_path = [\"packages\"]\n",
        )
        .unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let readme = install(&root, &conn, &kit, Mode::Link, true).unwrap();
        assert!(readme.unwrap().contains("directions"));
        // Not copied — discovered through the linked path.
        assert!(!root.packages().join("kpkg").exists());
        let found = packages::find(&root, "kpkg").unwrap();
        assert!(found
            .dir
            .starts_with(kit.join("packages").canonicalize().unwrap()));
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
        assert!(p.contains("elanus_path"));
        assert!(!p.contains("package_path"));
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert_eq!(prof.elanus_path[0], "packages");
        assert_eq!(
            prof.elanus_path[1],
            kit.canonicalize().unwrap().display().to_string()
        );
        // Idempotent: re-link adds no second entry.
        install(&root, &conn, &kit, Mode::Link, true).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert_eq!(prof.elanus_path.len(), 2);
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
        assert_eq!(prof.elanus_path, vec!["packages".to_string()]);
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn shadowed_link_is_not_granted() {
        let (root, kit) = scratch("shadow");
        // A local package with the same name exists BEFORE the link.
        let local = root.packages().join("kpkg");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(
            local.join("elanus.toml"),
            "[request]\nsubscribe=[\"in/package/kpkg/local\"]\n",
        )
        .unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        install(&root, &conn, &kit, Mode::Link, true).unwrap();
        // The local one shadows; the kit must not have approved it.
        assert!(
            !packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/local").unwrap()
        );
        assert!(!packages::is_approved(&conn, "kpkg", "subscribe", "in/package/kpkg/go").unwrap());
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn protected_packages_tracks_kit_toml() {
        let (root, _kit) = scratch("protected");
        let kits = root.dir.join("kits");
        // A protected kit and an ordinary one, both under <root>/kits.
        std::fs::create_dir_all(kits.join("guarded/packages/lockpkg")).unwrap();
        std::fs::write(kits.join("guarded/kit.toml"), "protected = true\n").unwrap();
        std::fs::create_dir_all(kits.join("loose/packages/freepkg")).unwrap();
        // loose has no kit.toml → not protected.
        let prot = protected_packages(&root);
        assert!(
            prot.contains("lockpkg"),
            "a package in a protected kit must be protected"
        );
        assert!(
            !prot.contains("freepkg"),
            "a package in an unmarked kit must not be"
        );
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn unlink_honors_protected_gate() {
        let (root, kit) = scratch("unlink-protected");
        std::fs::write(kit.join("kit.toml"), "protected = true\n").unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        install(&root, &conn, &kit, Mode::Link, true).unwrap();

        // Without --force: refused, loud, and still linked.
        let err = guard_unlink_protected(&kit, "mykit", false).unwrap_err();
        assert!(
            err.to_string().contains("protected"),
            "refusal must name the reason: {err}"
        );
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert!(prof
            .elanus_path
            .iter()
            .any(|p| p == &kit.canonicalize().unwrap().display().to_string()));

        // With --force: guard steps aside, unlink proceeds.
        guard_unlink_protected(&kit, "mykit", true).unwrap();
        unlink(&root, &kit).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert!(!prof
            .elanus_path
            .iter()
            .any(|p| p == &kit.canonicalize().unwrap().display().to_string()));
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }

    #[test]
    fn unlink_ordinary_kit_unchanged() {
        let (root, kit) = scratch("unlink-ordinary");
        // No kit.toml at all — not protected.
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        install(&root, &conn, &kit, Mode::Link, true).unwrap();

        // No --force needed for an ordinary kit.
        guard_unlink_protected(&kit, "mykit", false).unwrap();
        unlink(&root, &kit).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        assert!(!prof
            .elanus_path
            .iter()
            .any(|p| p == &kit.canonicalize().unwrap().display().to_string()));
        std::fs::remove_dir_all(root.dir.parent().unwrap()).ok();
    }
}

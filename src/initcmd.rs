use crate::db;
use crate::manifest::ThrottleDecl;
use crate::packages;
use crate::paths::Root;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

struct PkgFile {
    rel: &'static str,
    content: &'static str,
    exec: bool,
}

/// Packages shipped with the binary. Real source of truth is the repo's
/// packages/ dir; init materializes copies into the harness root so the root
/// is self-contained and the user can edit/fork them freely.
const PKG_FILES: &[PkgFile] = &[
    PkgFile { rel: "chat/elanus.toml", content: include_str!("../packages/chat/elanus.toml"), exec: false },
    PkgFile { rel: "chat/scripts/run", content: include_str!("../packages/chat/scripts/run"), exec: true },
    PkgFile { rel: "echo/elanus.toml", content: include_str!("../packages/echo/elanus.toml"), exec: false },
    PkgFile { rel: "echo/scripts/echo", content: include_str!("../packages/echo/scripts/echo"), exec: true },
    PkgFile { rel: "notify/elanus.toml", content: include_str!("../packages/notify/elanus.toml"), exec: false },
    PkgFile { rel: "notify/scripts/notify", content: include_str!("../packages/notify/scripts/notify"), exec: true },
    PkgFile { rel: "watchdog/elanus.toml", content: include_str!("../packages/watchdog/elanus.toml"), exec: false },
    PkgFile { rel: "watchdog/scripts/scan", content: include_str!("../packages/watchdog/scripts/scan"), exec: true },
    PkgFile { rel: "notes/SKILL.md", content: include_str!("../packages/notes/SKILL.md"), exec: false },
];

const PROFILE_TOML: &str = include_str!("../templates/profile.toml");
const RECORDER_TOML: &str = include_str!("../templates/recorder.toml");
const BUS_TOML: &str = include_str!("../templates/bus.toml");
const BLOCK_SYSTEM: &str = include_str!("../templates/block-00-system.md");
const BLOCK_CONTEXT: &str = include_str!("../templates/block-10-context.md");

pub fn init(dir: PathBuf, kits: Vec<String>) -> Result<()> {
    // Resolve every kit BEFORE touching disk: a typo'd kit name must not
    // leave a half-initialized root behind.
    let kit_dirs = kits.iter().map(|k| resolve_kit(k)).collect::<Result<Vec<_>>>()?;
    std::fs::create_dir_all(&dir)?;
    let root = Root { dir: dir.canonicalize()? };
    for d in [root.packages(), root.run_dir(), root.profile_dir("default").join("blocks")] {
        std::fs::create_dir_all(d)?;
    }

    write_if_missing(&root.recorder_file(), RECORDER_TOML, false)?;
    write_if_missing(&root.bus_file(), BUS_TOML, false)?;
    write_if_missing(&root.profile_dir("default").join("profile.toml"), PROFILE_TOML, false)?;
    write_if_missing(&root.profile_dir("default").join("blocks/00-system.md"), BLOCK_SYSTEM, false)?;
    write_if_missing(&root.profile_dir("default").join("blocks/10-context.md"), BLOCK_CONTEXT, false)?;

    for f in PKG_FILES {
        let path = root.packages().join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }

    let conn = db::open(&root)?;
    db::init_schema(&conn)?;
    // The algedonic class: never coalesced, never queued behind other work.
    packages::upsert_throttle(
        &conn,
        "signal/#",
        &ThrottleDecl { coalesce: Some(false), ..Default::default() },
    )?;
    if !root.trace_file().exists() {
        std::fs::write(root.trace_file(), "")?;
    }

    // Stock packages ship with the binary and init is a human gesture, so
    // their requests are approved here with that provenance. Anything that
    // lands on the package path later asks like everything else.
    packages::sync(&root, &conn)?;
    for name in ["chat", "echo", "notify", "watchdog"] {
        packages::decide(&root, &conn, name, true, "init")?;
    }

    // Kits: starter packs. Their packages are copied in and granted exactly
    // the way the stock packages above are — init IS the human install
    // gesture, so kit provenance is the same "init" the stock set carries.
    let mut readmes: Vec<(String, String)> = Vec::new();
    for (name, kit_dir) in kits.iter().zip(&kit_dirs) {
        if let Some(readme) = install_kit(&root, &conn, kit_dir)? {
            readmes.push((name.clone(), readme));
        }
        println!("installed kit {name} from {}", kit_dir.display());
    }

    println!();
    println!("initialized harness root at {}", root.dir.display());
    println!();
    println!("next steps:");
    // The default root needs no env var; only point at HARNESS_ROOT when
    // this root actually requires it.
    let is_default = crate::paths::default_root()
        .ok()
        .and_then(|d| d.canonicalize().ok())
        .map(|d| d == root.dir)
        .unwrap_or(false);
    if !is_default {
        println!("  export HARNESS_ROOT={}", root.dir.display());
    }
    println!("  elanus daemon &                     # the dispatcher");
    println!("  elanus exec --session hi \"hello\"    # chat (needs ANTHROPIC_API_KEY)");
    println!("  elanus emit in/agent/main --payload '{{\"prompt\":\"check in with me\"}}'");
    println!("  elanus inbox / elanus answer <id> \"...\"");
    println!("  elanus packages                     # what's installed, what's pending");
    println!("  elanus bus sub 'obs/#'              # watch the live stream");
    println!("  tail -f {}", root.trace_file().display());
    for (name, readme) in &readmes {
        println!();
        println!("── kit {name} ─────────────────────────────────────────");
        println!("{}", readme.trim_end());
    }
    Ok(())
}

/// Resolve a kit reference to its directory. A value containing '/' is a
/// path used directly. A bare name resolves against $ELANUS_KIT_PATH
/// (colon-separated directories), then against a `kits/` directory found by
/// walking up from the executable's location — dev convenience so a repo
/// build sees <repo>/kits; packaged installs should set ELANUS_KIT_PATH.
fn resolve_kit(kit: &str) -> Result<PathBuf> {
    if kit.contains('/') {
        let p = PathBuf::from(kit);
        if p.is_dir() {
            return Ok(p.canonicalize()?);
        }
        bail!("kit path {kit:?} is not a directory");
    }
    let mut tried: Vec<String> = Vec::new();
    if let Ok(kp) = std::env::var("ELANUS_KIT_PATH") {
        for entry in kp.split(':').filter(|s| !s.is_empty()) {
            let p = Path::new(entry).join(kit);
            if p.is_dir() {
                return Ok(p.canonicalize()?);
            }
            tried.push(p.display().to_string());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors().skip(1) {
            let p = anc.join("kits").join(kit);
            if p.is_dir() {
                return Ok(p.canonicalize()?);
            }
        }
        tried.push(format!("kits/{kit} up from {}", exe.display()));
    }
    bail!(
        "kit {kit:?} not found (tried: {}); set ELANUS_KIT_PATH or pass a path",
        if tried.is_empty() { "nothing — no ELANUS_KIT_PATH".into() } else { tried.join(", ") }
    )
}

/// A kit is a directory: `packages/` (copied into the root's packages/ and
/// granted like the stock set), optional `profiles/<name>/...` (files copied
/// if missing — an existing profile is never clobbered), optional README.md
/// returned for printing.
fn install_kit(root: &Root, conn: &rusqlite::Connection, kit_dir: &Path) -> Result<Option<String>> {
    let mut names: Vec<String> = Vec::new();
    let pkgs = kit_dir.join("packages");
    if pkgs.is_dir() {
        for e in sorted_dirs(&pkgs)? {
            let name = e.file_name().unwrap().to_string_lossy().to_string();
            copy_tree_if_missing(&e, &root.packages().join(&name))?;
            names.push(name);
        }
    }
    let profs = kit_dir.join("profiles");
    if profs.is_dir() {
        for e in sorted_dirs(&profs)? {
            let name = e.file_name().unwrap().to_string_lossy().to_string();
            copy_tree_if_missing(&e, &root.profile_dir(&name))?;
        }
    }
    if !names.is_empty() {
        packages::sync(root, conn)?;
        for name in &names {
            packages::decide(root, conn, name, true, "init")?;
        }
    }
    let readme = kit_dir.join("README.md");
    if readme.is_file() {
        return Ok(Some(std::fs::read_to_string(readme)?));
    }
    Ok(None)
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

fn write_if_missing(path: &Path, content: &str, exec: bool) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, content)?;
    }
    if exec {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

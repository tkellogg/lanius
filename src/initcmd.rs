use crate::db;
use crate::manifest::ThrottleDecl;
use crate::packages;
use crate::paths::Root;
use anyhow::Result;
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

pub fn init(dir: PathBuf) -> Result<()> {
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

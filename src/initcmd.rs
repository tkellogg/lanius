use crate::db;
use crate::manifest::ThrottleDecl;
use crate::paths::Root;
use crate::skills;
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
    PkgFile { rel: "chat/harness.toml", content: include_str!("../packages/chat/harness.toml"), exec: false },
    PkgFile { rel: "chat/scripts/run", content: include_str!("../packages/chat/scripts/run"), exec: true },
    PkgFile { rel: "echo/harness.toml", content: include_str!("../packages/echo/harness.toml"), exec: false },
    PkgFile { rel: "echo/scripts/echo", content: include_str!("../packages/echo/scripts/echo"), exec: true },
    PkgFile { rel: "notify/harness.toml", content: include_str!("../packages/notify/harness.toml"), exec: false },
    PkgFile { rel: "notify/scripts/notify", content: include_str!("../packages/notify/scripts/notify"), exec: true },
    PkgFile { rel: "watchdog/harness.toml", content: include_str!("../packages/watchdog/harness.toml"), exec: false },
    PkgFile { rel: "watchdog/scripts/scan", content: include_str!("../packages/watchdog/scripts/scan"), exec: true },
    PkgFile { rel: "notes/SKILL.md", content: include_str!("../packages/notes/SKILL.md"), exec: false },
];

const PROFILE_TOML: &str = include_str!("../templates/profile.toml");
const BLOCK_SYSTEM: &str = include_str!("../templates/block-00-system.md");
const BLOCK_CONTEXT: &str = include_str!("../templates/block-10-context.md");

pub fn init(dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let root = Root { dir: dir.canonicalize()? };
    for d in [root.skills(), root.handlers(), root.run_dir(), root.profile_dir("default").join("blocks")] {
        std::fs::create_dir_all(d)?;
    }

    write_if_missing(&root.profile_dir("default").join("profile.toml"), PROFILE_TOML, false)?;
    write_if_missing(&root.profile_dir("default").join("blocks/00-system.md"), BLOCK_SYSTEM, false)?;
    write_if_missing(&root.profile_dir("default").join("blocks/10-context.md"), BLOCK_CONTEXT, false)?;

    for f in PKG_FILES {
        let path = root.skills().join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }

    let conn = db::open(&root)?;
    db::init_schema(&conn)?;
    // The algedonic class: never coalesced, never queued behind other work.
    skills::upsert_throttle(
        &conn,
        "signal.*",
        &ThrottleDecl { coalesce: Some(false), ..Default::default() },
    )?;
    if !root.trace_file().exists() {
        std::fs::write(root.trace_file(), "")?;
    }

    for name in ["chat", "echo", "notify", "watchdog"] {
        skills::enable(&root, &conn, name)?;
    }

    println!();
    println!("initialized harness root at {}", root.dir.display());
    println!();
    println!("next steps:");
    println!("  export HARNESS_ROOT={}", root.dir.display());
    println!("  harness daemon &                     # the dispatcher");
    println!("  harness exec --session hi \"hello\"    # chat (needs ANTHROPIC_API_KEY)");
    println!("  harness emit agent.exec --payload '{{\"prompt\":\"check in with me\"}}'");
    println!("  harness inbox / harness answer <id> \"...\"");
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

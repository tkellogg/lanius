use crate::db;
use crate::kit;
use crate::manifest::ThrottleDecl;
use crate::packages;
use crate::paths::Root;
use anyhow::{Context, Result};
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
    PkgFile {
        rel: "chat/elanus.toml",
        content: include_str!("../packages/chat/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "chat/scripts/run",
        content: include_str!("../packages/chat/scripts/run"),
        exec: true,
    },
    PkgFile {
        rel: "echo/elanus.toml",
        content: include_str!("../packages/echo/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "echo/scripts/echo",
        content: include_str!("../packages/echo/scripts/echo"),
        exec: true,
    },
    PkgFile {
        rel: "notify/elanus.toml",
        content: include_str!("../packages/notify/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "notify/scripts/notify",
        content: include_str!("../packages/notify/scripts/notify"),
        exec: true,
    },
    PkgFile {
        rel: "watchdog/elanus.toml",
        content: include_str!("../packages/watchdog/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "watchdog/scripts/scan",
        content: include_str!("../packages/watchdog/scripts/scan"),
        exec: true,
    },
    PkgFile {
        rel: "notes/SKILL.md",
        content: include_str!("../packages/notes/SKILL.md"),
        exec: false,
    },
    // Ships pending, NOT auto-approved below: an approved stage shapes every
    // prompt; activating it is the human's call (elanus approve recent-history).
    PkgFile {
        rel: "recent-history/elanus.toml",
        content: include_str!("../packages/recent-history/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "recent-history/scripts/main",
        content: include_str!("../packages/recent-history/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "window/elanus.toml",
        content: include_str!("../packages/window/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "window/scripts/stage",
        content: include_str!("../packages/window/scripts/stage"),
        exec: true,
    },
];

struct StockHarnessPackage {
    dir: &'static str,
    binary: &'static str,
    manifest: &'static str,
}

const STOCK_HARNESS_PACKAGES: &[StockHarnessPackage] = &[
    StockHarnessPackage {
        dir: "harness-claude",
        binary: "harness-claude",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"claude\"\n",
            "aliases = [\"cc\"]\n",
            "agent_noun = \"claude-code\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
    StockHarnessPackage {
        dir: "harness-codex",
        binary: "harness-codex",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"codex\"\n",
            "agent_noun = \"codex\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
    StockHarnessPackage {
        dir: "harness-opencode",
        binary: "harness-opencode",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"opencode\"\n",
            "agent_noun = \"opencode\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
];

/// ALL stock kits, seeded into <root>/kits so every root has the
/// out-of-the-box set resolvable — no env var, no repo checkout. The kit
/// dir is the config; these are just its defaults (write_if_missing, so
/// edits and deletions stick).
const STOCK_KIT_FILES: &[PkgFile] = &[
    PkgFile {
        rel: "core/README.md",
        content: include_str!("../kits/core/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/harness-doctrine/SKILL.md",
        content: include_str!("../kits/core/packages/harness-doctrine/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/self-modify/SKILL.md",
        content: include_str!("../kits/core/packages/self-modify/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/escalate/SKILL.md",
        content: include_str!("../kits/core/packages/escalate/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/sibling-coordination/SKILL.md",
        content: include_str!("../kits/core/packages/sibling-coordination/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/sibling-coordination/elanus.toml",
        content: include_str!("../kits/core/packages/sibling-coordination/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/profiles/architect/profile.toml",
        content: include_str!("../kits/core/profiles/architect/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/profiles/architect/blocks/00-architect.md",
        content: include_str!("../kits/core/profiles/architect/blocks/00-architect.md"),
        exec: false,
    },
    PkgFile {
        rel: "dev/README.md",
        content: include_str!("../kits/dev/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "dev/packages/git-protect/elanus.toml",
        content: include_str!("../kits/dev/packages/git-protect/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "dev/packages/git-protect/scripts/gate",
        content: include_str!("../kits/dev/packages/git-protect/scripts/gate"),
        exec: true,
    },
    PkgFile {
        rel: "dev/profiles/dev/profile.toml",
        content: include_str!("../kits/dev/profiles/dev/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/README.md",
        content: include_str!("../kits/funnel/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-intake/elanus.toml",
        content: include_str!("../kits/funnel/packages/funnel-intake/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-intake/scripts/main",
        content: include_str!("../kits/funnel/packages/funnel-intake/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/elanus.toml",
        content: include_str!("../kits/funnel/packages/funnel-sift/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/rules.txt",
        content: include_str!("../kits/funnel/packages/funnel-sift/rules.txt"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/scripts/sift",
        content: include_str!("../kits/funnel/packages/funnel-sift/scripts/sift"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/packages/funnel-scout/elanus.toml",
        content: include_str!("../kits/funnel/packages/funnel-scout/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-scout/scripts/run",
        content: include_str!("../kits/funnel/packages/funnel-scout/scripts/run"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/profiles/scout/profile.toml",
        content: include_str!("../kits/funnel/profiles/scout/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/profiles/scout/blocks/00-scout.md",
        content: include_str!("../kits/funnel/profiles/scout/blocks/00-scout.md"),
        exec: false,
    },
    // stdlib: the protected, always-on kit (docs/config.md). Installed and
    // auto-approved unconditionally in init(); history (the transcript view) is
    // its first member, so the web UI's sessions tab always has something to read.
    PkgFile {
        rel: "stdlib/README.md",
        content: include_str!("../kits/stdlib/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/kit.toml",
        content: include_str!("../kits/stdlib/kit.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/history/elanus.toml",
        content: include_str!("../kits/stdlib/packages/history/elanus.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/history/scripts/main",
        content: include_str!("../kits/stdlib/packages/history/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/history/SKILL.md",
        content: include_str!("../kits/stdlib/packages/history/SKILL.md"),
        exec: false,
    },
];

const PROFILE_TOML: &str = include_str!("../templates/profile.toml");
const RECORDER_TOML: &str = include_str!("../templates/recorder.toml");
const BUS_TOML: &str = include_str!("../templates/bus.toml");
const BLOCK_SYSTEM: &str = include_str!("../templates/block-00-system.md");
const BLOCK_CONTEXT: &str = include_str!("../templates/block-10-context.md");

pub fn init(dir: PathBuf, kits: Vec<String>, copy_kits: bool) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let root = Root {
        dir: dir.canonicalize()?,
    };
    for d in [
        root.packages(),
        root.run_dir(),
        root.profile_dir("default").join("blocks"),
        root.secrets(),
    ] {
        std::fs::create_dir_all(d)?;
    }
    // The secret store is the kernel's; keep it 0700 so even outside the cage
    // it is not casually readable. The cage fences it from actors regardless.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(root.secrets(), std::fs::Permissions::from_mode(0o700));
    }
    // Mint the human and kernel credentials now, so they exist right after
    // init — the human's surfaces can read them before any daemon is up, and
    // the daemon's own ensure() at startup is then idempotent.
    crate::secrets::ensure(&root)?;
    // The configuration repository (docs/config.md): a kernel-owned git repo
    // whose `live` branch holds package config. Created here so every root has
    // it from the start; the cage fences it from agents (sandbox.rs Protect).
    crate::config_repo::init(&root).context("initializing the config repo")?;
    // Seed <root>/kits with the stock kits FIRST so `init --kit core` (and
    // every later `kit add`) resolves without env vars or a repo checkout.
    for f in STOCK_KIT_FILES {
        let path = root.dir.join("kits").join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }
    // Resolve every kit BEFORE installing anything: a typo'd kit name must
    // not leave a half-installed root behind.
    let kit_dirs = kits
        .iter()
        .map(|k| kit::resolve(&root, k))
        .collect::<Result<Vec<_>>>()?;

    write_if_missing(&root.recorder_file(), RECORDER_TOML, false)?;
    write_if_missing(&root.bus_file(), BUS_TOML, false)?;
    write_if_missing(
        &root.profile_dir("default").join("profile.toml"),
        PROFILE_TOML,
        false,
    )?;
    write_if_missing(
        &root.profile_dir("default").join("blocks/00-system.md"),
        BLOCK_SYSTEM,
        false,
    )?;
    write_if_missing(
        &root.profile_dir("default").join("blocks/10-context.md"),
        BLOCK_CONTEXT,
        false,
    )?;
    let _ =
        crate::config_repo::commit_agent(&root, "default", "config: seed default agent profile");

    for f in PKG_FILES {
        let path = root.packages().join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }

    // Stock harness packages seed the built-in coding agents as discoverable
    // packages without changing dispatch yet.
    seed_stock_harness_packages(&root)?;

    let conn = db::open(&root)?;
    db::init_schema(&conn)?;
    // The algedonic class: never coalesced, never queued behind other work.
    packages::upsert_throttle(
        &conn,
        "signal/#",
        &ThrottleDecl {
            coalesce: Some(false),
            ..Default::default()
        },
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

    // Kits: starter packs (src/kit.rs). Linked by default — the kit dir
    // stays the source and a local copy shadows it; --copy vendors. Either
    // way init is the human install gesture, provenance kit:<name>.
    let mode = if copy_kits {
        kit::Mode::Copy
    } else {
        kit::Mode::Link
    };
    // Stdlib is installed and auto-approved in EVERY root (docs/config.md): the
    // protected packages the product itself depends on — history's transcript
    // view first, so the sessions tab is never a dead 503. Linked, not vendored
    // (it stays kernel-managed); its packages are revoke-guarded
    // (kit::protected_packages + `elanus revoke`).
    let stdlib_dir = kit::resolve(&root, "stdlib").context("resolving the stdlib kit")?;
    kit::install(&root, &conn, &stdlib_dir, kit::Mode::Link, true).context("installing stdlib")?;
    let mut readmes: Vec<(String, String)> = Vec::new();
    for (name, kit_dir) in kits.iter().zip(&kit_dirs) {
        if let Some(readme) = kit::install(&root, &conn, kit_dir, mode, true)? {
            readmes.push((name.clone(), readme));
        }
        println!("installed kit {name} from {}", kit_dir.display());
    }

    println!();
    println!("initialized harness root at {}", root.dir.display());
    println!();
    println!(
        "you are \"{}\" here (the default identity). to use your own name:",
        crate::secrets::owner_name(&root)
    );
    println!("  elanus profile set default owner=<yourname>   # then restart the daemon");
    println!();
    println!("next steps:");
    // The default root needs no env var; only point at $ELANUS_ROOT when
    // this root actually requires it.
    let is_default = crate::paths::default_root()
        .ok()
        .and_then(|d| d.canonicalize().ok())
        .map(|d| d == root.dir)
        .unwrap_or(false);
    if !is_default {
        println!("  export ELANUS_ROOT={}", root.dir.display());
    }
    println!("  elanus daemon &                     # the dispatcher");
    println!("  elanus exec --session hi \"hello\"    # chat (needs ANTHROPIC_API_KEY)");
    println!("  elanus emit in/agent/main --payload '{{\"prompt\":\"check in with me\"}}'");
    println!("  elanus inbox / elanus answer <id> \"...\"");
    println!("  elanus packages                     # what's installed, what's pending");
    println!("  elanus approve history              # transcripts in the web UI (granted serving)");
    println!(
        "  elanus approve recent-history       # cross-run memory of recent mail (a context stage)"
    );
    println!("  elanus bus sub 'obs/#'              # watch the live stream");
    println!("  tail -f {}", root.trace_file().display());
    for (name, readme) in &readmes {
        println!();
        println!("── kit {name} ─────────────────────────────────────────");
        println!("{}", readme.trim_end());
    }
    Ok(())
}

fn write_if_missing(path: &Path, content: &str, exec: bool) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, content)?;
    }
    if exec {
        set_executable(path)?;
    }
    Ok(())
}

fn seed_stock_harness_packages(root: &Root) -> Result<()> {
    let exe = std::env::current_exe().context("locating the running elanus binary")?;
    let exe_dir = exe
        .parent()
        .context("running elanus binary has no parent directory")?;

    for pkg in STOCK_HARNESS_PACKAGES {
        let pkg_dir = root.packages().join(pkg.dir);
        let bin_dir = pkg_dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        write_if_missing(&pkg_dir.join("elanus.toml"), pkg.manifest, false)?;

        let adapter = bin_dir.join("adapter");
        let source = exe_dir.join(format!("{}{}", pkg.binary, std::env::consts::EXE_SUFFIX));
        if source.is_file() {
            if !adapter.exists() {
                std::fs::copy(&source, &adapter).with_context(|| {
                    format!("copying {} -> {}", source.display(), adapter.display())
                })?;
            }
            set_executable(&adapter)?;
        } else if !adapter.exists() {
            eprintln!(
                "[init] warning: missing stock harness binary {}; seeded {} without bin/adapter",
                source.display(),
                pkg_dir.display()
            );
        }
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

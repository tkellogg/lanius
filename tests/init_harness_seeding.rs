use anyhow::{Context, Result};
use elanus::manifest::Manifest;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn init_seeds_stock_harness_packages() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new("cargo")
        .args([
            "build",
            "--bin",
            "elanus",
            "--bin",
            "harness-claude",
            "--bin",
            "harness-codex",
            "--bin",
            "harness-opencode",
        ])
        .current_dir(&repo)
        .output()
        .context("running cargo build for elanus and the stock harness binaries")?;
    assert!(
        build.status.success(),
        "cargo build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let target_debug = target_debug_dir()?;
    let elanus_bin = target_debug.join(format!("elanus{}", std::env::consts::EXE_SUFFIX));

    let root_dir = unique_temp_dir("elanus-init-root")?;
    let workdir = unique_temp_dir("elanus-init-work")?;

    let output = Command::new(&elanus_bin)
        .arg("init")
        .env("ELANUS_ROOT", &root_dir)
        .current_dir(&workdir)
        .output()
        .with_context(|| format!("running {}", elanus_bin.display()))?;
    assert!(
        output.status.success(),
        "elanus init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for (pkg_dir, expected_name, expected_agent_noun, expected_aliases) in [
        ("harness-claude", "claude", "claude-code", &["cc"][..]),
        ("harness-codex", "codex", "codex", &[][..]),
        ("harness-opencode", "opencode", "opencode", &[][..]),
    ] {
        let pkg = root_dir.join("packages").join(pkg_dir);
        let manifest_path = pkg.join("elanus.toml");
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&raw)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        assert_eq!(
            manifest.harness.len(),
            1,
            "expected a single harness declaration in {}",
            manifest_path.display()
        );
        let harness = &manifest.harness[0];
        assert_eq!(harness.name, expected_name);
        assert_eq!(harness.agent_noun, expected_agent_noun);
        assert_eq!(harness.run, "bin/adapter");
        let expected_aliases = expected_aliases
            .iter()
            .map(|alias| (*alias).to_string())
            .collect::<Vec<_>>();
        assert_eq!(harness.aliases, expected_aliases);

        let adapter = pkg.join("bin/adapter");
        assert!(adapter.exists(), "missing adapter binary {}", adapter.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&adapter)
                .with_context(|| format!("reading metadata for {}", adapter.display()))?
                .permissions()
                .mode();
            assert!(
                mode & 0o111 != 0,
                "adapter binary is not executable: {}",
                adapter.display()
            );
        }
    }

    // The KB arc must survive init (docs/handoffs/kb-search.md + kb-groundskeeper.md
    // follow-up: pin the KB packages into the init-seeding regression). kb-search is
    // stdlib — auto-installed on EVERY root — so it must be both seeded into the
    // stdlib kit AND discoverable via `elanus kb`/`packages`; kb-llm-strengths (the
    // corpus) rides the same stdlib install. kb-groundskeeper is a core package, so
    // plain init seeds it into <root>/kits/core (installable via `init --kit core`)
    // rather than <root>/packages.
    for rel in [
        "kits/stdlib/packages/kb-search/elanus.toml",
        "kits/stdlib/packages/kb-search/scripts/index",
        "kits/stdlib/packages/kb-search/scripts/search",
        // discovery — the privileged capability search (docs/handoffs/kb-discovery.md):
        // stdlib, so a fresh root gets the find_capability tool + the taught block.
        "kits/stdlib/packages/discovery/elanus.toml",
        "kits/stdlib/packages/discovery/scripts/find",
        "kits/stdlib/packages/discovery/SKILL.md",
        // the seeded high-awareness block that TEACHES find_capability (M2/journey-14).
        "profiles/default/blocks/20-discovery.md",
        "kits/stdlib/packages/kb-llm-strengths/elanus.toml",
        "kits/stdlib/packages/kb-llm-strengths/kb/role-verifier.md",
        "kits/core/packages/kb-groundskeeper/elanus.toml",
        "kits/core/packages/kb-groundskeeper/scripts/dispatch",
        "kits/core/packages/kb-groundskeeper/SKILL.md",
        "kits/core/packages/kb-pipeline/elanus.toml",
        "kits/core/packages/kb-pipeline/scripts/run",
        "kits/core/profiles/kb-compactor/profile.toml",
        "kits/core/profiles/kb-ratifier/profile.toml",
    ] {
        let p = root_dir.join(rel);
        assert!(p.exists(), "init must seed {rel}, missing at {}", p.display());
    }

    // The stdlib KB is not just on disk — it is auto-installed, so `elanus kb list`
    // names kb-llm-strengths on a fresh root (the concrete regression the kb-search
    // verifier flagged: init round 1 failed to seed/install the package).
    let kb_list = Command::new(&elanus_bin)
        .args(["kb", "list", "--json"])
        .env("ELANUS_ROOT", &root_dir)
        .current_dir(&workdir)
        .output()
        .context("running elanus kb list")?;
    assert!(
        kb_list.status.success(),
        "elanus kb list failed\nstderr:\n{}",
        String::from_utf8_lossy(&kb_list.stderr)
    );
    let listed = String::from_utf8_lossy(&kb_list.stdout);
    assert!(
        listed.contains("kb-llm-strengths"),
        "kb-llm-strengths must be auto-installed + listed after init: {listed}"
    );

    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&workdir);
    Ok(())
}

fn target_debug_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir).join("debug"));
    }
    let mut dir = std::env::current_exe().context("resolving current test executable")?;
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    Ok(dir)
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

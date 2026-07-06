//! End-to-end acceptance for the discovery arc (docs/handoffs/kb-discovery.md).
//!
//! Drives the built `elanus` binary against a real init'd root:
//!   - M1: `elanus discover <query>` names an available-but-disabled package,
//!     what enabling it would add, and the enable path; a capability already
//!     visible to the caller is NOT re-surfaced; `--json` is machine-stable.
//!   - M2: the stdlib `discovery` package's `find_capability` tool grant is
//!     auto-approved into existence (the M0 "tool" grant gate), the tool script
//!     is a thin wrapper over `elanus discover --json` that reshapes the report,
//!     and the seeded high-awareness block teaches the tool on the default
//!     profile (`elanus render`).
//!   - M3: the enable guidance rides the existing config-proposal flow — no new
//!     enable mechanism is invented (asserted on the guidance text; the proposal
//!     machinery itself is exercised by the config-repo tests).

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    elanus: PathBuf,
    root: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// Build the binary, init a root, and plant a `discord`-shaped package in the
    /// universe plus a `worker` profile whose path (an empty dir) cannot see it.
    fn setup() -> Result<Fixture> {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let build = Command::new("cargo")
            .args(["build", "--bin", "elanus"])
            .current_dir(&repo)
            .output()
            .context("cargo build --bin elanus")?;
        assert!(
            build.status.success(),
            "cargo build failed\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let elanus = target_debug_dir()?.join(format!("elanus{}", std::env::consts::EXE_SUFFIX));

        let root = unique_temp_dir("elanus-disc-root")?;
        let work = unique_temp_dir("elanus-disc-work")?;
        let init = Command::new(&elanus)
            .arg("init")
            .env("ELANUS_ROOT", &root)
            .current_dir(&work)
            .output()
            .context("elanus init")?;
        assert!(
            init.status.success(),
            "elanus init failed\n{}",
            String::from_utf8_lossy(&init.stderr)
        );

        // A discord package in the instance universe (<root>/packages) — a kb file,
        // a skill, a tool — but off the worker profile's path.
        let d = root.join("packages/discord");
        std::fs::create_dir_all(d.join("kb"))?;
        std::fs::create_dir_all(d.join("scripts"))?;
        std::fs::write(
            d.join("elanus.toml"),
            "[kb]\ntitle = \"Discord API notes\"\n\n\
             [[tool]]\nname = \"send_discord\"\ndescription = \"post to a channel\"\nrun = \"scripts/send\"\n",
        )?;
        std::fs::write(
            d.join("kb/discord-api-notes.md"),
            "# Discord API\nRate limits, gateway intents, webhook posting.\n",
        )?;
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: discord\ndescription: talk to Discord — channels, webhooks, the gateway\n---\n# discord\n",
        )?;
        std::fs::write(d.join("scripts/send"), "#!/bin/sh\ncat\n")?;

        // The worker profile: its path is an empty dir, so discord is in the
        // universe but NOT on its path (elanus_path is read from profile.toml).
        std::fs::create_dir_all(root.join("empty"))?;
        std::fs::create_dir_all(root.join("profiles/worker"))?;
        std::fs::write(
            root.join("profiles/worker/profile.toml"),
            "agent = \"worker\"\nowner = \"owner\"\nelanus_path = [\"empty\"]\n\n\
             [model]\nmodel = \"claude-sonnet-4-6\"\n\n[skills]\ninclude = [\"#\"]\n",
        )?;

        Ok(Fixture { elanus, root, work })
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.elanus)
            .args(args)
            .env("ELANUS_ROOT", &self.root)
            .current_dir(&self.work)
            .output()
            .expect("running elanus")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

#[test]
fn m1_discover_cli_surfaces_a_missing_package_and_spares_a_visible_one() -> Result<()> {
    let fx = Fixture::setup()?;

    // The worker lacks discord → it surfaces, naming what enabling adds + the path.
    let out = fx.run(&["discover", "--profile", "worker", "discord api"]);
    assert!(
        out.status.success(),
        "discover failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("discord"), "discord package named: {text}");
    assert!(
        text.contains("send_discord"),
        "the tool enabling adds is named: {text}"
    );
    assert!(
        text.contains("discord-api-notes.md"),
        "the kb file enabling adds is named: {text}"
    );
    assert!(
        text.contains("config-proposal"),
        "the enable path rides config-proposal: {text}"
    );

    // The default profile CAN see discord → it is not "missing".
    let out = fx.run(&["discover", "--profile", "default", "discord api"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("discord (not enabled)"),
        "a capability visible to the caller must not be re-surfaced: {text}"
    );

    // --json is machine-stable and shape-correct.
    let out = fx.run(&["discover", "--profile", "worker", "--json", "discord api"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "parsing discover --json: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;
    assert_eq!(v["profile"], "worker");
    let m = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["package"] == "discord")
        .expect("discord in the json matches");
    assert_eq!(m["enabled"], false);
    assert!(m["adds"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "send_discord"));
    Ok(())
}

#[test]
fn m2_find_capability_tool_is_approved_taught_and_wraps_the_cli() -> Result<()> {
    let fx = Fixture::setup()?;

    // The stdlib discovery package's "tool" grant is auto-approved at init — the
    // M0 grant gate is satisfied, so find_capability can fold into an agent array.
    let out = fx.run(&["packages", "--json", "--profile", "default"]);
    let listed = String::from_utf8_lossy(&out.stdout);
    let approved = listed
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|o| {
            o["name"] == "discovery"
                && o["grants"].as_array().map_or(false, |gs| {
                    gs.iter().any(|g| {
                        g["kind"] == "tool"
                            && g["value"] == "find_capability"
                            && g["state"] == "approved"
                    })
                })
        });
    assert!(
        approved,
        "discovery's find_capability tool grant is approved at init: {listed}"
    );

    // The seeded high-awareness block teaches the tool on the default profile.
    let out = fx.run(&["render", "--profile", "default"]);
    let ctx = String::from_utf8_lossy(&out.stdout);
    assert!(
        ctx.contains("find_capability"),
        "the default profile's rendered context teaches find_capability: {ctx}"
    );

    // The tool script is a thin wrapper: stdin {query} → `elanus discover --json`
    // (as the calling profile) → reshaped {query, found:[...]}. Drive it the way
    // the [[tool]] seam dispatches (args JSON on stdin, ELANUS_PROFILE in env).
    let find = fx.root.join("kits/stdlib/packages/discovery/scripts/find");
    assert!(
        find.exists(),
        "the linked stdlib find script exists at {}",
        find.display()
    );
    let mut child = Command::new("python3")
        .arg(&find)
        .env("ELANUS_ROOT", &fx.root)
        .env("ELANUS_PROFILE", "worker")
        .env(
            "PATH",
            format!(
                "{}:{}",
                target_debug_dir()?.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .current_dir(&fx.work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawning the find tool script")?;
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"query":"discord api"}"#)?;
    let out = child.wait_with_output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "parsing find output: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })?;
    assert_eq!(v["query"], "discord api");
    let found = v["found"].as_array().expect("found array");
    let discord = found
        .iter()
        .find(|f| f["package"] == "discord")
        .expect("discord in found");
    assert!(discord["adds"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "send_discord"));
    assert!(discord["enable"]
        .as_str()
        .unwrap()
        .contains("config-proposal"));
    Ok(())
}

fn target_debug_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir).join("debug"));
    }
    let mut dir = std::env::current_exe().context("resolving the test executable")?;
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

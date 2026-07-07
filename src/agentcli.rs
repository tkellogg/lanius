//! `lanius agent` - front door for native/profile agents plus launch discovery.

use crate::db;
use crate::events::{self, EmitOpts};
use crate::exec::{self, ExecOpts};
use crate::packages;
use crate::paths::Root;
use crate::profile;
use crate::provider;
use crate::topic;
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub struct CatalogOpts {
    pub json: bool,
}

pub struct RunOpts {
    pub profile: String,
    pub prompt: String,
    pub session: Option<String>,
    pub with_packages: Vec<String>,
    pub provider: Option<String>,
}

pub struct SpawnOpts {
    pub profile: String,
    pub prompt: String,
    pub session: Option<String>,
    pub priority: i64,
    pub with_packages: Vec<String>,
    pub provider: Option<String>,
}

/// The shared spawn request (docs/handoffs/agent-launching.md M3): both
/// `lanius agent spawn` and the native `launch_agent` tool build one of these
/// and hand it to `spawn_core`, so the CLI door and the tool door cannot drift.
pub struct SpawnRequest {
    pub profile: String,
    pub prompt: String,
    pub session: Option<String>,
    pub priority: i64,
    pub with_packages: Vec<String>,
    pub provider: Option<String>,
    /// Provenance for the mailbox event's `sender` — the launching agent for the
    /// `launch_agent` tool, else the ambient actor. `created_by` in the ledger.
    pub created_by: Option<String>,
    /// Optional per-run model override, rides the payload (docs/handoffs/kb-groundskeeper.md
    /// M3): the groundskeeper pipeline threads the config-chosen cheap/expensive
    /// model into the compactor/ratifier spawn. `None` (the default) leaves the
    /// spawn byte-identical — the profile's own `[model]` decides.
    pub model: Option<String>,
    /// Optional per-pass token budget, rides the payload (docs/handoffs/kb-groundskeeper.md
    /// M3): the groundskeeper threads its configured budget so a pass is bounded.
    /// `None` omits it entirely (byte-identical default).
    pub budget: Option<i64>,
}

/// `lanius agent catalog` - one inventory surface for launchable things.
pub fn catalog(root: &Root, opts: CatalogOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    packages::sync(root, &conn)?;
    let profiles = profile_rows(root, &conn)?;
    let providers = provider::list(&conn).unwrap_or_default();
    let coding_tools = crate::codeagent::tools();

    if opts.json {
        println!(
            "{}",
            json!({
                "coding_tools": coding_tools,
                "profiles": profiles,
                "providers": providers,
            })
        );
        return Ok(());
    }

    println!("coding tools:");
    for tool in coding_tools {
        println!("  {tool}");
    }
    println!();
    println!("native profiles:");
    for p in &profiles {
        let status = if p["daemon_drivable"].as_bool() == Some(true) {
            "spawn-ready"
        } else {
            "run-only"
        };
        println!(
            "  {:<16} agent={:<16} model={:<28} {}",
            p["profile"].as_str().unwrap_or("?"),
            p["agent"].as_str().unwrap_or("?"),
            p["model"].as_str().unwrap_or("?"),
            status
        );
        if p["daemon_drivable"].as_bool() != Some(true) {
            if let Some(reason) = p["daemon_reason"].as_str() {
                println!("    {reason}");
            }
        }
    }
    if !providers.is_empty() {
        println!();
        println!("providers:");
        for p in providers {
            println!("  {:<20} {}", p.name, p.kind);
        }
    }
    Ok(())
}

/// `lanius agent run` - blocking native/profile-agent turn.
pub fn run(root: &Root, opts: RunOpts) -> Result<()> {
    validate_profile_name(&opts.profile)?;
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    packages::sync(root, &conn)?;
    validate_with_packages(root, &conn, &opts.profile, &opts.with_packages)?;
    exec::run(
        root,
        ExecOpts {
            session: opts.session,
            profile: opts.profile,
            prompt: Some(opts.prompt),
            resume: None,
            event: None,
            with_packages: opts.with_packages,
            provider: opts.provider,
            model: None,
            budget: None,
        },
    )
}

/// `lanius agent spawn` - durable background native/profile-agent turn.
///
/// This emits work to the profile's agent mailbox. It is intentionally gated on
/// an approved exec handler matching that mailbox; otherwise the event would be
/// immediately marked done with no consumer by the daemon.
pub fn spawn(root: &Root, opts: SpawnOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    packages::sync(root, &conn)?;
    let descriptor = spawn_core(
        root,
        &conn,
        SpawnRequest {
            profile: opts.profile,
            prompt: opts.prompt,
            session: opts.session,
            priority: opts.priority,
            with_packages: opts.with_packages,
            provider: opts.provider,
            created_by: None,
            model: None,
            budget: None,
        },
    )?;
    println!("{descriptor}");
    Ok(())
}

/// Shared spawn core for the CLI (`lanius agent spawn`) and the native
/// `launch_agent` tool (docs/handoffs/agent-launching.md M3). Validates the
/// profile + `--with-package` names, gates on an approved exec handler for the
/// profile's mailbox (otherwise the daemon would mark the event done with no
/// consumer), and emits the work — with any launch overrides riding the payload —
/// onto that mailbox. Assumes `packages::sync` already ran on `conn`. Returns the
/// launch descriptor {event, correlation, session, profile, agent, mailbox}.
pub fn spawn_core(root: &Root, conn: &Connection, req: SpawnRequest) -> Result<Value> {
    validate_profile_name(&req.profile)?;
    validate_with_packages(root, conn, &req.profile, &req.with_packages)?;
    let (prof, _) = profile::load(root, &req.profile)?;
    let mailbox = topic::agent_mailbox(&prof.agent);
    let handlers = packages::matching_exec_handlers(root, conn, &mailbox)?;
    if handlers.is_empty() {
        bail!(
            "profile {:?} is not daemon-drivable: no approved exec package subscribes to {}",
            req.profile,
            mailbox
        );
    }
    let correlation = format!("agent-spawn-{}", uuid::Uuid::new_v4().simple());
    let session = req
        .session
        .unwrap_or_else(|| format!("agent-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let mut payload = json!({
        "prompt": req.prompt,
        "profile": req.profile,
        "session": session,
    });
    // Launch overrides ride the payload (wonky bit 1): the exec side applies them
    // for that run only. Omitted when empty so an ordinary spawn is byte-identical.
    if !req.with_packages.is_empty() {
        payload["with_packages"] = json!(req.with_packages);
    }
    if let Some(provider) = &req.provider {
        payload["provider"] = json!(provider);
    }
    // The groundskeeper pipeline (docs/handoffs/kb-groundskeeper.md M3) threads its
    // config-chosen model + per-pass budget onto the spawn payload. Omitted when
    // None so an ordinary spawn stays byte-identical.
    if let Some(model) = &req.model {
        payload["model"] = json!(model);
    }
    if let Some(budget) = req.budget {
        payload["budget"] = json!(budget);
    }
    let event_id = events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(payload),
            priority: req.priority,
            correlation: Some(correlation.clone()),
            sender: req.created_by.or_else(current_actor),
            ..EmitOpts::new(&mailbox)
        },
    )?;
    Ok(json!({
        "event": event_id,
        "correlation": correlation,
        "session": session,
        "profile": req.profile,
        "agent": prof.agent,
        "mailbox": mailbox,
    }))
}

fn profile_rows(root: &Root, conn: &Connection) -> Result<Vec<Value>> {
    let mut names: Vec<String> = match std::fs::read_dir(root.dir.join("profiles")) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    let config_agents = root.dir.join("config/agents");
    if let Ok(rd) = std::fs::read_dir(&config_agents) {
        for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
            names.push(e.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    names.dedup();

    let mut out = Vec::new();
    for name in names {
        let (prof, _pdir) = match profile::load(root, &name) {
            Ok(loaded) => loaded,
            Err(e) => {
                out.push(json!({
                    "profile": name,
                    "ok": false,
                    "error": format!("{e:#}"),
                }));
                continue;
            }
        };
        let mailbox = topic::agent_mailbox(&prof.agent);
        let handlers = packages::matching_exec_handlers(root, conn, &mailbox).unwrap_or_default();
        let packages = packages_for_profile(root, &name).unwrap_or_default();
        out.push(json!({
            "profile": name,
            "ok": true,
            "agent": prof.agent,
            "owner": prof.owner,
            "model": prof.model.model,
            "provider": prof.model.provider,
            "max_turns": prof.model.max_turns,
            "mailbox": mailbox,
            "daemon_drivable": !handlers.is_empty(),
            "daemon_reason": if handlers.is_empty() {
                Some(format!("no approved exec package subscribes to {}", mailbox))
            } else {
                None
            },
            "handlers": handlers.into_iter().map(|(pkg, path)| json!({
                "package": pkg,
                "script": path,
            })).collect::<Vec<_>>(),
            "packages": packages,
            "subagents": prof.subagents,
            "autonomy": prof.autonomy,
        }));
    }
    Ok(out)
}

fn packages_for_profile(root: &Root, profile_name: &str) -> Result<Vec<Value>> {
    Ok(packages::discover_for_profile(root, profile_name)?
        .into_iter()
        .map(|p| {
            json!({
                "name": p.name,
                "dir": p.dir,
                "kind": match (&p.manifest, &p.meta) {
                    (Some(_), Some(_)) => "actor+skill",
                    (Some(_), None) => "actor",
                    (None, Some(_)) => "skill",
                    (None, None) => "empty",
                },
                "mode": p.manifest.as_ref()
                    .and_then(|lm| lm.manifest.process.as_ref().map(|pr| pr.mode.clone())),
                "skill": p.meta.as_ref().map(|m| json!({
                    "name": m.name,
                    "description": m.description,
                })),
                "stages": p.manifest.as_ref()
                    .map(|lm| lm.manifest.stage.iter().map(|s| s.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
                "mcp": p.manifest.as_ref()
                    .map(|lm| lm.manifest.mcp.iter().map(|m| m.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Validate `--with-package` names for a run/spawn launch
/// (docs/handoffs/agent-launching.md M2, wonky bit 1). A package already on the
/// profile's path needs no extension. A package NOT on the path is allowed as a
/// run-scoped VISIBILITY extension only when it is installed on the instance AND
/// granted (approved) — widening what the run can SEE, never what it may DO
/// (bus authority stays gated by the grants ledger). Anything else bails loudly.
fn validate_with_packages(
    root: &Root,
    conn: &Connection,
    profile_name: &str,
    required: &[String],
) -> Result<()> {
    if required.is_empty() {
        return Ok(());
    }
    let visible: BTreeSet<String> = packages::discover_for_profile(root, profile_name)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let universe: BTreeSet<String> = packages::discover(root)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    for pkg in required {
        if !topic::valid_name(pkg) || pkg.contains('/') {
            bail!("package {pkg:?} must be one topic level");
        }
        if visible.contains(pkg) {
            continue; // already on the profile's path — no extension needed
        }
        if !universe.contains(pkg) {
            bail!(
                "package {pkg:?} is not installed on this instance; \
                 `lanius agent catalog` lists what is available"
            );
        }
        if !packages::is_granted(conn, pkg)? {
            bail!(
                "package {pkg:?} is not granted (approved) — a launch may widen visibility \
                 only to approved packages; run `lanius approve {pkg}` first"
            );
        }
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("bad profile name {name:?} (alphanumeric, dash, underscore)");
    }
    Ok(())
}

fn current_actor() -> Option<String> {
    std::env::var("LANIUS_ACTOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("HARNESS_ACTOR")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("LANIUS_CODE_SESSION")
                .ok()
                .filter(|s| !s.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-agentcli-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "agent = \"main\"\n[model]\nmodel = \"m\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("packages")).unwrap();
        Root { dir }
    }

    fn write_pkg(root: &Root, name: &str, manifest: &str) {
        let d = root.dir.join("packages").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("lanius.toml"), manifest).unwrap();
    }

    #[test]
    fn validates_profile_names() {
        assert!(validate_profile_name("worker_1").is_ok());
        assert!(validate_profile_name("../x").is_err());
    }

    #[test]
    fn catalog_profile_rows_are_machine_pickable() {
        // M1 acceptance: `lanius agent catalog --json` must be complete enough for
        // an agent to pick a profile AND its packages. Assert the per-profile row
        // carries the fields that choice needs.
        let root = scratch("catalog-json");
        write_pkg(
            &root,
            "helper-pkg",
            "[request]\nsubscribe = [\"in/package/helper/x\"]\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        let rows = profile_rows(&root, &conn).unwrap();
        let default = rows
            .iter()
            .find(|r| r["profile"] == "default")
            .expect("default profile in catalog");
        for field in [
            "profile",
            "agent",
            "model",
            "provider",
            "mailbox",
            "daemon_drivable",
            "packages",
            "subagents",
            "autonomy",
        ] {
            assert!(
                default.get(field).is_some(),
                "catalog row missing field {field:?}: {default}"
            );
        }
        assert!(default["packages"].is_array(), "packages must be a list");
        let has_pkg = default["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "helper-pkg");
        assert!(has_pkg, "per-profile package list should name the package");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn with_package_unknown_name_bails() {
        let root = scratch("unknown-pkg");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        let err = validate_with_packages(&root, &conn, "default", &["missing".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not installed"), "got: {err}");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn with_package_ungranted_bails_but_granted_passes() {
        // M2 (wonky bit 1): a launch may widen visibility only to APPROVED
        // packages. An installed-but-pending package refuses; approving it makes
        // the same `--with-package` succeed. Visibility, not authority.
        let root = scratch("granted-pkg");
        write_pkg(
            &root,
            "extra",
            "[request]\nsubscribe = [\"in/package/extra/x\"]\n",
        );
        // A profile whose path does NOT include `extra` (so it isn't already
        // visible), while `extra` stays in the instance universe (the default
        // `packages/` path). This is the case the extension exists for.
        let wdir = root.dir.join("profiles/worker");
        std::fs::create_dir_all(&wdir).unwrap();
        std::fs::write(
            wdir.join("profile.toml"),
            "agent = \"w\"\nelanus_path = [\"empty\"]\n[model]\nmodel = \"m\"\n",
        )
        .unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        // Installed but its request is still pending → not granted → refuse.
        let err = validate_with_packages(&root, &conn, "worker", &["extra".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not granted"), "got: {err}");
        // Approve it → the same launch is now allowed.
        packages::decide(&root, &conn, "extra", true, "test").unwrap();
        validate_with_packages(&root, &conn, "worker", &["extra".to_string()])
            .expect("a granted package may be a run-scoped extension");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn spawn_core_emits_mailbox_event_with_overrides_and_provenance() {
        // M3: spawn_core (shared by the CLI and the launch_agent tool) gates on an
        // approved exec handler, then emits ONE event onto the profile's mailbox
        // carrying the prompt, launch overrides, correlation and provenance.
        let root = scratch("spawn-core");
        // An exec-mode package subscribed (and approved) for the default agent's
        // mailbox makes the profile daemon-drivable.
        let mailbox = topic::agent_mailbox("main");
        write_pkg(
            &root,
            "runner",
            &format!(
                "[process]\nmode = \"exec\"\nrun = \"main\"\n[request]\nsubscribe = [\"{mailbox}\"]\n"
            ),
        );
        // A granted extra package the launch will widen to.
        write_pkg(
            &root,
            "extra",
            "[request]\nsubscribe = [\"in/package/extra/x\"]\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        packages::decide(&root, &conn, "runner", true, "test").unwrap();
        packages::decide(&root, &conn, "extra", true, "test").unwrap();

        let desc = spawn_core(
            &root,
            &conn,
            SpawnRequest {
                profile: "default".into(),
                prompt: "read the reactor logs".into(),
                session: Some("sess-1".into()),
                priority: 0,
                with_packages: vec!["extra".into()],
                provider: Some("deepseek".into()),
                created_by: Some("launcher-agent".into()),
                model: Some("claude-haiku".into()),
                budget: Some(4096),
            },
        )
        .expect("spawn_core should emit");
        let corr = desc["correlation"].as_str().unwrap().to_string();
        assert_eq!(desc["session"], "sess-1");
        assert_eq!(desc["mailbox"], mailbox);

        // Exactly one event on the mailbox, with the right payload + sender.
        let (payload, sender): (String, Option<String>) = conn
            .query_row(
                "SELECT payload, sender FROM events WHERE type=?1 AND correlation_id=?2",
                params![mailbox, corr],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let p: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(p["prompt"], "read the reactor logs");
        assert_eq!(p["session"], "sess-1");
        assert_eq!(p["with_packages"], json!(["extra"]));
        assert_eq!(p["provider"], "deepseek");
        // The groundskeeper's model + budget thread onto the spawn payload (M3).
        assert_eq!(p["model"], "claude-haiku");
        assert_eq!(p["budget"], 4096);
        assert_eq!(sender.as_deref(), Some("launcher-agent"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn spawn_core_refuses_profile_without_exec_handler() {
        // M3: a profile whose mailbox has no approved exec handler is not
        // daemon-drivable — spawn_core refuses clearly rather than dropping the
        // event where the daemon would mark it done with no consumer.
        let root = scratch("spawn-no-handler");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        let err = spawn_core(
            &root,
            &conn,
            SpawnRequest {
                profile: "default".into(),
                prompt: "hi".into(),
                session: None,
                priority: 0,
                with_packages: Vec::new(),
                provider: None,
                created_by: None,
                model: None,
                budget: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not daemon-drivable"), "got: {err}");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

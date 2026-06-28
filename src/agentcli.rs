//! `elanus agent` - front door for native/profile agents plus launch discovery.

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
}

pub struct SpawnOpts {
    pub profile: String,
    pub prompt: String,
    pub session: Option<String>,
    pub priority: i64,
    pub with_packages: Vec<String>,
}

/// `elanus agent catalog` - one inventory surface for launchable things.
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

/// `elanus agent run` - blocking native/profile-agent turn.
pub fn run(root: &Root, opts: RunOpts) -> Result<()> {
    validate_profile_name(&opts.profile)?;
    ensure_profile_packages(root, &opts.profile, &opts.with_packages)?;
    exec::run(
        root,
        ExecOpts {
            session: opts.session,
            profile: opts.profile,
            prompt: Some(opts.prompt),
            resume: None,
            event: None,
        },
    )
}

/// `elanus agent spawn` - durable background native/profile-agent turn.
///
/// This emits work to the profile's agent mailbox. It is intentionally gated on
/// an approved exec handler matching that mailbox; otherwise the event would be
/// immediately marked done with no consumer by the daemon.
pub fn spawn(root: &Root, opts: SpawnOpts) -> Result<()> {
    validate_profile_name(&opts.profile)?;
    ensure_profile_packages(root, &opts.profile, &opts.with_packages)?;
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    packages::sync(root, &conn)?;
    let (prof, _) = profile::load(root, &opts.profile)?;
    let mailbox = topic::agent_mailbox(&prof.agent);
    let handlers = packages::matching_exec_handlers(root, &conn, &mailbox)?;
    if handlers.is_empty() {
        bail!(
            "profile {:?} is not daemon-drivable: no approved exec package subscribes to {}",
            opts.profile,
            mailbox
        );
    }
    let correlation = format!("agent-spawn-{}", uuid::Uuid::new_v4().simple());
    let session = opts
        .session
        .unwrap_or_else(|| format!("agent-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let event_id = events::emit(
        root,
        &conn,
        EmitOpts {
            payload: Some(json!({
                "prompt": opts.prompt,
                "profile": opts.profile,
                "session": session,
            })),
            priority: opts.priority,
            correlation: Some(correlation.clone()),
            sender: current_actor(),
            ..EmitOpts::new(&mailbox)
        },
    )?;
    println!(
        "{}",
        json!({
            "event": event_id,
            "correlation": correlation,
            "session": session,
            "profile": opts.profile,
            "agent": prof.agent,
            "mailbox": mailbox,
        })
    );
    Ok(())
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

fn ensure_profile_packages(root: &Root, profile_name: &str, required: &[String]) -> Result<()> {
    if required.is_empty() {
        return Ok(());
    }
    let visible: BTreeSet<String> = packages::discover_for_profile(root, profile_name)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    for pkg in required {
        if !topic::valid_name(pkg) || pkg.contains('/') {
            bail!("package {pkg:?} must be one topic level");
        }
        if !visible.contains(pkg) {
            bail!(
                "package {:?} is not visible to profile {:?}; add it to that profile's elanus_path or choose another profile",
                pkg,
                profile_name
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
    std::env::var("ELANUS_ACTOR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("HARNESS_ACTOR")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("ELANUS_CODE_SESSION")
                .ok()
                .filter(|s| !s.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-agentcli-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "agent = \"main\"\n[model]\nmodel = \"m\"\n",
        )
        .unwrap();
        Root { dir }
    }

    #[test]
    fn with_package_requires_profile_visibility() {
        let root = scratch("pkg");
        let err = ensure_profile_packages(&root, "default", &["missing".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not visible"));
    }

    #[test]
    fn validates_profile_names() {
        assert!(validate_profile_name("worker_1").is_ok());
        assert!(validate_profile_name("../x").is_err());
    }
}

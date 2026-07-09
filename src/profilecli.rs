//! `lanius profile` — list / get / set / new. The CLI is the API: the web
//! UI's agent management shells out to these, so profile.toml editing
//! lives in ONE place (toml_edit, comments preserved) instead of being
//! reimplemented in node.

use crate::config_repo;
use crate::paths::Root;
use crate::profile;
use anyhow::{bail, Context, Result};
use serde_json::json;

/// All profiles on disk, as JSON lines: the fields the UI's agent list
/// needs, parsed through the same loader the kernel uses.
pub fn list(root: &Root) -> Result<()> {
    let dir = root.dir.join("profiles");
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    for name in names {
        // One unloadable profile must NOT blank the whole list. A person editing
        // the raw file of a single agent would otherwise lose sight of every
        // agent, and the web UI treats a non-zero exit as total failure. Skip and
        // warn on the broken one, keep listing the rest, and still exit 0.
        let (p, pdir) = match profile::load(root, &name) {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("skipping profile {name:?}: {e:#}");
                continue;
            }
        };
        println!(
            "{}",
            json!({
                "profile": name,
                "agent": p.agent,
                "owner": p.owner,
                "parent": p.parent,
                "model": p.model.model,
                "max_turns": p.model.max_turns,
                "provider": p.model.provider,
                "base_url": p.model.base_url,
                "api_key_env": p.model.api_key_env,
                "workdir": p.sandbox.workdir,
                "fs_write": p.sandbox.fs_write,
                "capture_exclude": p.sandbox.capture_exclude,
                "skills": { "include": p.skills.include, "exclude": p.skills.exclude },
                "local_elanus_path": profile::local_elanus_path(root, &name).unwrap_or(None),
                "elanus_path": p.elanus_path,
                "package_path": p.elanus_path,
                "autonomy": p.autonomy,
                "context": {
                    "program": p.context.program,
                    "max_total_ms": p.context.max_total_ms,
                    "stages": p.context.stages,
                },
                "subagents": p.subagents,
                "ui": { "surface": p.ui.surface },
                "vars": p.vars,
                "throttle": p.throttle,
                "dir": pdir,
            })
        );
    }
    Ok(())
}

/// One profile: parsed summary plus the raw TOML (the UI shows the form
/// AND offers the file).
pub fn get(root: &Root, name: &str) -> Result<()> {
    println!("{}", get_value(root, name)?);
    Ok(())
}

/// The `get` JSON as a value (so tests can assert the surfaced fields without
/// capturing stdout).
fn get_value(root: &Root, name: &str) -> Result<serde_json::Value> {
    valid_name(name)?;
    let (p, pdir) = profile::load(root, name)?;
    let raw = std::fs::read_to_string(pdir.join("profile.toml")).unwrap_or_default();
    Ok(json!({
        "profile": name,
        "agent": p.agent,
        "owner": p.owner,
        "parent": p.parent,
        "model": p.model.model,
        "max_turns": p.model.max_turns,
        "provider": p.model.provider,
        "base_url": p.model.base_url,
        "api_key_env": p.model.api_key_env,
        "workdir": p.sandbox.workdir,
        "fs_write": p.sandbox.fs_write,
        "capture_exclude": p.sandbox.capture_exclude,
        // The read/network cage keys — surfaced typed so the web UI reads
        // them as fields instead of re-parsing the raw TOML blob (sandbox-
        // config-ui M1). `cage` is this profile's computed posture in
        // product words, through the one shared mapping.
        "network": p.sandbox.network,
        "fs_read_deny": p.sandbox.fs_read_deny,
        "fs_read_allow": p.sandbox.fs_read_allow,
        "cage": crate::web::cage_status_json(&p.sandbox),
        "skills": { "include": p.skills.include, "exclude": p.skills.exclude },
        "local_elanus_path": profile::local_elanus_path(root, name).unwrap_or(None),
        "elanus_path": p.elanus_path,
        "package_path": p.elanus_path,
        "autonomy": p.autonomy,
        "context": {
            "program": p.context.program,
            "max_total_ms": p.context.max_total_ms,
            "stages": p.context.stages,
        },
        "subagents": p.subagents,
        "vars": p.vars,
        "throttle": p.throttle,
        "toml": raw,
    }))
}

/// Set dotted keys: `lanius profile set default agent=kestrel
/// model.max_turns=12 'skills.include=["#"]'`. The right-hand side is
/// parsed as a TOML value when it parses (ints, bools, arrays, quoted
/// strings) and treated as a bare string otherwise. Comments survive
/// (toml_edit). The file is validated through the kernel loader BEFORE
/// being written — a set that produces an unloadable profile never lands.
pub fn set(root: &Root, name: &str, pairs: &[String]) -> Result<Option<String>> {
    valid_name(name)?;
    config_repo::init(root)?;
    let pdir = root.profile_dir(name);
    let f = pdir.join("profile.toml");
    if !f.exists() {
        bail!("no profile {name:?} (create it with `lanius profile new {name}`)");
    }
    let raw = std::fs::read_to_string(&f)?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parsing {}", f.display()))?;
    for pair in pairs {
        let Some((path, val)) = pair.split_once('=') else {
            bail!("expected key=value, got {pair:?}");
        };
        let value = parse_value(val);
        let path = if path == "package_path" {
            "elanus_path"
        } else {
            path
        };
        if path == "elanus_path" {
            doc.remove("package_path");
        }
        let segs: Vec<&str> = path.split('.').collect();
        if segs.iter().any(|s| s.is_empty()) {
            bail!("bad key path {path:?}");
        }
        let mut item: &mut toml_edit::Item = doc.as_item_mut();
        for seg in &segs[..segs.len() - 1] {
            if item.get(seg).is_none() {
                item[seg] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            item = &mut item[seg];
        }
        item[segs[segs.len() - 1]] = toml_edit::Item::Value(value);
        println!("set {path} = {}", val.trim());
    }
    // Validate before writing: serde must accept what we produced.
    let candidate = doc.to_string();
    let parsed: profile::Profile = toml::from_str(&candidate)
        .with_context(|| "refusing to write: the result would not load as a profile")?;
    profile::validate(&parsed)
        .with_context(|| "refusing to write: the result would not load as a profile")?;
    write_profile_and_commit(root, name, &f, &candidate, "config: update agent profile")
}

/// Validate that a candidate `profile.toml` (read from `path`) would load as a
/// Profile — the same serde check `set` runs before it writes. Prints nothing on
/// success; exits non-zero with the reason otherwise. This lets an UNtrusted raw
/// editor (the web UI's "edit the file" box) refuse to save a file that would
/// silently break the agent, without the kernel having to trust the bytes.
pub fn validate(path: &str) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let parsed: profile::Profile =
        toml::from_str(&raw).with_context(|| "the file would not load as a profile")?;
    profile::validate(&parsed).with_context(|| "the file would not load as a profile")?;
    Ok(())
}

/// Replace a raw profile.toml from a candidate file. Used by the web raw editor
/// so it never owns direct profile writes.
pub fn put(root: &Root, name: &str, path: &str) -> Result<Option<String>> {
    valid_name(name)?;
    config_repo::init(root)?;
    let candidate = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let parsed: profile::Profile =
        toml::from_str(&candidate).with_context(|| "the file would not load as a profile")?;
    profile::validate(&parsed).with_context(|| "the file would not load as a profile")?;
    let pdir = root.profile_dir(name);
    std::fs::create_dir_all(&pdir)?;
    let f = pdir.join("profile.toml");
    write_profile_and_commit(
        root,
        name,
        &f,
        &candidate,
        "config: update raw agent profile",
    )
}

/// Scaffold a profile: agent noun defaults to the profile name, blocks
/// seeded from the default profile's (an agent should start with SOME
/// identity to edit, not an empty prompt).
pub fn new(
    root: &Root,
    name: &str,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<Option<String>> {
    valid_name(name)?;
    config_repo::init(root)?;
    let agent = agent.unwrap_or(name);
    if !crate::topic::valid_name(agent) || agent.contains('/') {
        bail!("agent {agent:?} must be one topic level (it becomes in/agent/{agent})");
    }
    let pdir = root.profile_dir(name);
    if pdir.join("profile.toml").exists() {
        bail!("profile {name:?} already exists");
    }
    std::fs::create_dir_all(pdir.join("blocks"))?;
    let model_line = model
        .map(|m| format!("model = \"{m}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        pdir.join("profile.toml"),
        format!(
            "# profile {name} — created by `lanius profile new`\n\
             agent = \"{agent}\"\nowner = \"owner\"\n\n[model]\n{model_line}"
        ),
    )?;
    // Seed blocks from default (copy-if-present, never required).
    let src = root.profile_dir("default").join("blocks");
    if src.is_dir() && name != "default" {
        for e in std::fs::read_dir(&src)?.filter_map(|e| e.ok()) {
            let to = pdir.join("blocks").join(e.file_name());
            if e.path().is_file() && !to.exists() {
                std::fs::copy(e.path(), to)?;
            }
        }
    }
    println!("created profile {name} (agent {agent}, mailbox in/agent/{agent})");
    println!(
        "dispatch to it: lanius emit in/agent/{agent} --payload '{{\"prompt\":\"...\",\"profile\":\"{name}\"}}'"
    );
    match config_repo::commit_agent(root, name, "config: create agent profile") {
        Ok((sha, changed)) => Ok(changed.then_some(sha)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&pdir);
            config_repo::reset_agent(root, name);
            Err(e)
        }
    }
}

fn write_profile_and_commit(
    root: &Root,
    name: &str,
    file: &std::path::Path,
    candidate: &str,
    msg: &str,
) -> Result<Option<String>> {
    let prior = std::fs::read_to_string(file).ok();
    if prior.as_deref() == Some(candidate) {
        return Ok(None);
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, candidate)?;
    std::fs::rename(&tmp, file)?;
    match config_repo::commit_agent(root, name, msg) {
        Ok((sha, true)) => Ok(Some(sha)),
        Ok((_sha, false)) => Ok(None),
        Err(e) => {
            match prior {
                Some(p) => {
                    let _ = std::fs::write(file, p);
                }
                None => {
                    let _ = std::fs::remove_file(file);
                }
            }
            config_repo::reset_agent(root, name);
            Err(e)
        }
    }
}

fn parse_value(raw: &str) -> toml_edit::Value {
    let trimmed = raw.trim();
    // Parse through a scratch doc so arrays/ints/bools/strings all work.
    if let Ok(doc) = format!("x = {trimmed}").parse::<toml_edit::DocumentMut>() {
        if let Some(v) = doc["x"].as_value() {
            return v.clone();
        }
    }
    toml_edit::Value::from(trimmed)
}

fn valid_name(name: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-profcli-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "# keep me\nagent = \"main\"\n\n[model]\nmodel = \"m1\"\n",
        )
        .unwrap();
        Root { dir }
    }

    #[test]
    fn set_preserves_comments_and_validates() {
        let root = scratch("set");
        config_repo::init(&root).unwrap();
        set(
            &root,
            "default",
            &[
                "agent=kestrel".into(),
                "model.max_turns=7".into(),
                "context.max_total_ms=12000".into(),
                "context.stage=[{package=\"window\",name=\"window\",enabled=true,order=10,timeout_ms=9000}]".into(),
                "subagents.allow_profiles=[\"scout\"]".into(),
                "subagents.max_depth=2".into(),
            ],
        )
        .unwrap();
        let raw =
            std::fs::read_to_string(root.profile_dir("default").join("profile.toml")).unwrap();
        assert!(raw.contains("# keep me"));
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(p.agent, "kestrel");
        assert_eq!(p.model.max_turns, 7);
        assert_eq!(p.context.program, "default");
        assert_eq!(p.context.max_total_ms, 12_000);
        assert_eq!(p.context.stages.len(), 1);
        assert_eq!(p.context.stages[0].package, "window");
        assert_eq!(p.context.stages[0].timeout_ms, Some(9000));
        assert_eq!(p.subagents.allow_profiles, vec!["scout".to_string()]);
        assert_eq!(p.subagents.max_depth, 2);
        // An invalid set must not land.
        assert!(set(&root, "default", &["model.max_turns=\"lots\"".into()]).is_err());
        assert!(set(&root, "default", &["context.program=\"custom\"".into()]).is_err());
        assert!(set(
            &root,
            "default",
            &["subagents.grant_policy=\"wide\"".into()]
        )
        .is_err());
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(
            p.model.max_turns, 7,
            "failed set must leave the file untouched"
        );
        assert_eq!(
            p.context.max_total_ms, 12_000,
            "failed set must leave context config untouched"
        );
        assert_eq!(
            p.subagents.max_depth, 2,
            "failed set must leave subagent config untouched"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn get_surfaces_cage_keys_and_per_agent_posture() {
        // M1: `profile get` emits the three read/network keys typed, plus a `cage`
        // posture computed FOR THAT profile — not the default's.
        let root = scratch("get-cage");
        config_repo::init(&root).unwrap();
        set(
            &root,
            "default",
            &[
                "sandbox.network=\"none\"".into(),
                "sandbox.fs_read_deny=[\"/x\",\"/y\"]".into(),
                "sandbox.fs_read_allow=[\"/z\"]".into(),
            ],
        )
        .unwrap();
        let v = get_value(&root, "default").unwrap();
        assert_eq!(v["network"], json!("none"));
        assert_eq!(v["fs_read_deny"], json!(["/x", "/y"]));
        assert_eq!(v["fs_read_allow"], json!(["/z"]));
        // The cage block agrees with the shared mapping over this profile's config.
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(v["cage"], crate::web::cage_status_json(&p.sandbox));
        // On the enforcement platform (macOS + sandbox-exec) the words are the
        // policy words: network "none" reads "network off".
        if crate::sandbox::enforcement_available() {
            assert_eq!(v["cage"]["network"], json!("network off"));
        }
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn new_scaffolds_with_seeded_blocks() {
        let root = scratch("new");
        std::fs::create_dir_all(root.profile_dir("default").join("blocks")).unwrap();
        std::fs::write(root.profile_dir("default").join("blocks/00-x.md"), "id").unwrap();
        config_repo::init(&root).unwrap();
        new(&root, "scout2", None, Some("claude-haiku-4-5-20251001")).unwrap();
        let (p, _) = profile::load(&root, "scout2").unwrap();
        assert_eq!(p.agent, "scout2");
        assert_eq!(p.model.model, "claude-haiku-4-5-20251001");
        assert!(root.profile_dir("scout2").join("blocks/00-x.md").exists());
        assert!(new(&root, "scout2", None, None).is_err(), "no clobber");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn set_migrates_package_path_alias() {
        let root = scratch("path-alias");
        config_repo::init(&root).unwrap();
        let file = root.profile_dir("default").join("profile.toml");
        std::fs::write(&file, "agent = \"main\"\npackage_path = [\"packages\"]\n").unwrap();
        set(
            &root,
            "default",
            &["elanus_path=[\"packages\", \"kits/demo\"]".into()],
        )
        .unwrap();
        let raw = std::fs::read_to_string(file).unwrap();
        assert!(raw.contains("elanus_path"));
        assert!(!raw.contains("package_path"));
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(
            p.elanus_path,
            vec!["packages".to_string(), "kits/demo".to_string()]
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

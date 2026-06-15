//! `elanus profile` — list / get / set / new. The CLI is the API: the web
//! UI's agent management shells out to these, so profile.toml editing
//! lives in ONE place (toml_edit, comments preserved) instead of being
//! reimplemented in node.

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
                "model": p.model.model,
                "max_turns": p.model.max_turns,
                "workdir": p.sandbox.workdir,
                "fs_write": p.sandbox.fs_write,
                "skills": { "include": p.skills.include, "exclude": p.skills.exclude },
                "dir": pdir,
            })
        );
    }
    Ok(())
}

/// One profile: parsed summary plus the raw TOML (the UI shows the form
/// AND offers the file).
pub fn get(root: &Root, name: &str) -> Result<()> {
    valid_name(name)?;
    let (p, pdir) = profile::load(root, name)?;
    let raw = std::fs::read_to_string(pdir.join("profile.toml")).unwrap_or_default();
    println!(
        "{}",
        json!({
            "profile": name,
            "agent": p.agent,
            "owner": p.owner,
            "model": p.model.model,
            "max_turns": p.model.max_turns,
            "base_url": p.model.base_url,
            "workdir": p.sandbox.workdir,
            "fs_write": p.sandbox.fs_write,
            "skills": { "include": p.skills.include, "exclude": p.skills.exclude },
            "package_path": p.package_path,
            "toml": raw,
        })
    );
    Ok(())
}

/// Set dotted keys: `elanus profile set default agent=kestrel
/// model.max_turns=12 'skills.include=["#"]'`. The right-hand side is
/// parsed as a TOML value when it parses (ints, bools, arrays, quoted
/// strings) and treated as a bare string otherwise. Comments survive
/// (toml_edit). The file is validated through the kernel loader BEFORE
/// being written — a set that produces an unloadable profile never lands.
pub fn set(root: &Root, name: &str, pairs: &[String]) -> Result<()> {
    valid_name(name)?;
    let pdir = root.profile_dir(name);
    let f = pdir.join("profile.toml");
    if !f.exists() {
        bail!("no profile {name:?} (create it with `elanus profile new {name}`)");
    }
    let raw = std::fs::read_to_string(&f)?;
    let mut doc: toml_edit::DocumentMut =
        raw.parse().with_context(|| format!("parsing {}", f.display()))?;
    for pair in pairs {
        let Some((path, val)) = pair.split_once('=') else {
            bail!("expected key=value, got {pair:?}");
        };
        let value = parse_value(val);
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
    toml::from_str::<profile::Profile>(&candidate)
        .with_context(|| "refusing to write: the result would not load as a profile")?;
    std::fs::write(&f, candidate)?;
    Ok(())
}

/// Validate that a candidate `profile.toml` (read from `path`) would load as a
/// Profile — the same serde check `set` runs before it writes. Prints nothing on
/// success; exits non-zero with the reason otherwise. This lets an UNtrusted raw
/// editor (the web UI's "edit the file" box) refuse to save a file that would
/// silently break the agent, without the kernel having to trust the bytes.
pub fn validate(path: &str) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    toml::from_str::<profile::Profile>(&raw)
        .with_context(|| "the file would not load as a profile")?;
    Ok(())
}

/// Scaffold a profile: agent noun defaults to the profile name, blocks
/// seeded from the default profile's (an agent should start with SOME
/// identity to edit, not an empty prompt).
pub fn new(root: &Root, name: &str, agent: Option<&str>, model: Option<&str>) -> Result<()> {
    valid_name(name)?;
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
            "# profile {name} — created by `elanus profile new`\n\
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
    println!("dispatch to it: elanus emit in/agent/{agent} --payload '{{\"prompt\":\"...\",\"profile\":\"{name}\"}}'");
    Ok(())
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
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
        set(&root, "default", &["agent=kestrel".into(), "model.max_turns=7".into()]).unwrap();
        let raw = std::fs::read_to_string(root.profile_dir("default").join("profile.toml")).unwrap();
        assert!(raw.contains("# keep me"));
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(p.agent, "kestrel");
        assert_eq!(p.model.max_turns, 7);
        // An invalid set must not land.
        assert!(set(&root, "default", &["model.max_turns=\"lots\"".into()]).is_err());
        let (p, _) = profile::load(&root, "default").unwrap();
        assert_eq!(p.model.max_turns, 7, "failed set must leave the file untouched");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn new_scaffolds_with_seeded_blocks() {
        let root = scratch("new");
        std::fs::create_dir_all(root.profile_dir("default").join("blocks")).unwrap();
        std::fs::write(root.profile_dir("default").join("blocks/00-x.md"), "id").unwrap();
        new(&root, "scout2", None, Some("claude-haiku-4-5-20251001")).unwrap();
        let (p, _) = profile::load(&root, "scout2").unwrap();
        assert_eq!(p.agent, "scout2");
        assert_eq!(p.model.model, "claude-haiku-4-5-20251001");
        assert!(root.profile_dir("scout2").join("blocks/00-x.md").exists());
        assert!(new(&root, "scout2", None, None).is_err(), "no clobber");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

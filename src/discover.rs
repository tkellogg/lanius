//! `lanius discover <query>` — the privileged capability search
//! (docs/handoffs/kb-discovery.md M1). It answers "you don't have the discord
//! package enabled, but it exists and matches your query."
//!
//! Privileged = it reads the instance's **package universe** (`packages::discover`
//! — the whole path, no profile filter) rather than the agent's own **visibility
//! set** (`discover_for_profile`). For every package the caller can NOT see, it
//! scans everything the package carries — `kb/` files, `SKILL.md`, `[[tool]]`,
//! `[[stage]]`, `[[mcp]]`, `[[harness]]`, `provides_builtin_tools` — for a match
//! against the query, and reports the hit with what enabling it would add and the
//! enable path. This is a read of package metadata on disk: it grants nothing
//! (visibility, not authority — src/packages.rs:150-151). Getting the capability
//! still rides the config-proposal flow.
//!
//! One implementation, three surfaces: this kernel scan is the CLI (`lanius
//! discover`), the `discovery` package's `find_capability` tool script shells it
//! (`--json`), and the discovery skill teaches it.

use crate::manifest::LoadedManifest;
use crate::packages::{self, Package};
use crate::paths::Root;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

/// A `kb/` file is scanned name + a bounded slice of its bytes, so a content
/// match ("discord api" → a note that mentions it) surfaces without reading a
/// pathological file whole. Names always match; content up to this many bytes.
const MAX_KB_SCAN_BYTES: usize = 64 * 1024;
/// Cap how many `kb/` files one package contributes to the scan — a runaway KB
/// must not turn a discovery call into an unbounded directory walk.
const MAX_KB_FILES: usize = 512;

/// What a not-enabled package carries that an agent gains by enabling it.
#[derive(Debug, Default, Serialize)]
pub struct Adds {
    /// `kb/`-relative file paths (the corpus that would join the search union).
    pub kb: Vec<String>,
    /// The `SKILL.md` name, if the package ships one.
    pub skills: Vec<String>,
    /// `[[tool]]` names the package would fold into the agent's tool array.
    pub tools: Vec<String>,
    /// `[[stage]]` names (context transforms).
    pub stages: Vec<String>,
    /// `[[mcp]]` server names (third-party tool servers).
    pub mcp: Vec<String>,
    /// `[[harness]]` names (`lanius code <name>` adapters).
    pub harnesses: Vec<String>,
    /// Built-in tools this package gates via `provides_builtin_tools`.
    pub builtin_tools: Vec<String>,
}

/// One discovery hit: a package the caller can NOT see that matches the query.
#[derive(Debug, Serialize)]
pub struct Match {
    pub package: String,
    /// Always false here — discovery only surfaces capabilities you lack.
    pub enabled: bool,
    /// Human-readable "what matched", e.g. `["kb/discord-api-notes.md", "package name"]`.
    pub matched: Vec<String>,
    /// What enabling the package would add.
    pub adds: Adds,
    /// The enable path — how to actually get the capability (rides the
    /// config-proposal flow; discovery invents no new mechanism).
    pub enable: String,
}

/// The whole discovery result for a query (the `--json` shape the tool wrapper
/// reshapes, and what the CLI renders).
#[derive(Debug, Serialize)]
pub struct Report {
    pub query: String,
    pub profile: String,
    pub matches: Vec<Match>,
}

/// Scan the package universe for capabilities the `profile` can NOT see that
/// match `query`. Name-sorted (discovery already sorts). An empty/whitespace
/// query matches nothing (there is nothing to search for).
pub fn scan(root: &Root, profile: &str, query: &str) -> Result<Report> {
    let tokens = crate::kb::query_tokens(query);
    let visible: BTreeSet<String> = packages::discover_for_profile(root, profile)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let mut matches = Vec::new();
    if !tokens.is_empty() {
        for pkg in packages::discover(root)? {
            if visible.contains(&pkg.name) {
                continue; // already visible to the agent — not "missing"
            }
            let adds = collect_adds(&pkg);
            let matched = match_package(&pkg, &adds, &tokens);
            if matched.is_empty() {
                continue;
            }
            matches.push(Match {
                enable: enable_guidance(&pkg.name, profile),
                package: pkg.name,
                enabled: false,
                matched,
                adds,
            });
        }
    }
    Ok(Report {
        query: query.to_string(),
        profile: profile.to_string(),
        matches,
    })
}

/// Everything a package carries, for the "what enabling adds" report.
fn collect_adds(pkg: &Package) -> Adds {
    let mut adds = Adds {
        kb: kb_files(pkg),
        ..Adds::default()
    };
    if let Some(meta) = &pkg.meta {
        adds.skills.push(meta.name.clone());
    }
    if let Some(lm) = &pkg.manifest {
        let m = &lm.manifest;
        adds.tools = m.tool.iter().map(|t| t.name.clone()).collect();
        adds.stages = m.stage.iter().map(|s| s.name.clone()).collect();
        adds.mcp = m.mcp.iter().map(|s| s.name.clone()).collect();
        adds.harnesses = m.harness.iter().map(|h| h.name.clone()).collect();
        adds.builtin_tools = m.provides_builtin_tools.clone();
    }
    adds
}

/// The `kb/`-relative paths of a package's knowledge files (bounded). A package
/// need not carry a `[kb]` marker to be discoverable by its `kb/` content — the
/// scan is over everything on disk, and a marker is only what `kb list`/the
/// search union key on.
fn kb_files(pkg: &Package) -> Vec<String> {
    let root = crate::kb::kb_dir(pkg);
    let mut out = Vec::new();
    walk_kb(&root, &root, &mut out);
    out.sort();
    out.truncate(MAX_KB_FILES);
    out
}

fn walk_kb(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        // Never follow a symlink out of the tree (agent-written content).
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() {
            walk_kb(base, &p, out);
        } else if ft.is_file() {
            if let Ok(rel) = p.strip_prefix(base) {
                out.push(format!("kb/{}", rel.to_string_lossy()));
            }
        }
        if out.len() >= MAX_KB_FILES {
            return;
        }
    }
}

/// Which carried things match the query, as human-readable labels. A candidate
/// matches when ANY query token is a case-insensitive substring of its haystack
/// (the package name, a kb file's name + bounded content, a skill's name +
/// description, a tool/stage/mcp/harness name, a gated builtin tool name).
fn match_package(pkg: &Package, adds: &Adds, tokens: &[String]) -> Vec<String> {
    let mut matched = Vec::new();
    let hit = |hay: &str| {
        tokens
            .iter()
            .any(|t| hay.to_lowercase().contains(t.as_str()))
    };

    if hit(&pkg.name) {
        matched.push("package name".to_string());
    }
    // kb files: name always, plus a bounded content read.
    let kb_root = crate::kb::kb_dir(pkg);
    for rel in &adds.kb {
        let name = rel.strip_prefix("kb/").unwrap_or(rel);
        let mut hay = name.to_string();
        if let Ok(bytes) = std::fs::read(kb_root.join(name)) {
            let n = bytes.len().min(MAX_KB_SCAN_BYTES);
            hay.push(' ');
            hay.push_str(&String::from_utf8_lossy(&bytes[..n]));
        }
        if hit(&hay) {
            matched.push(rel.clone());
        }
    }
    if let Some(meta) = &pkg.meta {
        if hit(&format!("{} {}", meta.name, meta.description)) {
            matched.push(format!("skill {}", meta.name));
        }
    }
    if let Some(lm) = &pkg.manifest {
        collect_manifest_hits(lm, &hit, &mut matched);
    }
    matched
}

fn collect_manifest_hits(
    lm: &LoadedManifest,
    hit: &impl Fn(&str) -> bool,
    matched: &mut Vec<String>,
) {
    let m = &lm.manifest;
    for t in &m.tool {
        if hit(&format!("{} {}", t.name, t.description)) {
            matched.push(format!("tool {}", t.name));
        }
    }
    for s in &m.stage {
        if hit(&s.name) {
            matched.push(format!("stage {}", s.name));
        }
    }
    for s in &m.mcp {
        if hit(&s.name) {
            matched.push(format!("mcp {}", s.name));
        }
    }
    for h in &m.harness {
        if hit(&h.name) {
            matched.push(format!("harness {}", h.name));
        }
    }
    for b in &m.provides_builtin_tools {
        if hit(b) {
            matched.push(format!("builtin tool {b}"));
        }
    }
    if let Some(kb) = &m.kb {
        let title = kb.title.clone().unwrap_or_default();
        let desc = kb.description.clone().unwrap_or_default();
        if !title.is_empty() || !desc.is_empty() {
            if hit(&format!("{title} {desc}")) && !matched.iter().any(|x| x.starts_with("kb/")) {
                matched.push(format!("kb: {title}"));
            }
        }
    }
}

/// The enable path an agent follows to actually get the capability. Honest to
/// the ground: the package is available in the instance but off the profile's
/// path; enablement rides the existing config-proposal flow (or the owner adds
/// it). Discovery invents no new enable mechanism (wonky bit 5).
fn enable_guidance(package: &str, profile: &str) -> String {
    format!(
        "package {package:?} is available in this instance but not on the {profile:?} \
         profile's path — to use it, request enablement through the existing \
         config-proposal flow (propose adding it to your profile, which a human or \
         autonomy accepts), or ask the owner to enable it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-discover-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("packages")).unwrap();
        Root { dir }
    }

    /// A discord-shaped package: a kb file, a skill, a tool — living in the
    /// instance universe but excluded from a worker profile's path.
    fn install_discord(root: &Root) {
        let d = root.packages().join("discord");
        std::fs::create_dir_all(d.join("kb")).unwrap();
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("lanius.toml"),
            "[kb]\ntitle = \"Discord API notes\"\n\n\
             [[tool]]\nname = \"send_discord\"\ndescription = \"post to a channel\"\nrun = \"scripts/send\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("kb/discord-api-notes.md"),
            "# Discord API\nRate limits, gateway intents, webhook posting.\n",
        )
        .unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: discord\ndescription: talk to Discord — channels, webhooks, the gateway\n---\n# discord\n",
        )
        .unwrap();
        std::fs::write(d.join("scripts/send"), "#!/bin/sh\ncat\n").unwrap();
    }

    /// A worker profile whose path is packages/, plus a `default` whose path is
    /// also packages/ — then narrow the worker so discord is off its path.
    fn write_profile(root: &Root, name: &str, path_entries: &str) {
        let pd = root.dir.join("profiles").join(name);
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("profile.toml"), path_entries).unwrap();
    }

    #[test]
    fn surfaces_a_missing_package_matching_the_query() {
        let root = scratch("hit");
        install_discord(&root);
        // The worker's path is an empty dir: discord is in the universe (default's
        // packages/) but NOT visible to the worker.
        write_profile(&root, "default", "elanus_path = [\"packages\"]\n");
        write_profile(&root, "worker", "elanus_path = [\"empty\"]\n");

        let rep = scan(&root, "worker", "discord api").unwrap();
        let m = rep
            .matches
            .iter()
            .find(|m| m.package == "discord")
            .expect("discord surfaces for a worker that lacks it");
        assert!(!m.enabled);
        // What enabling adds: the kb file, the skill, the tool.
        assert!(m.adds.kb.iter().any(|f| f == "kb/discord-api-notes.md"));
        assert!(m.adds.skills.iter().any(|s| s == "discord"));
        assert!(m.adds.tools.iter().any(|t| t == "send_discord"));
        // What matched is reported (the kb file by name/content and/or the pkg name).
        assert!(!m.matched.is_empty());
        // The enable path names the config-proposal flow, not a new mechanism.
        assert!(m.enable.contains("config-proposal"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn does_not_resurface_a_visible_capability() {
        let root = scratch("visible");
        install_discord(&root);
        write_profile(&root, "default", "elanus_path = [\"packages\"]\n");
        // The default profile CAN see discord → it is not "missing".
        let rep = scan(&root, "default", "discord api").unwrap();
        assert!(
            !rep.matches.iter().any(|m| m.package == "discord"),
            "a capability already visible to the agent must not be surfaced as missing"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn no_match_and_empty_query_return_nothing() {
        let root = scratch("nomatch");
        install_discord(&root);
        write_profile(&root, "default", "elanus_path = [\"packages\"]\n");
        write_profile(&root, "worker", "elanus_path = [\"empty\"]\n");
        // A query that matches nothing the discord package carries.
        let rep = scan(&root, "worker", "kubernetes helm chart").unwrap();
        assert!(rep.matches.is_empty(), "no capability matches → no hits");
        // An empty query matches nothing (there is nothing to search for).
        let empty = scan(&root, "worker", "   ").unwrap();
        assert!(empty.matches.is_empty());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn matches_a_tool_by_name_and_description() {
        let root = scratch("tool");
        let d = root.packages().join("paging");
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("lanius.toml"),
            "[[tool]]\nname = \"page_oncall\"\ndescription = \"escalate to the pager\"\nrun = \"scripts/p\"\n",
        )
        .unwrap();
        std::fs::write(d.join("scripts/p"), "#!/bin/sh\ncat\n").unwrap();
        write_profile(&root, "default", "elanus_path = [\"packages\"]\n");
        write_profile(&root, "worker", "elanus_path = [\"empty\"]\n");

        let rep = scan(&root, "worker", "pager escalation").unwrap();
        let m = rep.matches.iter().find(|m| m.package == "paging").unwrap();
        assert!(
            m.matched.iter().any(|x| x == "tool page_oncall"),
            "a tool matches by its description: {:?}",
            m.matched
        );
        assert!(m.adds.tools.iter().any(|t| t == "page_oncall"));
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

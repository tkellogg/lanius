use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// harness.toml — the harness-facing manifest inside a skill package.
/// SKILL.md stays pure per the agentskills.io spec; this sibling file carries
/// everything the dispatcher needs.
#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub handler: Vec<HandlerDecl>,
    #[serde(default)]
    pub hook: Vec<HookDecl>,
    #[serde(default)]
    pub cron: Vec<CronDecl>,
    #[serde(default)]
    pub provider: Vec<ProviderDecl>,
    #[serde(default)]
    pub throttle: BTreeMap<String, ThrottleDecl>,
}

#[derive(Debug, Deserialize)]
pub struct HandlerDecl {
    pub on: String, // MQTT topic filter ("work/agent/exec", "signal/#")
    pub run: String, // executable path relative to the skill dir
    #[serde(default = "default_order")]
    pub order: u32,
}

/// Blocking interception, git-hooks style: fork/exec with the subject JSON on
/// stdin. Exit 0 = allow (nonempty JSON-object stdout = rewritten subject);
/// nonzero = deny. `on_timeout` also covers spawn errors — fail-open vs
/// fail-closed is a security decision and is declared, never defaulted
/// silently (default is deny: a dead policy hook must not approve).
#[derive(Debug, Deserialize)]
pub struct HookDecl {
    pub point: String, // pre_tool_call | post_tool_call | pre_dispatch
    pub run: String,   // executable path relative to the skill dir
    #[serde(default = "default_order")]
    pub order: u32,
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_on_timeout")]
    pub on_timeout: String, // allow | deny
    /// MQTT filter against the tool name (tool hooks) or event topic
    /// (pre_dispatch). Default matches everything.
    #[serde(default = "default_match_all", rename = "match")]
    pub match_filter: String,
}

pub const HOOK_POINTS: &[&str] = &["pre_tool_call", "post_tool_call", "pre_dispatch"];

fn default_hook_timeout_ms() -> u64 {
    500
}
fn default_on_timeout() -> String {
    "deny".into()
}
fn default_match_all() -> String {
    "#".into()
}

#[derive(Debug, Deserialize)]
pub struct CronDecl {
    pub schedule: String, // standard 5-field cron
    pub emit: String,     // event type to emit when due
    pub payload: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderDecl {
    pub run: String, // executable contributing a context block at render time
    #[serde(default = "default_order")]
    pub order: u32,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ThrottleDecl {
    pub max_concurrent: Option<i64>,
    pub rate_per_min: Option<i64>,
    pub llm_tokens_per_hour: Option<i64>,
    pub coalesce: Option<bool>,
}

fn default_order() -> u32 {
    50
}

pub fn load(skill_dir: &Path) -> Result<Option<Manifest>> {
    let f = skill_dir.join("harness.toml");
    if !f.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(&f)?;
    let m: Manifest = toml::from_str(&s).with_context(|| format!("parsing {}", f.display()))?;
    Ok(Some(m))
}

/// Minimal SKILL.md frontmatter reader: name + description. Deliberately not a
/// full YAML parser — the spec's required fields are single-line scalars.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
}

pub fn skill_md(skill_dir: &Path) -> Option<SkillMeta> {
    let s = std::fs::read_to_string(skill_dir.join("SKILL.md")).ok()?;
    let mut lines = s.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if let Some(v) = t.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }
    Some(SkillMeta {
        name: name?,
        description: description.unwrap_or_default(),
    })
}

pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

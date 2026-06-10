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
    pub cron: Vec<CronDecl>,
    #[serde(default)]
    pub provider: Vec<ProviderDecl>,
    #[serde(default)]
    pub throttle: BTreeMap<String, ThrottleDecl>,
}

#[derive(Debug, Deserialize)]
pub struct HandlerDecl {
    pub on: String, // event type, glob ok ("signal.*")
    pub run: String, // executable path relative to the skill dir
    #[serde(default = "default_order")]
    pub order: u32,
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

use crate::manifest::ThrottleDecl;
use crate::paths::Root;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// profile.toml — one file, whole identity: skill visibility, throttles,
/// sandbox policy, model selection, template vars.
#[derive(Debug, Deserialize, Default)]
pub struct Profile {
    #[serde(default)]
    pub model: ModelCfg,
    #[serde(default)]
    pub skills: SkillsCfg,
    #[serde(default)]
    pub throttle: BTreeMap<String, ThrottleDecl>,
    #[serde(default)]
    pub sandbox: SandboxCfg,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ModelCfg {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// SDK-style base URL override for Anthropic-compatible providers
    /// (e.g. "https://api.deepseek.com/anthropic"). Falls back to the
    /// ANTHROPIC_BASE_URL env var when the model resolves to the Anthropic
    /// adapter — same semantics as Anthropic's own SDK.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Env var holding the API key, when it isn't the adapter's default.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl Default for ModelCfg {
    fn default() -> Self {
        ModelCfg {
            model: default_model(),
            max_turns: default_max_turns(),
            base_url: None,
            api_key_env: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SkillsCfg {
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for SkillsCfg {
    fn default() -> Self {
        SkillsCfg {
            include: default_include(),
            exclude: vec![],
        }
    }
}

/// The whole-agent fs grant (docs/sandbox.md). `fs_write` prefixes are the
/// cage: when nonempty, the shell tool's process tree can only write inside
/// them (plus the harness root, system temp, and /dev — the harness must not
/// cage itself out of its own ledger). Empty = no cage. The camera (boundary
/// diff → fs/ events) runs either way, over root + fs_write.
/// This lives in the profile until the grants ledger lands in migration
/// step 5; it then hoists into the approval ledger with package grants.
#[derive(Debug, Deserialize, Default)]
pub struct SandboxCfg {
    #[serde(default)]
    pub fs_write: Vec<String>, // absolute, or relative to the harness root
    /// Camera exclusions, prefix-matched against root-relative paths.
    /// Exclusion is never silent: deltas carry the active patterns.
    #[serde(default = "default_capture_exclude")]
    pub capture_exclude: Vec<String>,
}

fn default_capture_exclude() -> Vec<String> {
    // Kernel churn (db/wal/trace/run) would self-noise every diff.
    ["harness.db", "trace.jsonl", "run/", ".env", ".git/", "target/", "node_modules/"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_max_turns() -> u32 {
    24
}
fn default_include() -> Vec<String> {
    vec!["*".into()]
}

pub fn load(root: &Root, name: &str) -> Result<(Profile, PathBuf)> {
    let dir = root.profile_dir(name);
    let f = dir.join("profile.toml");
    if !f.exists() {
        return Ok((Profile::default(), dir));
    }
    let s = std::fs::read_to_string(&f)?;
    let p: Profile = toml::from_str(&s).with_context(|| format!("parsing {}", f.display()))?;
    Ok((p, dir))
}

/// Skill names are single-level topics; include/exclude use the same MQTT
/// filter language as everything else ("#" = all).
pub fn skill_visible(p: &Profile, skill: &str) -> bool {
    let hit = |pats: &[String]| pats.iter().any(|pat| crate::topic::matches(pat, skill));
    hit(&p.skills.include) && !hit(&p.skills.exclude)
}

use crate::manifest::ThrottleDecl;
use crate::paths::Root;
use anyhow::{Context, Result};
use globset::Glob;
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
    #[allow(dead_code)]
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

#[derive(Debug, Deserialize, Default)]
pub struct SandboxCfg {
    /// MVP: "vm" only — the box is the boundary. Parsed now so profiles can
    /// declare intent; enforcement lands later.
    #[serde(default)]
    #[allow(dead_code)]
    pub preset: Option<String>,
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

pub fn skill_visible(p: &Profile, skill: &str) -> bool {
    let matches = |pats: &[String]| {
        pats.iter().any(|pat| {
            Glob::new(pat)
                .map(|g| g.compile_matcher().is_match(skill))
                .unwrap_or(false)
        })
    };
    matches(&p.skills.include) && !matches(&p.skills.exclude)
}

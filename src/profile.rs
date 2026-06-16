use crate::manifest::ThrottleDecl;
use crate::paths::Root;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// profile.toml — one file, whole identity: skill visibility, throttles,
/// sandbox policy, model selection, template vars.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// The agent noun (docs/topics.md decided item 6): this profile's agent
    /// mailbox is in/agent/<agent> and its telemetry lands under
    /// obs/agent/<agent>/<session>/...
    #[serde(default = "default_agent")]
    pub agent: String,
    /// The human owner's noun: asks address in/human/<owner>.
    #[serde(default = "default_owner")]
    pub owner: String,
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
    /// Ordered package search path; relative entries resolve against the
    /// root. First hit wins by name (systemd unit load path semantics).
    /// ELANUS_PACKAGE_PATH overrides.
    #[serde(default = "default_package_path")]
    pub package_path: Vec<String>,
    /// How much of this agent's CONFIGURATION PROPOSALS auto-accept (docs/config.md
    /// D4). "off" (default) = the agent gets no config-proposal machinery at all
    /// (least privilege; most agents never manage config). The other levels give
    /// it a config clone to edit and differ only in what merges without a human:
    /// "manual" (every proposal waits), "assisted" (only diffs whose changed keys
    /// the package marks agent-tunable), "autonomous" (any settings diff except a
    /// protected/stdlib package). The agent only ever PROPOSES regardless.
    #[serde(default = "default_autonomy")]
    pub autonomy: String,
}

fn default_package_path() -> Vec<String> {
    vec!["packages".into()]
}

fn default_autonomy() -> String {
    "off".into()
}

fn default_agent() -> String {
    "main".into()
}

fn default_owner() -> String {
    "owner".into()
}

// Manual Default so a missing profile.toml behaves exactly like an empty
// one: derive(Default) would zero the serde field defaults (empty
// package_path = no discovery at all).
impl Default for Profile {
    fn default() -> Self {
        Profile {
            agent: default_agent(),
            owner: default_owner(),
            model: ModelCfg::default(),
            skills: SkillsCfg::default(),
            throttle: BTreeMap::new(),
            sandbox: SandboxCfg::default(),
            vars: BTreeMap::new(),
            package_path: default_package_path(),
            autonomy: default_autonomy(),
        }
    }
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
/// diff → obs/fs/ events) runs either way, over root + fs_write.
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
    /// Working directory for the shell tool's subprocess spawns. LOCATION,
    /// not authority: writes still flow through the whole-agent grant +
    /// leases exactly as without it; reads are already open. Absolute path
    /// (~ expands); if it doesn't exist the tool call fails with a clear
    /// error rather than silently falling back to the harness root.
    #[serde(default)]
    pub workdir: Option<String>,
}

fn default_capture_exclude() -> Vec<String> {
    // Kernel churn (db/wal/trace/run) would self-noise every diff. "elanus.db"
    // prefix-excludes its -wal/-shm siblings too.
    ["elanus.db", "trace.jsonl", "run/", ".env", ".git/", "target/", "node_modules/"]
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
    // "#" is match-all in the MQTT filter language this field speaks
    // ("skill_visible" doc below). "*" — the old value — is a literal level
    // there, so a profile without [skills] silently saw NOTHING: no skills
    // inventory, no providers, no stages. Found by e2e 14(e).
    vec!["#".into()]
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

/// Mailbox topics derived from the "default" profile, cached per process.
/// Ledger plumbing (dispatcher tick, CLI inbox/answer) needs the v3 nouns
/// without threading a Profile through every call; a process serves one root,
/// and changes pick up on restart — same semantics as the recorder.
pub struct Mailboxes {
    /// in/agent/<agent> — where answers (and other agent mail) go.
    pub agent: String,
    /// in/human/<owner> — where asks go.
    pub human: String,
}

pub fn mailboxes(root: &Root) -> &'static Mailboxes {
    static MAILBOXES: std::sync::OnceLock<Mailboxes> = std::sync::OnceLock::new();
    MAILBOXES.get_or_init(|| {
        let p = load(root, "default").map(|(p, _)| p).unwrap_or_default();
        Mailboxes {
            agent: crate::topic::agent_mailbox(&p.agent),
            human: crate::topic::human_mailbox(&p.owner),
        }
    })
}

/// Skill names are single-level topics; include/exclude use the same MQTT
/// filter language as everything else ("#" = all).
pub fn skill_visible(p: &Profile, skill: &str) -> bool {
    let hit = |pats: &[String]| pats.iter().any(|pat| crate::topic::matches(pat, skill));
    hit(&p.skills.include) && !hit(&p.skills.exclude)
}

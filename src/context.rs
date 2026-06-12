//! The context pipeline (docs/context.md): an ordered chain of stages, each
//! a program `Context -> Context` over one typed JSON document. Programs
//! decide content; the kernel guarantees the wire — the document is seeded
//! from the transcript truth, transformed by approved package stages, then
//! validated (tool_result adjacency, role shape) before it becomes a
//! provider request. Fail closed: a broken stage fails the run with a
//! stage-attributed error, never a silent skip.

use crate::packages;
use crate::paths::Root;
use crate::profile::{self, Profile};
use anyhow::{bail, Context as _, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
        self.system.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n\n")
    }
}

/// One resolved stage in the effective chain.
pub struct StageRef {
    pub package: String,
    pub name: String,
    pub script: PathBuf,
    pub order: u32,
    pub mode: String,
    pub approved: bool,
}

/// The effective chain for a profile: every [[stage]] from a visible
/// package, in the deterministic total order (order, package, stage name).
/// Approval is checked under the FRESHLY loaded manifest hash — the bytes
/// about to run — same pin as exec handlers (docs/security.md entry 9).
pub fn chain(root: &Root, conn: &Connection, prof: &Profile) -> Result<Vec<StageRef>> {
    let mut out: Vec<StageRef> = Vec::new();
    for pkg in packages::discover(root)? {
        if !profile::skill_visible(prof, &pkg.name) {
            continue;
        }
        let Some(lm) = &pkg.manifest else { continue };
        for s in &lm.manifest.stage {
            let approved = packages::approved_under(conn, &pkg.name, &lm.hash, "stage")?
                .iter()
                .any(|v| v == &s.name);
            out.push(StageRef {
                package: pkg.name.clone(),
                name: s.name.clone(),
                script: pkg.dir.join(&s.run),
                order: s.order,
                mode: s.mode.clone(),
                approved,
            });
        }
    }
    out.sort_by(|a, b| {
        (a.order, &a.package, &a.name).cmp(&(b.order, &b.package, &b.name))
    });
    Ok(out)
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
    let mut doc = Doc {
        v: 1,
        system: system_seed
            .iter()
            .map(|(name, text)| Block { name: name.clone(), text: text.clone() })
            .collect(),
        messages,
        event,
        meta,
    };
    validate(&doc).context("seed transcript invalid (kernel bug)")?;
    for s in stages {
        if !s.approved {
            // Fail-closed would brick the agent on every newly-linked kit;
            // an unapproved stage is a REQUEST, and requests are inert by
            // doctrine (discovery is not authority). Loud, observable skip.
            eprintln!("[context] stage {}/{} requested but not approved — skipped", s.package, s.name);
            crate::trace::write(
                root,
                &obs_topic(&doc.meta, &format!("{}-skipped", s.name)),
                ids,
                json!({ "package": s.package, "stage": s.name, "reason": "not approved" }),
            );
            continue;
        }
        let before = (doc.system.len(), doc.messages.len(), doc_bytes(&doc));
        doc = run_stage(root, s, &doc)
            .with_context(|| format!("stage {}/{} failed", s.package, s.name))?;
        if doc.v != 1 {
            bail!("stage {}/{}: unsupported document version {}", s.package, s.name, doc.v);
        }
        validate(&doc)
            .with_context(|| format!("stage {}/{} broke a wire invariant", s.package, s.name))?;
        // The camera doctrine on context assembly: deltas, never the doc.
        crate::trace::write(
            root,
            &obs_topic(&doc.meta, &s.name),
            ids,
            json!({
                "package": s.package,
                "system_blocks": { "before": before.0, "after": doc.system.len() },
                "messages": { "before": before.1, "after": doc.messages.len() },
                "bytes": { "before": before.2, "after": doc_bytes(&doc) },
                "block_names": doc.system.iter().map(|b| b.name.clone()).collect::<Vec<_>>(),
            }),
        );
    }
    Ok(doc)
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
        "resident" => bail!(
            "resident stages are not wired yet (docs/context.md) — declare mode = \"exec\""
        ),
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
        .env("HARNESS_ROOT", &root.dir)
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = w.join();
            let out = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            if !status.success() {
                bail!("exited {:?}", status.code());
            }
            return serde_json::from_str(&out).context("stage stdout is not a context document");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after 10s");
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
                    bail!("message {i}: assistant turn while tool calls {pending:?} are unanswered");
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
        Meta { profile: "default".into(), agent: "main".into(), session: "s".into(), turn: 1, model: "m".into() }
    }

    fn doc(messages: Vec<Value>) -> Doc {
        Doc { v: 1, system: vec![], messages, event: Value::Null, meta: meta() }
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
    fn windowing_by_dropping_leading_turns_is_valid() {
        // Permission (c): drop complete leading turns.
        let d = doc(vec![
            json!({"role":"user","text":"later question"}),
            json!({"role":"assistant","text":"answer"}),
        ]);
        validate(&d).unwrap();
    }
}

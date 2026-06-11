//! The hook plane: blocking interception at fixed points (docs/bus.md).
//! Exec hooks only for now — fork/exec with the subject JSON on stdin; exit
//! 0 = allow (nonempty JSON-object stdout = rewritten subject), nonzero =
//! deny. Resident (bus-registered) hooks arrive with the bus.
//!
//! Failure semantics are the point of this plane: every registration declares
//! `on_timeout` (which also covers spawn errors) because fail-open vs
//! fail-closed is a security decision. Every invocation echoes to
//! obs/harness/hook/<point>/<outcome> on the flight recorder.

use crate::paths::Root;
use crate::trace;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What the chain decided. `subject` is the (possibly rewritten) input
/// object; callers extract their mutable field from it.
pub struct Decision {
    pub allow: bool,
    pub subject: Value,
    pub denied_by: Option<String>,
    pub reason: Option<String>,
}

struct HookRow {
    skill: String,
    run: String,
    timeout_ms: u64,
    on_timeout: String,
    match_filter: String,
}

/// Run the hook chain for `point`. `matched` is what registrations filter
/// on: the tool name for {pre,post}_tool_call, the event topic for
/// pre_dispatch. The chain runs ordered by (ord, id); the first deny stops
/// it; each allow's rewrite feeds the next hook.
pub fn run_chain(
    root: &Root,
    conn: &Connection,
    point: &str,
    matched: &str,
    mut subject: Value,
    ids: &trace::Ids,
) -> Result<Decision> {
    let rows: Vec<HookRow> = {
        let mut stmt = conn.prepare(
            "SELECT skill, run, timeout_ms, on_timeout, match_filter
             FROM hooks WHERE point = ?1 ORDER BY ord, id",
        )?;
        let r = stmt
            .query_map([point], |r| {
                Ok(HookRow {
                    skill: r.get(0)?,
                    run: r.get(1)?,
                    timeout_ms: r.get::<_, i64>(2)? as u64,
                    on_timeout: r.get(3)?,
                    match_filter: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for h in rows {
        if !crate::topic::matches(&h.match_filter, matched) {
            continue;
        }
        let started = Instant::now();
        let outcome = invoke(root, &h, &subject);
        let ms = started.elapsed().as_millis() as u64;
        let (effect, detail, rewrite) = settle(&h, outcome);
        trace::write(
            root,
            &format!("obs/harness/hook/{point}/{}", if effect { "allow" } else { "deny" }),
            ids,
            json!({
                "hook": format!("{}:{}", h.skill, h.run),
                "matched": matched,
                "ms": ms,
                "detail": detail,
            }),
        );
        if !effect {
            return Ok(Decision {
                allow: false,
                subject,
                denied_by: Some(format!("{}:{}", h.skill, h.run)),
                reason: detail["reason"].as_str().map(String::from),
            });
        }
        if let Some(v) = rewrite {
            subject = v;
        }
    }
    Ok(Decision { allow: true, subject, denied_by: None, reason: None })
}

enum Invoked {
    /// exit 0; stdout if any
    Allowed(String),
    /// nonzero exit; (code, stdout+stderr)
    Denied(i32, String),
    Timeout,
    SpawnError(String),
}

fn invoke(root: &Root, h: &HookRow, subject: &Value) -> Invoked {
    use std::os::unix::process::CommandExt as _;
    let mut c = Command::new(root.dir.join(&h.run));
    c.current_dir(&root.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HARNESS_ROOT", &root.dir)
        .env("HARNESS_DB", root.db())
        .env("HARNESS_TRACE", root.trace_file());
    // Own process group so a timeout kills the whole tree, like run_shell.
    c.process_group(0);
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => return Invoked::SpawnError(e.to_string()),
    };
    let pid = child.id() as i32;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(subject.to_string().as_bytes());
        // dropped here: EOF, so hooks reading stdin to end don't hang
    }
    let out_h = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let err_h = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let deadline = Instant::now() + Duration::from_millis(h.timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) | Err(_) => {
                if Instant::now() > deadline {
                    unsafe {
                        libc::killpg(pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let stdout = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let stderr = err_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    match status {
        None => Invoked::Timeout,
        Some(s) if s.success() => Invoked::Allowed(stdout),
        Some(s) => {
            let mut msg = stdout;
            if !stderr.is_empty() {
                if !msg.is_empty() {
                    msg.push('\n');
                }
                msg.push_str(&stderr);
            }
            Invoked::Denied(s.code().unwrap_or(-1), msg)
        }
    }
}

/// Map an invocation to (effect, trace detail, rewrite). A rewrite must be a
/// JSON object on stdout; anything else on stdout is ignored (hooks may print
/// debug noise) but recorded.
fn settle(h: &HookRow, inv: Invoked) -> (bool, Value, Option<Value>) {
    match inv {
        Invoked::Allowed(out) => {
            let trimmed = out.trim();
            if trimmed.is_empty() {
                (true, json!({ "mode": "ok" }), None)
            } else {
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(v) if v.is_object() => (true, json!({ "mode": "rewrite" }), Some(v)),
                    _ => (
                        true,
                        json!({ "mode": "ok", "stdout_ignored": trace::clip(trimmed, 256) }),
                        None,
                    ),
                }
            }
        }
        Invoked::Denied(code, msg) => (
            false,
            json!({ "mode": "exit", "code": code, "reason": trace::clip(msg.trim(), 1024) }),
            None,
        ),
        Invoked::Timeout => {
            let allow = h.on_timeout == "allow";
            (
                allow,
                json!({ "mode": "timeout", "on_timeout": h.on_timeout,
                        "reason": format!("hook timed out after {}ms", h.timeout_ms) }),
                None,
            )
        }
        Invoked::SpawnError(e) => {
            let allow = h.on_timeout == "allow";
            (
                allow,
                json!({ "mode": "spawn_error", "on_timeout": h.on_timeout,
                        "reason": format!("hook failed to spawn: {e}") }),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn test_root() -> Root {
        let dir = std::env::temp_dir().join(format!("elanus-hooks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    fn write_hook(root: &Root, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = root.dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).unwrap();
        name.to_string()
    }

    fn register(
        conn: &Connection,
        point: &str,
        run: &str,
        ord: u32,
        timeout_ms: u64,
        on_timeout: &str,
        mf: &str,
    ) {
        conn.execute(
            "INSERT INTO hooks(skill, point, run, ord, timeout_ms, on_timeout, match_filter)
             VALUES ('test', ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![point, run, ord, timeout_ms as i64, on_timeout, mf],
        )
        .unwrap();
    }

    fn setup() -> (Root, Connection) {
        let root = test_root();
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        (root, conn)
    }

    #[test]
    fn empty_chain_allows() {
        let (root, conn) = setup();
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({"a":1}), &trace::Ids::default()).unwrap();
        assert!(d.allow);
        assert_eq!(d.subject, json!({"a":1}));
    }

    #[test]
    fn deny_stops_chain_with_reason() {
        let (root, conn) = setup();
        let h1 = write_hook(&root, "deny.sh", "echo 'nope: policy says no'; exit 1");
        let h2 = write_hook(&root, "never.sh", "echo '{\"x\":2}'");
        register(&conn, "pre_tool_call", &h1, 10, 1000, "deny", "#");
        register(&conn, "pre_tool_call", &h2, 20, 1000, "deny", "#");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({"x":1}), &trace::Ids::default()).unwrap();
        assert!(!d.allow);
        assert_eq!(d.denied_by.as_deref(), Some(&*format!("test:{h1}")));
        assert!(d.reason.unwrap().contains("policy says no"));
        assert_eq!(d.subject, json!({"x":1})); // unrewritten
    }

    #[test]
    fn rewrite_feeds_next_hook() {
        let (root, conn) = setup();
        // First hook rewrites x to 2; second asserts it sees the rewrite.
        let h1 = write_hook(&root, "rw.sh", "cat >/dev/null; echo '{\"x\":2}'");
        let h2 = write_hook(
            &root,
            "check.sh",
            "grep -q '\"x\":2' || exit 1",
        );
        register(&conn, "pre_tool_call", &h1, 10, 1000, "deny", "#");
        register(&conn, "pre_tool_call", &h2, 20, 1000, "deny", "#");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({"x":1}), &trace::Ids::default()).unwrap();
        assert!(d.allow);
        assert_eq!(d.subject, json!({"x":2}));
    }

    #[test]
    fn timeout_respects_declaration() {
        let (root, conn) = setup();
        let slow = write_hook(&root, "slow.sh", "sleep 5");
        register(&conn, "pre_tool_call", &slow, 10, 100, "deny", "#");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({}), &trace::Ids::default()).unwrap();
        assert!(!d.allow, "on_timeout=deny must deny");

        let (root2, conn2) = setup();
        let slow2 = write_hook(&root2, "slow.sh", "sleep 5");
        register(&conn2, "pre_tool_call", &slow2, 10, 100, "allow", "#");
        let d = run_chain(&root2, &conn2, "pre_tool_call", "shell", json!({}), &trace::Ids::default()).unwrap();
        assert!(d.allow, "on_timeout=allow must allow");
    }

    #[test]
    fn spawn_error_respects_declaration() {
        let (root, conn) = setup();
        register(&conn, "pre_tool_call", "does/not/exist", 10, 1000, "deny", "#");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({}), &trace::Ids::default()).unwrap();
        assert!(!d.allow);
    }

    #[test]
    fn match_filter_scopes_hook() {
        let (root, conn) = setup();
        let deny = write_hook(&root, "deny.sh", "exit 1");
        register(&conn, "pre_tool_call", &deny, 10, 1000, "deny", "shell");
        let d = run_chain(&root, &conn, "pre_tool_call", "emit_event", json!({}), &trace::Ids::default()).unwrap();
        assert!(d.allow, "filter 'shell' must not match tool 'emit_event'");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({}), &trace::Ids::default()).unwrap();
        assert!(!d.allow);
    }

    #[test]
    fn debug_noise_is_not_a_rewrite() {
        let (root, conn) = setup();
        let noisy = write_hook(&root, "noisy.sh", "echo 'just logging something'");
        register(&conn, "pre_tool_call", &noisy, 10, 1000, "deny", "#");
        let d = run_chain(&root, &conn, "pre_tool_call", "shell", json!({"k":"v"}), &trace::Ids::default()).unwrap();
        assert!(d.allow);
        assert_eq!(d.subject, json!({"k":"v"}));
    }
}

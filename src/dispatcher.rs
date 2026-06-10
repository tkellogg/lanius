use crate::db;
use crate::events::{self, EmitOpts};
use crate::paths::Root;
use crate::profile;
use crate::skills;
use crate::trace;
use anyhow::Result;
use chrono::{DateTime, Utc};
use globset::Glob;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr as _;
use std::time::Duration;

/// One spawned handler process being supervised.
struct Running {
    child: Child,
    dispatch_id: i64,
    event_id: i64,
    etype: String,
    correlation: Option<String>,
    out_path: PathBuf,
    err_path: PathBuf,
}

/// The dispatcher does *nothing* but: notice pending events, match type to
/// handlers, check throttles, fork/exec, record exits, write trace lines.
/// It is a supervisor, not a doer.
pub fn run(root: &Root, interval_ms: u64) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    merge_profile_throttles(root, &conn);
    std::fs::create_dir_all(root.run_dir())?;
    // Orphaned 'running' rows from a previous daemon are unrecoverable: the
    // children died with that process. Mark them failed; replay is the cure.
    conn.execute(
        "UPDATE dispatches SET state='failed', exit_code=-3, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE state='running'",
        [],
    )?;
    conn.execute("UPDATE events SET state='pending' WHERE state='running'", [])?;
    eprintln!(
        "[daemon] root={} interval={}ms (let-it-crash; ctrl-c to stop)",
        root.dir.display(),
        interval_ms
    );
    let mut running: Vec<Running> = Vec::new();
    loop {
        if let Err(e) = tick(root, &conn, &mut running) {
            eprintln!("[daemon] tick error: {e:#}");
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

fn tick(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    tick_crons(root, conn)?;
    expire_deadlines(root, conn)?;
    reap(root, conn, running)?;
    resume_suspended(root, conn, running)?;
    dispatch_pending(root, conn, running)?;
    Ok(())
}

fn tick_crons(root: &Root, conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare("SELECT id, schedule, emit_type, payload, last_fired FROM crons")?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let now = Utc::now();
    for (id, schedule, emit_type, payload, last_fired) in rows {
        let Ok(cron) = croner::Cron::from_str(&schedule) else {
            continue;
        };
        match last_fired {
            None => {
                // Arm on first sight; don't fire for the past.
                conn.execute("UPDATE crons SET last_fired = ?1 WHERE id = ?2", params![trace::now_iso(), id])?;
            }
            Some(lf) => {
                let Ok(lf_dt) = DateTime::parse_from_rfc3339(&lf) else {
                    continue;
                };
                let lf_utc = lf_dt.with_timezone(&Utc);
                if let Ok(next) = cron.find_next_occurrence(&lf_utc, false) {
                    if next <= now {
                        events::emit(
                            root,
                            conn,
                            EmitOpts {
                                payload: payload.as_deref().and_then(|s| serde_json::from_str(s).ok()),
                                // Dedupes the same scheduled firing across daemon restarts.
                                idempotency: Some(format!("cron:{}:{}", id, next.to_rfc3339())),
                                ..EmitOpts::new(&emit_type)
                            },
                        )?;
                        conn.execute("UPDATE crons SET last_fired = ?1 WHERE id = ?2", params![trace::now_iso(), id])?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Defaults are the big unblock: an expired ask executes its default and logs
/// the assumption as an ordinary human.answer event — auditable, vetoable.
fn expire_deadlines(root: &Root, conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.correlation_id, e.default_action FROM events e
             WHERE e.type = 'human.ask' AND e.deadline IS NOT NULL
               AND e.state != 'expired' AND e.correlation_id IS NOT NULL
               AND e.deadline < strftime('%Y-%m-%dT%H:%M:%fZ','now')
               AND NOT EXISTS (SELECT 1 FROM events a
                               WHERE a.type = 'human.answer' AND a.correlation_id = e.correlation_id)",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (ask_id, corr, default_action) in rows {
        let default: Value = default_action
            .as_deref()
            .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.to_string())))
            .unwrap_or(Value::Null);
        events::emit(
            root,
            conn,
            EmitOpts {
                payload: Some(json!({ "answer": default, "assumed": true })),
                correlation: Some(corr.clone()),
                cause: Some(ask_id),
                ..EmitOpts::new("human.answer")
            },
        )?;
        conn.execute(
            "UPDATE events SET state='expired', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            [ask_id],
        )?;
        trace::write(
            root,
            "expire",
            &trace::Ids { event_id: Some(ask_id), correlation_id: Some(corr), ..Default::default() },
            json!({ "assumed_default": default }),
        );
    }
    Ok(())
}

fn reap(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let mut i = 0;
    while i < running.len() {
        match running[i].child.try_wait() {
            Ok(Some(status)) => {
                let r = running.swap_remove(i);
                let code = status.code().unwrap_or(-1);
                finish_dispatch(root, conn, &r, code)?;
            }
            Ok(None) => i += 1,
            Err(e) => {
                eprintln!("[daemon] wait error: {e}");
                i += 1;
            }
        }
    }
    Ok(())
}

fn finish_dispatch(root: &Root, conn: &Connection, r: &Running, code: i32) -> Result<()> {
    let stdout = read_clipped(&r.out_path);
    let stderr = read_clipped(&r.err_path);
    let mut dstate = match code {
        0 => "done",
        75 => "suspended", // EX_TEMPFAIL: checkpointed itself; resume via correlation_id
        _ => "failed",
    };
    let mut resume_correlation: Option<String> = None;
    if dstate == "suspended" {
        // The suspend contract: before exiting 75 the handler emitted a
        // human.ask caused by this event. Its correlation_id is the resume key.
        resume_correlation = conn
            .query_row(
                "SELECT correlation_id FROM events
                 WHERE cause_id = ?1 AND type = 'human.ask' AND correlation_id IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
                [r.event_id],
                |row| row.get(0),
            )
            .optional()?;
        if resume_correlation.is_none() {
            // Suspended with nothing to wake it: that's a failure, loudly.
            dstate = "failed";
        }
    }
    conn.execute(
        "UPDATE dispatches SET state=?1, exit_code=?2, resume_correlation=?3,
         finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?4",
        params![dstate, code, resume_correlation, r.dispatch_id],
    )?;
    trace::write(
        root,
        "handler.exit",
        &trace::Ids {
            event_id: Some(r.event_id),
            correlation_id: r.correlation.clone(),
            ..Default::default()
        },
        json!({
            "handler": handler_name(&r.out_path, conn, r.dispatch_id),
            "dispatch_id": r.dispatch_id,
            "exit_code": code,
            "state": dstate,
            "stdout": stdout,
            "stderr": stderr,
        }),
    );
    recompute_event_state(conn, r.event_id)?;
    Ok(())
}

fn handler_name(_out: &PathBuf, conn: &Connection, dispatch_id: i64) -> String {
    conn.query_row("SELECT handler FROM dispatches WHERE id=?1", [dispatch_id], |r| r.get(0))
        .unwrap_or_else(|_| "?".into())
}

fn recompute_event_state(conn: &Connection, event_id: i64) -> Result<()> {
    let (n_running, n_suspended, n_failed): (i64, i64, i64) = conn.query_row(
        "SELECT
           SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),
           SUM(CASE WHEN state='suspended' THEN 1 ELSE 0 END),
           SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END)
         FROM dispatches WHERE event_id=?1",
        [event_id],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    let state = if n_running > 0 {
        "running"
    } else if n_suspended > 0 {
        "waiting_on_human"
    } else if n_failed > 0 {
        "failed"
    } else {
        "done"
    };
    if state == "running" || state == "waiting_on_human" {
        conn.execute("UPDATE events SET state=?1 WHERE id=?2", params![state, event_id])?;
    } else {
        conn.execute(
            "UPDATE events SET state=?1, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2",
            params![state, event_id],
        )?;
    }
    Ok(())
}

/// A suspended handler whose resume correlation now has a human.answer gets
/// re-invoked with the original event plus the answer. Only that causality
/// chain parked; everything else kept flowing.
fn resume_suspended(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let rows: Vec<(i64, i64, String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.event_id, d.handler, d.resume_correlation,
                    (SELECT a.id FROM events a
                     WHERE a.type='human.answer' AND a.correlation_id = d.resume_correlation
                     ORDER BY a.id LIMIT 1) AS answer_id
             FROM dispatches d
             WHERE d.state='suspended'
               AND EXISTS (SELECT 1 FROM events a
                           WHERE a.type='human.answer' AND a.correlation_id = d.resume_correlation)",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (dispatch_id, event_id, handler, corr, answer_id) in rows {
        let mut envelope = events::envelope(conn, event_id)?;
        envelope["resume"] = events::envelope(conn, answer_id)?;
        conn.execute(
            "UPDATE dispatches SET state='running', exit_code=NULL, finished_at=NULL WHERE id=?1",
            [dispatch_id],
        )?;
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [event_id])?;
        spawn_handler(
            root,
            conn,
            running,
            event_id,
            &envelope,
            PathBuf::from(&handler),
            Some(dispatch_id),
            Some(corr),
        )?;
    }
    Ok(())
}

fn dispatch_pending(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let pending: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id FROM events WHERE state='pending'
             ORDER BY priority DESC, id ASC LIMIT 100",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, etype, corr) in pending {
        let handlers = skills::matching_handlers(root, &etype)?;
        if handlers.is_empty() {
            // No consumers: the event just lives in the log.
            conn.execute(
                "UPDATE events SET state='done', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [id],
            )?;
            continue;
        }
        if is_throttled(conn, &etype, running)? {
            continue; // stays pending; revisited next tick
        }
        let envelope = events::envelope(conn, id)?;
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [id])?;
        for h in handlers {
            spawn_handler(root, conn, running, id, &envelope, h, None, corr.clone())?;
        }
    }
    Ok(())
}

fn is_throttled(conn: &Connection, etype: &str, running: &[Running]) -> Result<bool> {
    let rows: Vec<(String, Option<i64>, Option<i64>, i64)> = {
        let mut stmt =
            conn.prepare("SELECT event_type, max_concurrent, rate_per_min, coalesce FROM throttles")?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let mut blocked = false;
    for (pat, max_concurrent, rate_per_min, coalesce) in rows {
        if !glob_match(&pat, etype) {
            continue;
        }
        // The algedonic exemption: never queued behind, never batched.
        if coalesce == 0 {
            return Ok(false);
        }
        if let Some(maxc) = max_concurrent {
            let n = running.iter().filter(|r| glob_match(&pat, &r.etype)).count() as i64;
            if n >= maxc {
                blocked = true;
            }
        }
        if let Some(rate) = rate_per_min {
            let types: Vec<String> = {
                let mut stmt = conn.prepare(
                    "SELECT e.type FROM dispatches d JOIN events e ON e.id = d.event_id
                     WHERE d.started_at > strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds')",
                )?;
                let r = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                r
            };
            if types.iter().filter(|t| glob_match(&pat, t)).count() as i64 >= rate {
                blocked = true;
            }
        }
    }
    Ok(blocked)
}

pub fn glob_match(pat: &str, s: &str) -> bool {
    if pat == s {
        return true;
    }
    Glob::new(pat)
        .map(|g| g.compile_matcher().is_match(s))
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn spawn_handler(
    root: &Root,
    conn: &Connection,
    running: &mut Vec<Running>,
    event_id: i64,
    envelope: &Value,
    handler: PathBuf,
    reuse_dispatch: Option<i64>,
    correlation: Option<String>,
) -> Result<()> {
    let dispatch_id = match reuse_dispatch {
        Some(d) => d,
        None => {
            conn.execute(
                "INSERT INTO dispatches(event_id, handler) VALUES (?1, ?2)",
                params![event_id, handler.display().to_string()],
            )?;
            conn.last_insert_rowid()
        }
    };
    let out_path = root.run_dir().join(format!("d{dispatch_id}.out"));
    let err_path = root.run_dir().join(format!("d{dispatch_id}.err"));
    let out_f = std::fs::File::create(&out_path)?;
    let err_f = std::fs::File::create(&err_path)?;

    // Handlers call back into `harness`; make sure this binary wins on PATH.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let path_env = format!(
        "{}:{}",
        exe_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let etype = envelope["type"].as_str().unwrap_or("?").to_string();
    let is_resume = envelope.get("resume").map(|v| !v.is_null()).unwrap_or(false);
    let mut cmd = Command::new(&handler);
    cmd.current_dir(&root.dir)
        .stdin(Stdio::piped())
        .stdout(out_f)
        .stderr(err_f)
        .env("HARNESS_EVENT_ID", event_id.to_string())
        .env("HARNESS_DB", root.db())
        .env("HARNESS_TRACE", root.trace_file())
        .env("HARNESS_ROOT", &root.dir)
        .env("HARNESS_PROFILE", root.profile_dir("default"))
        .env("PATH", path_env);
    if let Some(c) = envelope["cause_id"].as_i64() {
        cmd.env("HARNESS_CAUSE_ID", c.to_string());
    }
    if let Some(c) = &correlation {
        cmd.env("HARNESS_CORRELATION_ID", c);
    }
    if is_resume {
        cmd.env("HARNESS_RESUME", "1");
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write as _;
                // EPIPE from an instantly-exiting handler is fine.
                let _ = stdin.write_all(envelope.to_string().as_bytes());
            }
            trace::write(
                root,
                "dispatch",
                &trace::Ids {
                    event_id: Some(event_id),
                    cause_id: envelope["cause_id"].as_i64(),
                    correlation_id: correlation.clone(),
                    ..Default::default()
                },
                json!({
                    "handler": handler.display().to_string(),
                    "dispatch_id": dispatch_id,
                    "type": etype,
                    "resume": is_resume,
                }),
            );
            running.push(Running {
                child,
                dispatch_id,
                event_id,
                etype,
                correlation,
                out_path,
                err_path,
            });
        }
        Err(e) => {
            conn.execute(
                "UPDATE dispatches SET state='failed', exit_code=-2,
                 finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [dispatch_id],
            )?;
            trace::write(
                root,
                "handler.exit",
                &trace::Ids { event_id: Some(event_id), ..Default::default() },
                json!({ "handler": handler.display().to_string(), "spawn_error": e.to_string(), "state": "failed" }),
            );
            recompute_event_state(conn, event_id)?;
        }
    }
    Ok(())
}

fn read_clipped(p: &PathBuf) -> String {
    std::fs::read_to_string(p)
        .map(|s| trace::clip(&s, 4096))
        .unwrap_or_default()
}

/// Profile [throttle.*] sections merge into the throttle table at daemon start.
fn merge_profile_throttles(root: &Root, conn: &Connection) {
    let Ok(entries) = std::fs::read_dir(root.profiles()) else { return };
    for e in entries.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().to_string();
        if let Ok((prof, _)) = profile::load(root, &name) {
            for (pat, t) in &prof.throttle {
                if let Err(err) = skills::upsert_throttle(conn, pat, t) {
                    eprintln!("[daemon] throttle merge {pat}: {err:#}");
                }
            }
        }
    }
}

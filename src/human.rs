use crate::db;
use crate::events::{self, EmitOpts};
use crate::paths::Root;
use anyhow::{bail, Result};
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

/// The human's inbox is a view: asks not yet answered, not yet expired.
pub fn inbox(root: &Root, conn: &Connection) -> Result<()> {
    let mb = crate::profile::mailboxes(root);
    let rows: Vec<(i64, Option<String>, Option<String>, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.payload, e.deadline, e.created_at, e.default_action FROM events e
             WHERE e.type=?1 AND e.state != 'expired' AND e.correlation_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM events a
                               WHERE a.type=?2 AND a.correlation_id = e.correlation_id)
             ORDER BY e.priority DESC, e.id ASC",
        )?;
        let r = stmt
            .query_map([&mb.human, &mb.agent], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    if rows.is_empty() {
        println!("inbox zero — nothing is waiting on you");
        return Ok(());
    }
    for (id, payload, deadline, created_at, default_action) in rows {
        let p: Value = payload
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let question = p["question"].as_str().unwrap_or("(no question text)");
        let root_type = db::root_type(conn, id).unwrap_or_else(|_| "?".into());
        println!("#{id}  {question}");
        if let Some(opts) = p["options"].as_array() {
            let opts: Vec<&str> = opts.iter().filter_map(|o| o.as_str()).collect();
            println!("      options: {}", opts.join(" | "));
        }
        if let Some(d) = deadline {
            let def = default_action.unwrap_or_else(|| "null".into());
            println!("      deadline: {d} -> default: {def}");
        }
        println!("      asked: {created_at}  root cause: {root_type}");
        println!("      answer with: lanius answer {id} \"...\"");
    }
    Ok(())
}

pub fn answer(root: &Root, conn: &Connection, ask_id: i64, text: &str) -> Result<()> {
    let mb = crate::profile::mailboxes(root);
    let corr: Option<String> = conn
        .query_row(
            "SELECT correlation_id FROM events WHERE id=?1 AND type=?2",
            rusqlite::params![ask_id, mb.human],
            |r| r.get(0),
        )
        .map_err(|_| anyhow::anyhow!("no ask event ({}) with id {ask_id}", mb.human))?;
    let Some(corr) = corr else {
        bail!("ask {ask_id} has no correlation_id; cannot route an answer to it")
    };
    let already: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type=?1 AND correlation_id=?2",
        [&mb.agent, &corr],
        |r| r.get(0),
    )?;
    if already > 0 {
        bail!("ask {ask_id} already has an answer");
    }
    // The answer is mail to the agent (mailbox model, docs/topics.md):
    // correlation matches it back to the ask flow.
    let id = events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(json!({ "answer": text })),
            correlation: Some(corr),
            cause: Some(ask_id),
            ..EmitOpts::new(&mb.agent)
        },
    )?;
    println!("answered ask #{ask_id} (answer event #{id})");
    Ok(())
}

/// Sugar over emit: every ask gets a correlation_id; deadline + default are
/// how asks stop blocking work.
pub fn ask(
    root: &Root,
    conn: &Connection,
    question: &str,
    options: Option<&str>,
    deadline_minutes: Option<i64>,
    default: Option<&str>,
) -> Result<i64> {
    let mb = crate::profile::mailboxes(root);
    let corr = uuid::Uuid::new_v4().to_string();
    let mut payload = json!({ "question": question });
    if let Some(opts) = options {
        payload["options"] = json!(opts.split(',').map(|s| s.trim()).collect::<Vec<_>>());
    }
    let deadline = deadline_minutes
        .map(|m| (Utc::now() + Duration::minutes(m)).to_rfc3339_opts(SecondsFormat::Millis, true));
    let id = events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(payload),
            correlation: Some(corr.clone()),
            deadline,
            default_action: default.map(|d| Value::String(d.to_string())),
            ..EmitOpts::new(&mb.human)
        },
    )?;
    println!("ask #{id} (correlation {corr})");
    Ok(id)
}

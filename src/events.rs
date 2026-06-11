use crate::paths::Root;
use crate::trace;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub struct EmitOpts {
    pub etype: String,
    pub payload: Option<Value>,
    pub priority: i64,
    pub correlation: Option<String>,
    pub deadline: Option<String>,
    pub default_action: Option<Value>,
    pub idempotency: Option<String>,
    pub cause: Option<i64>,
    /// True only when the caller has already announced (or is about to
    /// announce, atomically with this insert) the event on the bus under its
    /// own topic — today that is exactly the broker's inbound path, which
    /// fans out the materialized event itself. Everything else leaves this
    /// false and the daemon's announce sweep (dispatcher) publishes it.
    /// This flag is what makes "announce exactly once" a row-level fact.
    pub pre_announced: bool,
}

impl EmitOpts {
    pub fn new(etype: &str) -> Self {
        EmitOpts {
            etype: etype.to_string(),
            payload: None,
            priority: 0,
            correlation: None,
            deadline: None,
            default_action: None,
            idempotency: None,
            cause: None,
            pre_announced: false,
        }
    }
}

/// The universal entry point. Threads cause_id from HARNESS_EVENT_ID when the
/// caller doesn't pass one explicitly — causality propagation must be
/// zero-effort or it won't happen.
pub fn emit(root: &Root, conn: &Connection, mut o: EmitOpts) -> Result<i64> {
    if !crate::topic::valid_name(&o.etype) {
        anyhow::bail!("invalid event type {:?}: must be a wildcard-free topic name", o.etype);
    }
    if o.cause.is_none() {
        o.cause = std::env::var("HARNESS_EVENT_ID").ok().and_then(|v| v.parse().ok());
    }
    let dispatch: Option<i64> = std::env::var("HARNESS_DISPATCH_ID").ok().and_then(|v| v.parse().ok());
    // Atomic idempotency: ON CONFLICT DO NOTHING avoids the check-then-insert
    // race where two concurrent emitters of the same key both pass a SELECT.
    let inserted = conn.execute(
        "INSERT INTO events(type, cause_id, correlation_id, payload, priority, deadline, default_action, idempotency_key, emitted_by_dispatch, announced)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(idempotency_key) DO NOTHING",
        params![
            o.etype,
            o.cause,
            o.correlation,
            o.payload.as_ref().map(|v| v.to_string()),
            o.priority,
            o.deadline,
            o.default_action.as_ref().map(|v| v.to_string()),
            o.idempotency,
            dispatch,
            o.pre_announced as i64,
        ],
    )?;
    if inserted == 0 {
        // Only reachable with an idempotency key: return the existing event.
        let key = o.idempotency.as_deref().unwrap_or_default();
        let existing: Option<i64> = conn
            .query_row("SELECT id FROM events WHERE idempotency_key = ?1", [key], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        anyhow::bail!("emit deduped but original event not found (key {key:?})");
    }
    let id = conn.last_insert_rowid();
    trace::write(
        root,
        "obs/harness/ledger/emit",
        &trace::Ids {
            event_id: Some(id),
            cause_id: o.cause,
            correlation_id: o.correlation.clone(),
            session_id: None,
        },
        json!({ "type": o.etype, "payload": o.payload, "priority": o.priority }),
    );
    Ok(id)
}

/// The full event envelope, as handlers receive it on stdin.
pub fn envelope(conn: &Connection, id: i64) -> Result<Value> {
    let v = conn.query_row(
        "SELECT id, type, cause_id, correlation_id, payload, state, priority, deadline, default_action, created_at
         FROM events WHERE id = ?1",
        [id],
        |r| {
            let payload: Option<String> = r.get(4)?;
            let default_action: Option<String> = r.get(8)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "type": r.get::<_, String>(1)?,
                "cause_id": r.get::<_, Option<i64>>(2)?,
                "correlation_id": r.get::<_, Option<String>>(3)?,
                "payload": parse_or_null(payload),
                "state": r.get::<_, String>(5)?,
                "priority": r.get::<_, i64>(6)?,
                "deadline": r.get::<_, Option<String>>(7)?,
                "default_action": parse_or_null(default_action),
                "created_at": r.get::<_, String>(9)?,
            }))
        },
    )?;
    Ok(v)
}

fn parse_or_null(s: Option<String>) -> Value {
    match s {
        Some(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        None => Value::Null,
    }
}

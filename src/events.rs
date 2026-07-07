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
    /// Who the kernel holds responsible for this event (docs/identity.md).
    /// The broker sets it from the authenticated connection for bus-origin
    /// events, overwriting anything the message claimed — that is the
    /// verified, unforgeable case. Left None elsewhere, where emit() falls
    /// back to the emitting process's declared actor (LANIUS_ACTOR) or, for
    /// the kernel's own machinery, "kernel".
    pub sender: Option<String>,
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
            sender: None,
        }
    }
}

/// The universal entry point. Threads cause_id from LANIUS_EVENT_ID when the
/// caller doesn't pass one explicitly — causality propagation must be
/// zero-effort or it won't happen.
pub fn emit(root: &Root, conn: &Connection, mut o: EmitOpts) -> Result<i64> {
    if !crate::topic::valid_name(&o.etype) {
        anyhow::bail!(
            "invalid event type {:?}: must be a wildcard-free topic name",
            o.etype
        );
    }
    if o.cause.is_none() {
        o.cause = crate::envcompat::read("EVENT_ID").and_then(|v| v.parse().ok());
    }
    let dispatch: Option<i64> = crate::envcompat::read("DISPATCH_ID").and_then(|v| v.parse().ok());
    // Provenance (docs/identity.md): a broker-verified sender wins; otherwise
    // the emitting process's declared actor (LANIUS_ACTOR, set by exec for
    // the agent it is running); otherwise the kernel itself. The broker path
    // is the only one that is unforgeable — the others are self-reported until
    // the ledger becomes kernel-only-writable.
    let sender = o
        .sender
        .clone()
        .or_else(|| crate::envcompat::read("ACTOR").filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "kernel".to_string());
    // Atomic idempotency: ON CONFLICT DO NOTHING avoids the check-then-insert
    // race where two concurrent emitters of the same key both pass a SELECT.
    let inserted = conn.execute(
        "INSERT INTO events(type, cause_id, correlation_id, payload, priority, deadline, default_action, idempotency_key, emitted_by_dispatch, announced, sender)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
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
            sender,
        ],
    )?;
    if inserted == 0 {
        // Only reachable with an idempotency key: return the existing event.
        let key = o.idempotency.as_deref().unwrap_or_default();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM events WHERE idempotency_key = ?1",
                [key],
                |r| r.get(0),
            )
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
        "SELECT id, type, cause_id, correlation_id, payload, state, priority, deadline, default_action, created_at, sender
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
                // The kernel-recorded sender (docs/identity.md) travels to the
                // handler so it can act on who sent the event it is handling.
                // NULL on pre-migration rows — treat absent as "unknown".
                "sender": r.get::<_, Option<String>>(10)?,
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

//! `elanus code mail --json` — the human-facing projection of agent-to-agent
//! message traffic (docs/handoffs/agent-comms-ui.md M1). The CLI IS the API: the
//! web server shells it exactly like `elanus code sessions --json`, so the comms
//! surface needs no new transport.
//!
//! This is a PURE LEDGER QUERY — no new bus capture. Agent-to-agent deliveries
//! are already `in/agent/<noun>/<session>` events on `elanus.db` carrying
//! `sender`, `priority`, `state`, and a `correlation_id` (`record_delivery`,
//! src/codeagent.rs). A worker's completion/failure reply rides the SAME
//! correlation (failure-mail is `{failed:true}` on `in/human/<owner>`, see
//! `exec::report_agent_failure`). We project the deliveries newest-first, thread
//! each to its reply by correlation, flag the failed ones, and tag the ones that
//! were handed MID-CYCLE (joined from `code_mail_delivered`, the C3 dedup table).
//!
//! CORRECTNESS (handoff "concerns spotted"):
//!  - the row identity is the EVENT id, so a delivery that Claude Code hands BOTH
//!    mid-cycle AND next-turn is ONE row with a `mid_cycle` tell, never two.
//!  - mid-cycle mail is deliberately NOT marked seen, so a `mid_cycle` row may
//!    still be unread; the UI explains this rather than "fixing" it.

use crate::paths::Root;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

/// One agent-to-agent delivery, threaded to its completion/failure reply.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MailRow {
    /// The ledger event id of the delivery (the row identity — dedup key).
    pub id: i64,
    /// The broker-verified sender (who it is from), if recorded.
    pub from: Option<String>,
    /// The target session the mailbox addressed (decoded from the topic).
    pub to: Option<String>,
    /// The agent noun the target publishes under (decoded from the topic).
    pub to_noun: Option<String>,
    /// The correlation that threads the round trip.
    pub correlation: Option<String>,
    /// The delivery's priority (higher = more urgent; 0 default).
    pub priority: i32,
    /// The delivery's lifecycle state (pending/running/done/failed/…).
    pub state: String,
    /// True when this delivery's correlation carries a `{failed:true}` reply
    /// (the worker run failed) — the silent-failure tell.
    pub failed: bool,
    /// True when this mail was handed to the worker MID-CYCLE (it is in the
    /// `code_mail_delivered` dedup table) — the algedonic "delivered mid-task"
    /// tell. The same event is never double-counted: this is a flag on the one
    /// row, not a second row.
    pub mid_cycle: bool,
    /// A short preview of the message text.
    pub preview: String,
    /// When the delivery was recorded.
    pub ts: String,
}

/// Decode a single percent-encoded topic level back to its raw value (mirrors the
/// private decoder in codeagent.rs; the mailbox segments are `encode_segment`d).
fn decode_segment(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Parse an `in/agent/<noun>/<session>` mailbox topic into `(noun, session)`.
/// `None` for the agent's bare mailbox (`in/agent/<noun>`, three levels) or any
/// other shape — the comms view only threads session-addressed deliveries.
fn parse_mailbox(topic: &str) -> Option<(String, String)> {
    let segs: Vec<&str> = topic.split('/').collect();
    if segs.len() != 4 || segs[0] != "in" || segs[1] != "agent" {
        return None;
    }
    Some((decode_segment(segs[2]), decode_segment(segs[3])))
}

/// Project the recent agent-to-agent mail (newest first, bounded by `limit`).
/// Each delivery is threaded to its reply by correlation; a `{failed:true}`
/// reply on the same correlation flags the delivery `failed`. Mid-cycle handoff
/// is joined from `code_mail_delivered`. A root with no mail returns an empty
/// vector — never an error.
pub fn recent_mail(root: &Root, limit: usize) -> Result<Vec<MailRow>> {
    let conn = crate::db::open(root).context("opening the ledger for the mail projection")?;
    crate::db::init_schema(&conn)?;

    // The deliveries: every `in/agent/%` event, newest first. We bound the scan
    // generously then filter to session-addressed mailboxes in Rust (the topic
    // shape gate is not SQL-cheap). LIKE 'in/agent/%' is index-free but the
    // events table is small and this is a read route, not a hot path.
    let mut stmt = conn.prepare(
        "SELECT id, type, sender, correlation_id, COALESCE(priority,0), state,
                COALESCE(payload,''), created_at
           FROM events
          WHERE type LIKE 'in/agent/%'
          ORDER BY id DESC",
    )?;
    let raw = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i32>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out: Vec<MailRow> = Vec::new();
    for (id, topic, sender, correlation, priority, state, payload, created_at) in raw {
        let Some((noun, session)) = parse_mailbox(&topic) else {
            continue; // a bare agent mailbox or odd shape — not session mail
        };
        // Only thread deliveries addressed to a coding session (`code-*`), the
        // agents this view is about. An ordinary agent's mailbox is the chat
        // seat's territory (/api/conversations), not the comms plane.
        if !session.starts_with("code-") {
            continue;
        }
        let pv: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let preview = crate::codeagent::delivery_message(&pv).unwrap_or_default();
        let preview = clip(&preview, 200);

        let failed = correlation
            .as_deref()
            .map(|c| correlation_failed(&conn, c).unwrap_or(false))
            .unwrap_or(false);
        let mid_cycle = mid_cycle_delivered(&conn, &session, id).unwrap_or(false);

        out.push(MailRow {
            id,
            from: sender,
            to: Some(session),
            to_noun: Some(noun),
            correlation,
            priority,
            state,
            failed,
            mid_cycle,
            preview,
            ts: created_at,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Does this correlation carry a `{failed:true}` reply (a failure-mail)? The
/// failure-mail rides `in/human/<owner>` on the same correlation
/// (`exec::report_agent_failure`); a worker-completion delivery may also carry
/// `failed`. We scan every event on the correlation for the flag.
fn correlation_failed(conn: &rusqlite::Connection, correlation: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(payload,'') FROM events WHERE correlation_id = ?1",
    )?;
    let rows = stmt
        .query_map([correlation], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for payload in rows {
        let pv: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        if pv.get("failed").and_then(Value::as_bool) == Some(true) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Was this event handed to the session mid-cycle (recorded in
/// `code_mail_delivered`)? Keyed by `(session, event_id)`, the immutable mail
/// event — so the tell is on the one row, never a duplicate.
fn mid_cycle_delivered(conn: &rusqlite::Connection, session: &str, event_id: i64) -> Result<bool> {
    use rusqlite::OptionalExtension as _;
    let seen = conn
        .query_row(
            "SELECT 1 FROM code_mail_delivered WHERE session = ?1 AND event_id = ?2",
            rusqlite::params![session, event_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    Ok(seen)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

// ── M3: rooms & shared channels projection ───────────────────────────────────

/// One member of a coordination room, with honest liveness.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoomMember {
    /// The member session id (`code-<id>`).
    pub session: String,
    /// The obs noun it publishes under.
    pub agent_noun: String,
    /// True when the owning process is still alive (signal-0 probe). A SIGKILL'd
    /// session reads stale here — liveness is honest, never assumed.
    pub live: bool,
}

/// One advisory edit claim in a room (who is editing what).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoomClaim {
    pub session: String,
    pub path: String,
    pub created_at: String,
}

/// One recent message on a room's shared channel (`in/group/<id>`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoomMessage {
    pub from: Option<String>,
    pub message: String,
    pub created_at: String,
}

/// A coordination room made legible to the human: its roster, its claims, and
/// its recent channel traffic.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoomRow {
    pub room: String,
    pub members: Vec<RoomMember>,
    pub claims: Vec<RoomClaim>,
    pub channel: Vec<RoomMessage>,
}

/// Project every coordination room with members (`code_room_members`), each with
/// its roster (liveness honest), its advisory claims, and its recent channel
/// traffic. Pure ledger query — no new capture. A root with no rooms returns an
/// empty vector. `recent_n` bounds the channel tail per room.
pub fn recent_rooms(root: &Root, recent_n: usize) -> Result<Vec<RoomRow>> {
    let conn = crate::db::open(root).context("opening the ledger for the rooms projection")?;
    crate::db::init_schema(&conn)?;

    // The distinct rooms with members, each room's roster.
    let mut stmt = conn.prepare(
        "SELECT room, session, agent_noun, owner_pid
           FROM code_room_members
          ORDER BY room ASC, joined_at ASC, session ASC",
    )?;
    let members = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i32>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Group members by room, in first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut rooms: std::collections::HashMap<String, RoomRow> = std::collections::HashMap::new();
    for (room, session, agent_noun, owner_pid) in members {
        let entry = rooms.entry(room.clone()).or_insert_with(|| {
            order.push(room.clone());
            RoomRow {
                room: room.clone(),
                members: Vec::new(),
                claims: Vec::new(),
                channel: Vec::new(),
            }
        });
        entry.members.push(RoomMember {
            session,
            agent_noun,
            live: crate::codesession::pid_alive_pub(owner_pid),
        });
    }

    // For each room: its claims (all of them — this is the human's view, so no
    // viewer-exclusion) and its recent channel traffic.
    for room in &order {
        if let Some(rr) = rooms.get_mut(room) {
            rr.claims = room_claims(&conn, room).unwrap_or_default();
            rr.channel = crate::codesession::room_recent(root, room, recent_n)
                .unwrap_or_default()
                .into_iter()
                .map(|m| RoomMessage {
                    from: m.from,
                    message: clip(&m.message, 200),
                    created_at: m.created_at,
                })
                .collect();
        }
    }

    Ok(order.into_iter().filter_map(|r| rooms.remove(&r)).collect())
}

/// All advisory claims in a room (every holder), newest last — the human sees the
/// whole board, not a per-viewer slice.
fn room_claims(conn: &rusqlite::Connection, room: &str) -> Result<Vec<RoomClaim>> {
    let mut stmt = conn.prepare(
        "SELECT session, path, created_at FROM code_claims
          WHERE room = ?1 ORDER BY created_at ASC, session ASC, path ASC",
    )?;
    let rows = stmt
        .query_map([room], |r| {
            Ok(RoomClaim {
                session: r.get(0)?,
                path: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `elanus code rooms [--json] [--recent N]`: print the coordination rooms.
pub fn rooms_cmd(root: &Root, rest: &[String]) -> Result<()> {
    let want_json = rest.iter().any(|a| a == "--json");
    let recent_n = parse_named(rest, "--recent").unwrap_or(5);
    let rows = recent_rooms(root, recent_n)?;
    if want_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("(no coordination rooms with members)");
    } else {
        for r in &rows {
            println!("room {} ({} member(s), {} claim(s))", r.room, r.members.len(), r.claims.len());
            for m in &r.members {
                println!("  member {} ({}) {}", m.session, m.agent_noun, if m.live { "live" } else { "stale" });
            }
            for c in &r.claims {
                println!("  claim {} <- {}", c.path, c.session);
            }
            for msg in &r.channel {
                println!("  channel {}: {}", msg.from.as_deref().unwrap_or("?"), msg.message);
            }
        }
    }
    Ok(())
}

fn parse_named(rest: &[String], flag: &str) -> Option<usize> {
    let i = rest.iter().position(|a| a == flag)?;
    rest.get(i + 1)?.parse::<usize>().ok().filter(|n| *n > 0)
}

// ── M4: memory-block inspector projection (read-only) ────────────────────────

/// One block surfaced in the inspector. `ephemeral` distinguishes the live,
/// never-stored inbox/channel blocks (computed each turn, owner-less) from the
/// durable identity/learned blocks read out of `context_blocks`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BlockRow {
    pub name: String,
    pub scope: String,
    pub placement: String,
    pub priority: i32,
    pub owner: String,
    pub content: String,
    /// True for the recomputed inbox/channel blocks (decision 2): these are
    /// session-computed, owner-less, and "live, not stored". The UI renders them
    /// under the session, separate from owner-keyed durable blocks.
    pub ephemeral: bool,
}

/// Project a coding session's blocks for the inspector: the DURABLE blocks read
/// from `context_blocks` (keyed by the session's agent noun + session, exactly as
/// `load_session_blocks` does for the per-turn injection) PLUS the recomputed
/// EPHEMERAL inbox/channel blocks (decision 2 — these are never persisted, so we
/// recompute them via `inbox_for_session`/`room_recent`). A session with no
/// blocks returns an empty vector, never an error.
pub fn session_blocks(root: &Root, session: &str) -> Result<Vec<BlockRow>> {
    let mut out: Vec<BlockRow> = Vec::new();
    let rec = crate::codesession::read_record(root, session)
        .ok()
        .flatten();
    let agent_noun = rec
        .as_ref()
        .map(|r| r.agent_noun.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "code-agent".to_string());

    // Durable blocks: owner = the session's agent noun (decision 5 — the same key
    // the run carries). load_session_blocks reads exactly what the agent sees.
    let conn = crate::db::open(root).context("opening the ledger for the block inspector")?;
    crate::db::init_schema(&conn)?;
    if let Ok(blocks) = crate::context_store::load_session_blocks(&conn, &agent_noun, session) {
        for b in blocks {
            out.push(BlockRow {
                name: b.name,
                scope: crate::context_store::scope_str(&b.scope).to_string(),
                placement: crate::context_store::placement_str(&b.placement).to_string(),
                priority: b.priority,
                owner: b.owner,
                content: b.content,
                ephemeral: false,
            });
        }
    }

    // Ephemeral inbox block: the live unseen-mail status, owner-less, never stored
    // (decision 2). NOTE: mid-cycle mail is deliberately not marked seen, so this
    // count can include a message also delivered mid-cycle — the UI explains that
    // ("urgent copy delivered early; still unread") rather than treating it as a bug.
    let unseen =
        crate::codesession::inbox_for_session(root, &agent_noun, session, true).unwrap_or_default();
    if !unseen.is_empty() {
        let latest = unseen.last();
        let content = format!(
            "{} unseen message(s) in the inbox (live — not stored). Latest from {}: {}",
            unseen.len(),
            latest.and_then(|i| i.from.as_deref()).unwrap_or("?"),
            clip(latest.map(|i| i.message.as_str()).unwrap_or(""), 200),
        );
        out.push(BlockRow {
            name: "inbox".to_string(),
            scope: "session".to_string(),
            placement: "system".to_string(),
            priority: -10,
            owner: String::new(),
            content,
            ephemeral: true,
        });
    }

    // Ephemeral channel block: the room's recent shared-channel traffic, owner-less.
    if let Some(room) = rec.as_ref().and_then(|r| r.room.clone()).filter(|s| !s.is_empty()) {
        let msgs = crate::codesession::room_recent(root, &room, 5).unwrap_or_default();
        if !msgs.is_empty() {
            let mut content =
                format!("Recent traffic on shared channel {room} (live — not stored):");
            for m in &msgs {
                content.push_str(&format!(
                    "\n  {}: {}",
                    m.from.as_deref().unwrap_or("?"),
                    clip(&m.message, 200)
                ));
            }
            out.push(BlockRow {
                name: format!("channel:{room}"),
                scope: "session".to_string(),
                placement: "system".to_string(),
                priority: 50,
                owner: String::new(),
                content,
                ephemeral: true,
            });
        }
    }

    Ok(out)
}

/// `elanus code blocks --session <id> [--json]`: print a session's blocks
/// (durable + recomputed-ephemeral). Backs the web /api/blocks route.
pub fn blocks_cmd(root: &Root, rest: &[String]) -> Result<()> {
    let want_json = rest.iter().any(|a| a == "--json");
    let session = parse_str(rest, "--session").unwrap_or_default();
    if session.is_empty() {
        anyhow::bail!("usage: elanus code blocks --session <code-id> [--json]");
    }
    let rows = session_blocks(root, &session)?;
    if want_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("(no blocks for {session})");
    } else {
        for b in &rows {
            let tag = if b.ephemeral { " [ephemeral]" } else { "" };
            println!("{} ({}/{}, p{}){tag}", b.name, b.scope, b.placement, b.priority);
        }
    }
    Ok(())
}

fn parse_str(rest: &[String], flag: &str) -> Option<String> {
    let i = rest.iter().position(|a| a == flag)?;
    rest.get(i + 1).cloned().filter(|s| !s.is_empty())
}

/// `elanus code mail [--json] [--limit N]`: print the recent agent-to-agent mail.
pub fn mail_cmd(root: &Root, rest: &[String]) -> Result<()> {
    let want_json = rest.iter().any(|a| a == "--json");
    let limit = parse_limit(rest).unwrap_or(200);
    let rows = recent_mail(root, limit)?;
    if want_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("(no agent-to-agent mail recorded yet)");
    } else {
        for m in &rows {
            let prio = if m.priority != 0 {
                format!(" p{}", m.priority)
            } else {
                String::new()
            };
            let tags = [
                m.failed.then_some("FAILED"),
                m.mid_cycle.then_some("mid-cycle"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(",");
            let tags = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{tags}]")
            };
            println!(
                "#{:<5} {} {} -> {}  {:<8}{prio}{tags}  {}",
                m.id,
                m.ts,
                m.from.as_deref().unwrap_or("?"),
                m.to.as_deref().unwrap_or("?"),
                m.state,
                m.preview,
            );
        }
    }
    Ok(())
}

/// Parse `--limit N` (the value may be the next token). None when absent/invalid.
fn parse_limit(rest: &[String]) -> Option<usize> {
    let i = rest.iter().position(|a| a == "--limit")?;
    rest.get(i + 1)?.parse::<usize>().ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, EmitOpts};
    use serde_json::json;

    fn temp_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "elanus-mailcli-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    /// Emit a delivery exactly as `record_delivery` does: an `in/agent/<noun>/<session>`
    /// event with sender + correlation + priority + a `{prompt}` payload.
    fn deliver(root: &Root, from: &str, noun: &str, session: &str, msg: &str, prio: i32, corr: &str) -> i64 {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let mailbox = format!(
            "in/agent/{}/{}",
            crate::topic::encode_segment(noun),
            crate::topic::encode_segment(session),
        );
        events::emit(
            root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": msg })),
                correlation: Some(corr.to_string()),
                sender: Some(from.to_string()),
                priority: prio as i64,
                ..EmitOpts::new(&mailbox)
            },
        )
        .unwrap()
    }

    #[test]
    fn projects_from_to_priority_state_threaded_by_correlation() {
        let root = temp_root("basic");
        // A delivered to B (normal), A to C (high-priority), A to D (will fail).
        let id_normal = deliver(&root, "code-a0000001", "claude-code", "code-b0000001", "do x", 0, "corr-normal");
        let id_high = deliver(&root, "code-a0000001", "claude-code", "code-c0000001", "urgent", 9, "corr-high");
        let id_fail = deliver(&root, "code-a0000001", "claude-code", "code-d0000001", "risky", 0, "corr-fail");

        // The fail one's correlation gets a {failed:true} reply (failure-mail).
        let conn = crate::db::open(&root).unwrap();
        events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "failed": true, "error": "boom" })),
                correlation: Some("corr-fail".to_string()),
                ..EmitOpts::new("in/human/owner")
            },
        )
        .unwrap();

        let rows = recent_mail(&root, 50).unwrap();
        assert_eq!(rows.len(), 3, "three deliveries projected");
        // Newest first.
        assert_eq!(rows[0].id, id_fail);
        assert_eq!(rows[2].id, id_normal);

        let normal = rows.iter().find(|r| r.id == id_normal).unwrap();
        assert_eq!(normal.from.as_deref(), Some("code-a0000001"));
        assert_eq!(normal.to.as_deref(), Some("code-b0000001"));
        assert_eq!(normal.to_noun.as_deref(), Some("claude-code"));
        assert_eq!(normal.priority, 0);
        assert!(!normal.failed);
        assert_eq!(normal.preview, "do x");

        let high = rows.iter().find(|r| r.id == id_high).unwrap();
        assert_eq!(high.priority, 9, "high-priority delivery surfaces its priority");

        let failed = rows.iter().find(|r| r.id == id_fail).unwrap();
        assert!(failed.failed, "the failure-mail correlation flags the delivery failed");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn empty_root_returns_empty_not_error() {
        let root = temp_root("empty");
        let rows = recent_mail(&root, 50).unwrap();
        assert!(rows.is_empty());
        let rooms = recent_rooms(&root, 5).unwrap();
        assert!(rooms.is_empty(), "no rooms with members → empty, not error");
        let blocks = session_blocks(&root, "code-nobody1").unwrap();
        assert!(blocks.is_empty(), "a session with no blocks → empty, not error");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── M3 — the rooms projection: roster (liveness), claims, channel ───────────
    #[test]
    fn rooms_project_roster_claims_and_channel() {
        let root = temp_root("rooms");
        // Two members in one room: one live (our own pid), one a dead ghost.
        crate::codesession::join_room(&root, "room-1", "code-live0001", "claude-code", std::process::id() as i32).unwrap();
        crate::codesession::join_room(&root, "room-1", "code-dead0001", "codex", i32::MAX).unwrap();
        // The live member holds an edit-claim.
        crate::codesession::add_claim(&root, "room-1", "code-live0001", "src/foo.rs").unwrap();
        // A message posted to the room's shared channel (in/group/room-1).
        let conn = crate::db::open(&root).unwrap();
        events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "anyone on the parser?" })),
                sender: Some("code-dead0001".to_string()),
                ..EmitOpts::new("in/group/room-1")
            },
        )
        .unwrap();

        let rooms = recent_rooms(&root, 5).unwrap();
        assert_eq!(rooms.len(), 1, "one room with members");
        let r = &rooms[0];
        assert_eq!(r.room, "room-1");
        assert_eq!(r.members.len(), 2, "both members in the roster");
        let live = r.members.iter().find(|m| m.session == "code-live0001").unwrap();
        assert!(live.live, "the live member's pid is alive");
        let dead = r.members.iter().find(|m| m.session == "code-dead0001").unwrap();
        assert!(!dead.live, "the ghost member reads stale (liveness honest)");
        // The claim is attributed to its holder.
        assert_eq!(r.claims.len(), 1);
        assert_eq!(r.claims[0].session, "code-live0001");
        assert_eq!(r.claims[0].path, "src/foo.rs");
        // The recent channel message is surfaced.
        assert_eq!(r.channel.len(), 1);
        assert_eq!(r.channel[0].message, "anyone on the parser?");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── M4 — the block inspector: durable + recomputed ephemeral inbox ──────────
    #[test]
    fn block_inspector_durable_plus_ephemeral_inbox() {
        let root = temp_root("blocks");
        // A default profile so a record's agent noun resolves; and a session record.
        std::fs::create_dir_all(root.dir.join("profiles/default")).unwrap();
        std::fs::write(
            root.dir.join("profiles/default/profile.toml"),
            "agent = \"claude-code\"\nowner = \"owner\"\n",
        )
        .unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-insp0001".into(),
                native_session: "n1".into(),
                tool: "claude".into(),
                agent_noun: "claude-code".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        // A durable session-scope block owned by the session's agent noun.
        let conn = crate::db::open(&root).unwrap();
        let mut blk = crate::context_blocks::ContextBlock::new("estimate", "{\"dollars\":0.4}", "claude-code");
        blk.scope = crate::context_blocks::Scope::Session;
        crate::context_store::upsert_block(&conn, "default", &blk, "code-insp0001", None).unwrap();
        // An unseen delivery in the session's mailbox → the live ephemeral inbox block.
        deliver(&root, "code-planner1", "claude-code", "code-insp0001", "ping", 0, "corr-i");

        let blocks = session_blocks(&root, "code-insp0001").unwrap();
        let durable = blocks.iter().find(|b| b.name == "estimate").unwrap();
        assert!(!durable.ephemeral, "durable block is not ephemeral");
        assert_eq!(durable.scope, "session");
        let inbox = blocks.iter().find(|b| b.name == "inbox").unwrap();
        assert!(inbox.ephemeral, "the inbox block is ephemeral (live, not stored)");
        assert_eq!(inbox.owner, "", "ephemeral inbox is owner-less (rendered under the session)");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn mid_cycle_is_a_flag_not_a_duplicate_row() {
        let root = temp_root("midcycle");
        let id = deliver(&root, "code-a0000001", "claude-code", "code-b0000001", "urgent", 9, "corr-mc");
        // Record the SAME event as delivered mid-cycle (the double-channel case).
        let conn = crate::db::open(&root).unwrap();
        conn.execute(
            "INSERT INTO code_mail_delivered (session, event_id) VALUES (?1, ?2)",
            rusqlite::params!["code-b0000001", id],
        )
        .unwrap();
        let rows = recent_mail(&root, 50).unwrap();
        assert_eq!(rows.len(), 1, "double-channel delivery is ONE row, not two");
        assert!(rows[0].mid_cycle, "the one row carries the mid-cycle tell");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn ignores_bare_agent_mailboxes_and_non_code_targets() {
        let root = temp_root("filter");
        // A bare agent mailbox (chat seat territory) — three levels, ignored.
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "hi" })),
                sender: Some("owner".to_string()),
                ..EmitOpts::new("in/agent/main")
            },
        )
        .unwrap();
        // A delivery to a non-code session target — also ignored.
        deliver(&root, "owner", "main", "web-123", "hello", 0, "corr-x");
        let rows = recent_mail(&root, 50).unwrap();
        assert!(rows.is_empty(), "only code-* session mail is projected");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

//! The exec/dispatcher side of resident hooks: publish a hook REQUEST to the
//! broker (the chain coordinator, src/broker.rs) and wait for the folded
//! verdict — MQTT request/response (§4.10), Response Topic + Correlation
//! Data, per docs/bus.md "[DECIDED 2026-06-11]".
//!
//! Zero-overhead contract: when no resident hooks are registered (the
//! overwhelmingly common case) this module does ONE indexed sqlite read (the
//! kv row the broker maintains on register/deregister) and returns — no
//! connection, no polling, no round trip. The bus client exists only while a
//! matching point is active, and it is rumqttc's *sync* Client driven on its
//! own std thread: a client library must never sit anywhere near
//! trace::write (the nested-runtime hazard documented in src/bus.rs), and
//! the sync client's internal current_thread runtime lives entirely on the
//! drive thread, so calling consult() from inside exec's tokio context is
//! safe (publish/subscribe are plain channel sends; the wait is a blocking
//! mpsc recv, which only parks one worker thread — same as a shell tool).
//!
//! Staleness, honestly: (a) a registration landing mid-tool-call is seen at
//! the next tool call (the kv row is read per chain run); (b) a daemon crash
//! leaves the row stale-active until the next daemon start clears it — the
//! consult then fails toward allow as fast as the connect error surfaces,
//! and a RETRY_AFTER backoff keeps a dead broker from costing a connect
//! attempt per tool call. Both windows are the accepted attach race.
//!
//! Degradation order (docs/bus.md): listener/broker down → resident hooks
//! simply don't exist. A consult that cannot reach the broker, or whose
//! verdict never arrives, allows — loudly, with an obs/harness/hook echo —
//! because the exec-hook chain has already run and recording is unaffected.

use crate::bus;
use crate::hooks::Decision;
use crate::paths::Root;
use crate::trace;
use rumqttc::v5::mqttbytes::v5::{Packet, PublishProperties};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{Client, Connection, Event, MqttOptions};
use rusqlite::Connection as Db;
use serde_json::{json, Value};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// kv key the broker maintains: comma-joined hook points with at least one
/// live resident registration. Empty/absent = nothing registered.
pub const ACTIVE_KEY: &str = "resident_hooks_active";

/// Hard cap on one consult. The broker enforces per-registration timeouts
/// and always answers while alive, so this only bites when the broker dies
/// mid-request — at which point allow (degradation order) is the answer.
const CONSULT_CAP: Duration = Duration::from_secs(10);
/// After a failed consult (connect refused, verdict lost), stay off the bus
/// this long: a stale-active kv row after a daemon crash must not cost a
/// connect attempt per tool call.
const RETRY_AFTER: Duration = Duration::from_secs(15);

enum Msg {
    Pub { topic: String, payload: Vec<u8> },
    Err,
}

struct Line {
    client: Client,
    rx: Receiver<Msg>,
    resp_topic: String,
}

struct State {
    line: Option<Line>,
    retry_after: Option<Instant>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| {
        Mutex::new(State {
            line: None,
            retry_after: None,
        })
    })
}

/// Is any resident hook registered for `point`? One indexed kv read; errors
/// read as "no" (a broken db will fail the tool call elsewhere, loudly).
fn point_active(conn: &Db, point: &str) -> bool {
    match crate::db::kv_get(conn, ACTIVE_KEY) {
        Ok(Some(v)) => v.split(',').any(|p| p == point),
        _ => false,
    }
}

fn connect(root: &Root) -> Option<Line> {
    let cfg = bus::config(root);
    if !cfg.enabled {
        return None;
    }
    let addr = bus::connect_addr(&cfg)?;
    let mut opts = MqttOptions::new(
        format!("el-hook-{}", std::process::id()),
        addr.ip().to_string(),
        addr.port(),
    );
    opts.set_keep_alive(Duration::from_secs(10));
    // The consult is kernel machinery running inside the (uncaged) exec
    // process, so it presents the kernel credential (docs/identity.md) — not
    // a package token, and not anonymous once deny-by-default is live.
    if let Some(secret) = crate::secrets::read(root, crate::secrets::KERNEL) {
        opts.set_credentials(crate::secrets::KERNEL, secret);
    }
    let (client, connection) = Client::new(opts, 16);
    let resp_topic = format!(
        "obs/harness/hookresp/req-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    // Queue the subscribe BEFORE any request publish: the broker processes
    // one session's packets in order, so the response subscription always
    // exists by the time a verdict could be published.
    client.subscribe(&resp_topic, QoS::AtLeastOnce).ok()?;
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let resub = client.clone();
    let resub_topic = resp_topic.clone();
    std::thread::Builder::new()
        .name("lanius-hookline".into())
        .spawn(move || drive(connection, tx, resub, resub_topic))
        .ok()?;
    Some(Line {
        client,
        rx,
        resp_topic,
    })
}

/// Drive the sync connection on its own thread; forward verdict publishes.
/// On any connection error: report it (the consult fails toward allow) and
/// keep polling — rumqttc reconnects, and a fresh ConnAck re-subscribes the
/// response topic (subscriptions don't survive our clean-start sessions).
fn drive(mut connection: Connection, tx: Sender<Msg>, client: Client, resp_topic: String) {
    loop {
        match connection.recv() {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                let topic = String::from_utf8_lossy(&p.topic).into_owned();
                if tx
                    .send(Msg::Pub {
                        topic,
                        payload: p.payload.to_vec(),
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => {
                let _ = client.subscribe(&resp_topic, QoS::AtLeastOnce);
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                if tx.send(Msg::Err).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(_) => return, // client handle dropped
        }
    }
}

// ── Resident STAGES (docs/context.md) ──────────────────────────────────────
//
// Same request/response idea as hooks, opposite failure semantics: a hook
// that can't be reached allows (policy must not take the agent down with
// the radio); a STAGE that can't be reached FAILS the run (it composes
// meaning — silently skipping it corrupts the prompt). Hence its own
// connection and state, never shared with the hook line.
//
// Addressing deviation from the hook seam, deliberate: response topic and
// correlation ride IN THE BODY ({doc, response_topic, correlation}), not
// MQTT §4.10 properties — the broker is not the coordinator here (it only
// fans out and keeps these topics off disk), and the serving daemon is a
// plain `lanius bus sub | transform | lanius bus pub` pipeline, which the
// CLI supports today with no property plumbing. Topics:
//   request:  obs/harness/stagereq/<package>/<stage>   (daemon subscribes)
//   response: obs/harness/stageresp/req-<pid>-<uuid>   (consult subscribes)
// Both prefixes are fan-out-only, never recorded (broker carve-out): a
// context document is megabytes; the per-stage obs delta is the record.

/// Stage budgets: resident stages get the same 10s an exec stage gets,
/// plus connection slack.
const STAGE_CAP: Duration = Duration::from_secs(15);
/// Context documents are MBs; rumqttc's default inbound cap is 10KB.
pub const MAX_PACKET: u32 = 64 * 1024 * 1024;

static STAGE_STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn stage_state() -> &'static Mutex<State> {
    STAGE_STATE.get_or_init(|| {
        Mutex::new(State {
            line: None,
            retry_after: None,
        })
    })
}

fn stage_connect(root: &Root) -> Option<Line> {
    let cfg = bus::config(root);
    if !cfg.enabled {
        return None;
    }
    let addr = bus::connect_addr(&cfg)?;
    let mut opts = MqttOptions::new(
        format!("el-stage-{}", std::process::id()),
        addr.ip().to_string(),
        addr.port(),
    );
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_max_packet_size(Some(MAX_PACKET));
    // Kernel machinery in the exec process — present the kernel credential.
    if let Some(secret) = crate::secrets::read(root, crate::secrets::KERNEL) {
        opts.set_credentials(crate::secrets::KERNEL, secret);
    }
    let (client, connection) = Client::new(opts, 16);
    let resp_topic = format!(
        "obs/harness/stageresp/req-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    client.subscribe(&resp_topic, QoS::AtLeastOnce).ok()?;
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let resub = client.clone();
    let resub_topic = resp_topic.clone();
    std::thread::Builder::new()
        .name("lanius-stageline".into())
        .spawn(move || drive(connection, tx, resub, resub_topic))
        .ok()?;
    Some(Line {
        client,
        rx,
        resp_topic,
    })
}

/// Consult a resident stage: publish the document, wait for the transformed
/// one. Every failure is an Err — the caller (context::assemble) fails the
/// run with a stage-attributed error. No retry_after backoff here: a stage
/// consult failing must not silently degrade the NEXT call either; each
/// call pays the connect attempt and reports honestly.
pub fn stage_consult(
    root: &Root,
    package: &str,
    stage: &str,
    doc: &Value,
) -> anyhow::Result<Value> {
    use anyhow::{bail, Context as _};
    let mut st = stage_state()
        .lock()
        .map_err(|_| anyhow::anyhow!("stage line poisoned"))?;
    if st.line.is_none() {
        st.line = stage_connect(root);
    }
    let Some(line) = st.line.as_ref() else {
        bail!("bus disabled or unreachable (resident stages need the daemon's broker)");
    };
    while line.rx.try_recv().is_ok() {} // drop stale responses
    let correlation = uuid::Uuid::new_v4().simple().to_string();
    let req_topic = format!(
        "obs/harness/stagereq/{}/{}",
        crate::topic::encode_segment(package),
        crate::topic::encode_segment(stage)
    );
    let body = json!({
        "doc": doc,
        "response_topic": line.resp_topic,
        "correlation": correlation,
    })
    .to_string();
    if line
        .client
        .publish(&req_topic, QoS::AtLeastOnce, false, body)
        .is_err()
    {
        st.line = None; // reconnect next call
        bail!("publish to {req_topic} failed");
    }
    let deadline = Instant::now() + STAGE_CAP;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!(
                "no response after {STAGE_CAP:?} — is the {package} daemon running and approved?"
            );
        }
        match line.rx.recv_timeout(left) {
            Ok(Msg::Pub { topic, payload }) if topic == line.resp_topic => {
                let v: Value = serde_json::from_slice(&payload).context("response was not JSON")?;
                if v["correlation"] != correlation.as_str() {
                    continue; // stale response from an abandoned consult
                }
                if let Some(err) = v["error"].as_str() {
                    bail!("stage daemon reported: {err}");
                }
                if !v["doc"].is_object() {
                    bail!("response carried no document");
                }
                return Ok(v["doc"].clone());
            }
            Ok(Msg::Pub { .. }) => {}
            Ok(Msg::Err) => {
                st.line = None;
                bail!("bus connection failed mid-consult");
            }
            Err(RecvTimeoutError::Timeout) => {
                bail!("no response after {STAGE_CAP:?} — is the {package} daemon running and approved?");
            }
            Err(RecvTimeoutError::Disconnected) => {
                st.line = None;
                bail!("stage line died");
            }
        }
    }
}

/// Consult the resident-hook chain for `point` on `matched` (tool name for
/// the tool-call points, event topic for pre_dispatch). Runs AFTER the
/// exec-hook chain (see the chain-order note in src/exec.rs) and feeds it
/// the possibly-rewritten subject. Infallible by design: every failure mode
/// degrades to allow-with-echo, because policy enforcement that can take the
/// whole agent down when the radio dies would invert the degradation order.
pub fn consult(
    root: &Root,
    conn: &Db,
    point: &str,
    matched: &str,
    subject: Value,
    ids: &trace::Ids,
) -> Decision {
    let pass = |subject: Value| Decision {
        allow: true,
        subject,
        denied_by: None,
        reason: None,
    };
    if !point_active(conn, point) {
        return pass(subject);
    }
    let mut st = match state().lock() {
        Ok(g) => g,
        Err(_) => return pass(subject),
    };
    if st.retry_after.is_some_and(|t| Instant::now() < t) {
        return pass(subject);
    }
    if st.line.is_none() {
        st.line = connect(root);
        if st.line.is_none() {
            st.retry_after = Some(Instant::now() + RETRY_AFTER);
            return pass(subject);
        }
    }
    let correlation = uuid::Uuid::new_v4().simple().to_string();
    // The matched value rides the topic so registrations filter with the one
    // pattern language: tool names are a single (encoded) level, event
    // topics keep their levels.
    let suffix: String = matched
        .split('/')
        .map(crate::topic::encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    let req_topic = format!("obs/harness/hookreq/{point}/{suffix}");
    let outcome = (|| -> Result<Option<Value>, ()> {
        let line = st.line.as_ref().ok_or(())?;
        // Drop verdicts a previous, abandoned consult left behind.
        while line.rx.try_recv().is_ok() {}
        let body = json!({
            "point": point, "matched": matched, "subject": subject,
            "correlation": correlation,
        })
        .to_string();
        let props = PublishProperties {
            response_topic: Some(line.resp_topic.clone()),
            correlation_data: Some(correlation.clone().into_bytes().into()),
            ..Default::default()
        };
        line.client
            .publish_with_properties(&req_topic, QoS::AtLeastOnce, false, body, props)
            .map_err(|_| ())?;
        let deadline = Instant::now() + CONSULT_CAP;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match line.rx.recv_timeout(left) {
                Ok(Msg::Pub { topic, payload }) if topic == line.resp_topic => {
                    let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                    if v["correlation"] == correlation.as_str() {
                        return Ok(Some(v));
                    }
                }
                Ok(Msg::Pub { .. }) => {}
                Ok(Msg::Err) => return Err(()),
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => return Err(()),
            }
        }
    })();
    match outcome {
        Ok(Some(v)) => {
            let allow = v["decision"] == "allow";
            let event = v.get("event").filter(|e| e.is_object()).cloned();
            Decision {
                allow,
                subject: event.unwrap_or(subject),
                denied_by: v["denied_by"].as_str().map(String::from),
                reason: v["reason"].as_str().map(String::from),
            }
        }
        Ok(None) | Err(()) => {
            // Verdict never came (broker died mid-request, or the connection
            // broke): tear down so the next consult reconnects, back off,
            // and allow — loudly. Per-registration timeouts are broker-side;
            // reaching this cap means the coordinator itself is gone.
            st.line = None;
            st.retry_after = Some(Instant::now() + RETRY_AFTER);
            trace::write(
                root,
                &format!("obs/harness/hook/{point}/allow"),
                ids,
                json!({
                    "hook": "resident", "matched": matched,
                    "detail": { "mode": "unavailable",
                                "reason": "resident-hook coordinator unreachable; allowing (degradation order)" },
                }),
            );
            pass(subject)
        }
    }
}

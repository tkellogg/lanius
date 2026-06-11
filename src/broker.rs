//! The micro-broker: elanus's MQTT 5 boundary interface (docs/bus.md).
//!
//! Runs as an ntex System on its own std::thread inside the daemon — never
//! start it from inside a tokio task (spike/ntex/REPORT.md: the two runtimes
//! coexist only because this thread has no tokio handle). All broker state
//! lives on the single worker thread (ntex internals are !Send); the kernel
//! reaches it through one unbounded channel.
//!
//! The listener is fan-out and ingress only. Inbound routing:
//! - `el-mirror` publishes (our own processes): forwarded verbatim — the
//!   origin already recorded them;
//! - `in/ signal/`: materialized into the ledger via emit(); the
//!   PUBACK means "the ledger accepted it" (at-least-once handoff), then the
//!   event is announced to subscribers;
//! - everything else: wrapped in the standard envelope, recorder decides
//!   disk, fan-out either way.
//!
//! Subscriptions: per-filter grant; invalid filters are denied with the
//! proper SUBACK reason. `$share/<group>/<filter>` shared subscriptions
//! (§4.8.2) are supported: one group member receives each matching message
//! (round-robin), no retained replay, ACL checked against the inner filter.
//! SUBSCRIBE user properties are accepted and ignored for now —
//! blocking-subscription declarations only become honored capabilities once
//! grants land (step 5). No auth: the loopback default is the boundary until
//! then.
//!
//! Work plane on the bus: an in/# (or signal/#) event is announced under its
//! own topic exactly once — by this broker when it arrived over the bus
//! (inbound() materializes then fans out, inserting announced=1), or by the
//! daemon's announce sweep (dispatcher::announce_ledger_events) for events
//! the kernel minted itself. When an in/# delivery fans out to QoS 1
//! subscribers, the broker joins their PUBACK futures and publishes
//! obs/harness/delivery/complete when the last one lands — completion as
//! control flow, never blocking the publisher's own PUBACK.

use crate::bus::{BusConfig, BusMsg, MIRROR_PROP};
use crate::events::{self, EmitOpts};
use crate::paths::Root;
use crate::recorder;
use crate::topic;
use crate::trace;
use anyhow::Result;
use ntex::service::{fn_factory_with_config, fn_service};
use ntex_mqtt::v5::{self, MqttServer, Publish, PublishAck, Session};
use ntex_mqtt::{Control, Reason};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Debug)]
pub struct ServerError;

impl From<()> for ServerError {
    fn from(_: ()) -> Self {
        ServerError
    }
}

impl TryFrom<ServerError> for PublishAck {
    type Error = ServerError;
    fn try_from(err: ServerError) -> Result<Self, Self::Error> {
        Err(err)
    }
}

/// Per-connection state handed to ntex; the broker-side record (sink + subs)
/// lives in Broker.sessions keyed by `key`.
#[derive(Clone)]
struct BusSession {
    key: u64,
    sink: v5::MqttSink,
}

struct SubRec {
    /// The filter exactly as subscribed — identity for re-subscribe and
    /// UNSUBSCRIBE, including any `$share/<group>/` prefix.
    filter: String,
    /// What actually matches topics: `filter` minus the share prefix.
    inner: String,
    /// Some(group) for `$share/<group>/<inner>` shared subscriptions.
    group: Option<String>,
    qos: v5::QoS,
}

struct Will {
    topic: String,
    payload: String,
    retain: bool,
}

struct SessionRec {
    sink: v5::MqttSink,
    subs: Vec<SubRec>,
    /// Some(package) when CONNECT authenticated with a supervisor-minted
    /// token: this session is that package actor, and its subscribe/publish
    /// are scoped to the package's approved grants. None = anonymous local
    /// client (the human at the keyboard) — full access until identity for
    /// non-actor clients lands (docs/bus.md open question 7).
    actor: Option<String>,
    /// CONNECT last will, fired on abnormal close (crash-only liveness).
    will: Option<Will>,
}

struct Broker {
    root: Root,
    /// Own connection: this thread is not the dispatcher's. None if open
    /// failed — inbound ledger topics are then dropped loudly.
    conn: Option<rusqlite::Connection>,
    sessions: RefCell<HashMap<u64, SessionRec>>,
    retained: RefCell<HashMap<String, String>>,
    /// Supervisor-minted actor tokens: package name → token. Registered
    /// over the kernel channel just before each actor spawn.
    actors: RefCell<HashMap<String, String>>,
    /// Round-robin cursors for shared subscription groups, keyed by
    /// "<group>\u{1}<inner filter>". Consumer choice is broker discretion
    /// (§4.8.2); round-robin spreads load and is easy to reason about in
    /// tests. No ordering guarantee across the group, per spec.
    shared_rr: RefCell<HashMap<String, usize>>,
    next_key: Cell<u64>,
}

impl Broker {
    fn new(root: Root) -> Broker {
        let conn = match crate::db::open(&root) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[bus] db unavailable to broker, inbound work will drop: {e:#}");
                None
            }
        };
        Broker {
            root,
            conn,
            sessions: RefCell::new(HashMap::new()),
            retained: RefCell::new(HashMap::new()),
            actors: RefCell::new(HashMap::new()),
            shared_rr: RefCell::new(HashMap::new()),
            next_key: Cell::new(1),
        }
    }

    /// May this actor publish here? The zero-cage floor (its own status
    /// subtree) is always allowed; everything else needs an approved
    /// publish grant under the current manifest hash.
    fn actor_may_publish(&self, pkg: &str, topic_name: &str) -> bool {
        // Encode the name into the floor filter exactly as status_event()
        // encodes it when publishing — otherwise a package literally named
        // "+" would yield the floor `obs/package/+/#`, a valid wildcard that
        // matches every other package's status subtree. (Discovery also now
        // rejects wildcard names, so this is belt-and-suspenders.)
        let floor = format!("obs/package/{}/#", crate::topic::encode_segment(pkg));
        if topic::matches(&floor, topic_name) {
            return true;
        }
        let Some(conn) = self.conn.as_ref() else { return false };
        crate::packages::may(conn, pkg, "publish", topic_name).unwrap_or(false)
    }

    fn actor_may_subscribe(&self, pkg: &str, filter: &str) -> bool {
        let Some(conn) = self.conn.as_ref() else { return false };
        crate::packages::approved(conn, pkg, "subscribe")
            .map(|fs| fs.iter().any(|f| f == filter))
            .unwrap_or(false)
    }
}

/// Remove a session; fire its will unless the close was clean. The denied/
/// dead echo goes to obs/ so the variety ladder can pick it up.
fn drop_session(st: &Rc<Broker>, key: u64, clean: bool) {
    let rec = st.sessions.borrow_mut().remove(&key);
    if let Some(rec) = rec {
        if !clean {
            if let Some(w) = rec.will {
                if w.retain {
                    if w.payload.is_empty() {
                        st.retained.borrow_mut().remove(&w.topic);
                    } else {
                        st.retained.borrow_mut().insert(w.topic.clone(), w.payload.clone());
                    }
                }
                fan_out(st, &w.topic, &w.payload);
            }
        }
    }
}

/// Start the broker thread; returns once the listener is bound (or not).
pub fn spawn(root: Root, cfg: BusConfig, rx: UnboundedReceiver<BusMsg>) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new()
        .name("elanus-bus".into())
        .spawn(move || run_system(root, cfg, rx, ready_tx))?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(r) => r,
        Err(_) => anyhow::bail!("bus thread did not report readiness within 5s"),
    }
}

fn run_system(
    root: Root,
    cfg: BusConfig,
    rx: UnboundedReceiver<BusMsg>,
    ready_tx: std::sync::mpsc::Sender<Result<()>>,
) {
    // The bind factory must be callable per worker; the receiver moves into
    // whichever worker takes it first (we run exactly one).
    let rx_cell: Arc<Mutex<Option<UnboundedReceiver<BusMsg>>>> = Arc::new(Mutex::new(Some(rx)));
    let bind = cfg.bind.clone();
    let sys = ntex::rt::System::new("elanus-bus", ntex::rt::DefaultRuntime);
    let run = sys.run(move || {
        let built = ntex::server::build().bind("mqtt", bind.as_str(), {
            let root = root.clone();
            let rx_cell = rx_cell.clone();
            move |_| {
                let root = root.clone();
                let rx_cell = rx_cell.clone();
                async move {
                    let st = Rc::new(Broker::new(root));
                    if let Some(rx) = rx_cell.lock().unwrap().take() {
                        let st = st.clone();
                        ntex::rt::spawn(async move { pump(st, rx).await });
                    }
                    let st_h = st.clone();
                    let st_p = st.clone();
                    let st_c = st.clone();
                    let st_pub = st.clone();
                    MqttServer::new(move |h: v5::Handshake| {
                        let st = st_h.clone();
                        async move { handshake(st, h).await }
                    })
                    .control(fn_factory_with_config(move |session: Session<BusSession>| {
                        let st = st_c.clone();
                        async move {
                            Ok::<_, ServerError>(fn_service(move |control| {
                                control_msg(st.clone(), session.clone(), control)
                            }))
                        }
                    }))
                    .protocol(fn_factory_with_config(move |session: Session<BusSession>| {
                        let st = st_p.clone();
                        async move {
                            Ok::<_, ServerError>(fn_service(move |msg| {
                                protocol_msg(st.clone(), session.clone(), msg)
                            }))
                        }
                    }))
                    .publish(fn_factory_with_config(move |session: Session<BusSession>| {
                        let st = st_pub.clone();
                        async move {
                            Ok::<_, ServerError>(fn_service(move |req| {
                                inbound(st.clone(), session.clone(), req)
                            }))
                        }
                    }))
                }
            }
        });
        match built {
            Ok(b) => {
                let _ = ready_tx.send(Ok(()));
                b.workers(1).run();
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e.into()));
                ntex::rt::System::current().stop();
            }
        }
        Ok(())
    });
    if let Err(e) = run {
        eprintln!("[bus] system exited: {e}");
    }
}

async fn handshake(st: Rc<Broker>, h: v5::Handshake) -> Result<v5::HandshakeAck<BusSession>, ServerError> {
    let pkt = h.packet();
    // Identity: username+password = package + supervisor-minted token.
    // Wrong credentials are refused; NO credentials is an anonymous local
    // client with full access (interim — bus.md open question 7).
    let actor = match (&pkt.username, &pkt.password) {
        (Some(u), Some(p)) => {
            let name = u.to_string();
            let token_ok = st
                .actors
                .borrow()
                .get(&name)
                .is_some_and(|t| t.as_bytes() == p.as_ref());
            if !token_ok {
                eprintln!("[bus] CONNECT refused: bad token for actor {name:?}");
                return Ok(h.failed(v5::codec::ConnectAckReason::NotAuthorized));
            }
            Some(name)
        }
        _ => None,
    };
    let will = pkt.last_will.as_ref().map(|w| Will {
        topic: w.topic.to_string(),
        payload: String::from_utf8_lossy(&w.message).into_owned(),
        retain: w.retain,
    });
    // An actor's will must also pass its publish ACL — a crash announcement
    // is still a publish.
    let will = match (&actor, will) {
        (Some(pkg), Some(w)) => {
            if topic::valid_name(&w.topic) && st.actor_may_publish(pkg, &w.topic) {
                Some(w)
            } else {
                eprintln!("[bus] dropping unauthorized will for {pkg}: {}", w.topic);
                None
            }
        }
        (_, w) => w,
    };
    let key = st.next_key.get();
    st.next_key.set(key + 1);
    let sink = h.sink();
    st.sessions
        .borrow_mut()
        .insert(key, SessionRec { sink: sink.clone(), subs: Vec::new(), actor, will });
    Ok(h.ack(BusSession { key, sink }))
}

async fn control_msg(
    st: Rc<Broker>,
    session: Session<BusSession>,
    control: Control<ServerError>,
) -> Result<Option<v5::codec::Encoded>, ServerError> {
    match control {
        Control::Stop(reason) => {
            // Reaching Stop with the session still in the map means it never
            // said a clean DISCONNECT: abnormal close, the will fires.
            drop_session(&st, session.key, false);
            if let Reason::Error(_) = reason {
                Ok(Some(
                    v5::codec::Packet::from(v5::codec::Disconnect {
                        reason_code: v5::codec::DisconnectReasonCode::UnspecifiedError,
                        ..Default::default()
                    })
                    .into(),
                ))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

async fn protocol_msg(
    st: Rc<Broker>,
    session: Session<BusSession>,
    msg: v5::ProtocolMessage,
) -> Result<v5::ProtocolMessageAck, ServerError> {
    match msg {
        v5::ProtocolMessage::Subscribe(mut s) => {
            let actor: Option<String> =
                st.sessions.borrow().get(&session.key).and_then(|r| r.actor.clone());
            for mut sub in s.iter_mut() {
                let filter = sub.topic().to_string();
                // $share/<group>/<filter> (§4.8.2): competing consumers —
                // one member of the group receives each matching message.
                let (group, inner) = match parse_share(&filter) {
                    Some(x) => x,
                    None => {
                        sub.fail(v5::codec::SubscribeAckReason::TopicFilterInvalid);
                        continue;
                    }
                };
                if !topic::valid_filter(&inner) {
                    sub.fail(v5::codec::SubscribeAckReason::TopicFilterInvalid);
                    continue;
                }
                if let Some(pkg) = &actor {
                    // The grant may be stored either as the full $share form
                    // (a manifest that requested the shared subscription
                    // verbatim) or as the inner filter (the capability is
                    // "receive these topics"; the group is delivery
                    // mechanics, not extra authority).
                    let allowed = st.actor_may_subscribe(pkg, &filter)
                        || (group.is_some() && st.actor_may_subscribe(pkg, &inner));
                    if !allowed {
                        // Per-filter 0x87; the echo lets the variety ladder
                        // escalate (handler → ask → approval → retry).
                        sub.fail(v5::codec::SubscribeAckReason::NotAuthorized);
                        trace::write(
                            &st.root,
                            &format!("obs/package/{pkg}/denied"),
                            &trace::Ids::default(),
                            json!({ "kind": "subscribe", "value": filter }),
                        );
                        continue;
                    }
                }
                // We do QoS 0 and 1; grant min(requested, 1).
                let granted = match sub.options().qos {
                    v5::QoS::AtMostOnce => v5::QoS::AtMostOnce,
                    _ => v5::QoS::AtLeastOnce,
                };
                if let Some(rec) = st.sessions.borrow_mut().get_mut(&session.key) {
                    rec.subs.retain(|x| x.filter != filter);
                    rec.subs.push(SubRec {
                        filter: filter.clone(),
                        inner: inner.clone(),
                        group: group.clone(),
                        qos: granted,
                    });
                }
                sub.confirm(granted);
                // Retained replay, QoS 0: last known value, best effort.
                // Never for shared subscriptions [MQTT-3.3.1-10].
                if group.is_none() {
                    let matching: Vec<(String, String)> = st
                        .retained
                        .borrow()
                        .iter()
                        .filter(|(t, _)| topic::matches(&inner, t))
                        .map(|(t, p)| (t.clone(), p.clone()))
                        .collect();
                    for (t, p) in matching {
                        let _ = session.sink.publish(t).send_at_most_once(p.into_bytes().into());
                    }
                }
            }
            Ok(s.ack())
        }
        v5::ProtocolMessage::Unsubscribe(s) => {
            if let Some(rec) = st.sessions.borrow_mut().get_mut(&session.key) {
                for f in s.iter() {
                    rec.subs.retain(|x| x.filter != f.as_str());
                }
            }
            Ok(s.ack())
        }
        v5::ProtocolMessage::Disconnect(d) => {
            // Clean goodbye: the will is discarded [MQTT-3.14], except the
            // explicit "disconnect with will" reason (0x04).
            let with_will = d.packet().reason_code
                == v5::codec::DisconnectReasonCode::DisconnectWithWillMessage;
            drop_session(&st, session.key, !with_will);
            Ok(d.ack())
        }
        v5::ProtocolMessage::Ping(p) => Ok(p.ack()),
        _ => Ok(msg.ack()),
    }
}

async fn inbound(
    st: Rc<Broker>,
    session: Session<BusSession>,
    publish: Publish,
) -> Result<PublishAck, ServerError> {
    let topic = publish.publish_topic().to_string();
    let (retain, mirror) = {
        let pkt = publish.packet();
        let mirror = pkt
            .properties
            .user_properties
            .iter()
            .any(|(k, _)| k.as_str() == MIRROR_PROP);
        (pkt.retain, mirror)
    };
    let payload = publish.read_all().await.unwrap_or_default();
    // A failure reason in the PUBACK is the honest answer for QoS 1: "I did
    // not take ownership." The hand-rolled mirror is QoS 0 (no ack), so this
    // only ever reaches a real client (rumqttc/`elanus bus pub`), which the
    // CLI now treats as an error — the at-least-once handoff cannot lie.
    use v5::codec::PublishAckReason as Nack;
    let nack = |r: Nack| Ok(PublishAck::new(r));
    if !topic::valid_name(&topic) {
        eprintln!("[bus] rejecting inbound publish to invalid topic {topic:?}");
        return nack(Nack::TopicNameInvalid);
    }
    // Actor sessions publish inside their approved filters (plus the status
    // floor); a deny is a 0x87 NACK plus an obs echo, never a silent success.
    let actor: Option<String> =
        st.sessions.borrow().get(&session.key).and_then(|r| r.actor.clone());
    if let Some(pkg) = &actor {
        if !st.actor_may_publish(pkg, &topic) {
            trace::write(
                &st.root,
                &format!("obs/package/{pkg}/denied"),
                &trace::Ids::default(),
                json!({ "kind": "publish", "value": topic }),
            );
            return nack(Nack::NotAuthorized);
        }
    }
    let text = String::from_utf8_lossy(&payload).into_owned();
    // The v3 routing rule (docs/topics.md): in/# and signal/# materialize to
    // the ledger; obs/# (and everything else) fans out only.
    let first = topic.split('/').next().unwrap_or("");
    let is_ledger = matches!(first, "in" | "signal");

    let out_line = if is_ledger {
        // Ledger topics ALWAYS materialize, el-mirror or not: the mirror's
        // "already recorded, forward verbatim" shortcut is for observations
        // the kernel itself produced (obs/...), never a license for a client
        // to inject un-ledgered, un-audited in/signal events by setting one
        // user property. The PUBACK below is the at-least-once handoff, so a
        // failed emit must NACK, not silently succeed.
        let pv: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let Some(conn) = st.conn.as_ref() else {
            eprintln!("[bus] no db, refusing inbound {topic}");
            return nack(Nack::ImplementationSpecificError);
        };
        let mut opts = EmitOpts::new(&topic);
        opts.payload = Some(pv.clone());
        // This same function fans the materialized event out below, so the
        // row is born already-announced; the dispatcher's announce sweep
        // must not publish it a second time.
        opts.pre_announced = true;
        match events::emit(&st.root, conn, opts) {
            Ok(id) => json!({
                "ts": trace::now_iso(), "kind": topic, "payload": pv, "event_id": id
            })
            .to_string(),
            Err(e) => {
                eprintln!("[bus] inbound {topic} emit failed: {e:#}");
                return nack(Nack::UnspecifiedError);
            }
        }
    } else if mirror {
        // Observation our own process already recorded; forward verbatim.
        text
    } else {
        // Observation from an external client: standard envelope, the
        // recorder decides disk, fan-out regardless.
        let pv: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let line = json!({ "ts": trace::now_iso(), "kind": topic, "payload": pv }).to_string();
        if recorder::get(&st.root).sink_for(&topic) == recorder::Sink::Trace {
            trace::append_line(&st.root, &line);
        }
        line
    };

    if retain {
        if payload.is_empty() {
            st.retained.borrow_mut().remove(&topic);
        } else {
            st.retained.borrow_mut().insert(topic.clone(), out_line.clone());
        }
    }
    fan_out(&st, &topic, &out_line);
    Ok(publish.ack())
}

/// Drain the kernel's channel: publishes onto connected subscribers,
/// control messages into broker state.
async fn pump(st: Rc<Broker>, mut rx: UnboundedReceiver<BusMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            BusMsg::Publish(p) => {
                if p.retain {
                    if p.line.is_empty() {
                        st.retained.borrow_mut().remove(&p.topic);
                    } else {
                        st.retained.borrow_mut().insert(p.topic.clone(), p.line.clone());
                    }
                }
                fan_out(&st, &p.topic, &p.line);
            }
            BusMsg::RegisterActor { name, token } => {
                st.actors.borrow_mut().insert(name, token);
            }
            BusMsg::UnregisterActor { name } => {
                st.actors.borrow_mut().remove(&name);
            }
        }
    }
}

/// Split a `$share/<group>/<filter>` subscription into (group, inner filter)
/// per §4.8.2. Non-shared filters pass through as (None, filter). None =
/// malformed share syntax (missing or wildcard group, empty inner filter).
fn parse_share(filter: &str) -> Option<(Option<String>, String)> {
    let Some(rest) = filter.strip_prefix("$share/") else {
        return Some((None, filter.to_string()));
    };
    let (group, inner) = rest.split_once('/')?;
    if group.is_empty() || group.contains('+') || group.contains('#') || inner.is_empty() {
        return None;
    }
    Some((Some(group.to_string()), inner.to_string()))
}

/// Deliver to every session with a matching subscription, once per session at
/// the strongest granted QoS. Shared subscriptions deliver to exactly one
/// member per (group, filter), round-robin; a session holding both a normal
/// and a shared matching subscription legitimately receives both copies.
/// QoS 1 deliveries resolve on the subscriber's PUBACK; dead sinks are pruned
/// on send failure — the safety net under Control::Stop cleanup.
///
/// Completion fan-in (docs/bus.md, "completion is kernel-owned"): for in/#
/// deliveries the broker counts QoS 0 sends as complete immediately and joins
/// the QoS 1 PUBACK futures; when the last lands it publishes
/// obs/harness/delivery/complete {topic, event_id, subscribers}. Zero
/// matching subscribers publishes nothing — "nobody was listening" is already
/// visible in the ledger/dispatch records, and a completion event about no
/// deliveries would be noise. This is observation/bookkeeping only: the
/// publisher's PUBACK (= "ledger accepted") never waits on it.
fn fan_out(st: &Rc<Broker>, topic_name: &str, line: &str) {
    let mut q0: Vec<v5::MqttSink> = Vec::new();
    let mut q1: Vec<(u64, v5::MqttSink)> = Vec::new();
    {
        let sessions = st.sessions.borrow();
        for (key, rec) in sessions.iter() {
            let mut best: Option<v5::QoS> = None;
            for sub in &rec.subs {
                if sub.group.is_none() && topic::matches(&sub.inner, topic_name) {
                    best = Some(match (best, sub.qos) {
                        (Some(v5::QoS::AtLeastOnce), _) | (_, v5::QoS::AtLeastOnce) => v5::QoS::AtLeastOnce,
                        _ => v5::QoS::AtMostOnce,
                    });
                }
            }
            match best {
                Some(v5::QoS::AtMostOnce) => q0.push(rec.sink.clone()),
                Some(_) => q1.push((*key, rec.sink.clone())),
                None => {}
            }
        }
        // Shared groups: collect members per (group, inner), pick one each.
        let mut groups: HashMap<(String, String), Vec<(u64, v5::MqttSink, v5::QoS)>> =
            HashMap::new();
        for (key, rec) in sessions.iter() {
            for sub in &rec.subs {
                if let Some(g) = &sub.group {
                    if topic::matches(&sub.inner, topic_name) {
                        groups
                            .entry((g.clone(), sub.inner.clone()))
                            .or_default()
                            .push((*key, rec.sink.clone(), sub.qos));
                    }
                }
            }
        }
        for ((g, inner), mut members) in groups {
            // Stable order so the round-robin cursor means something.
            members.sort_by_key(|m| m.0);
            let mut rr = st.shared_rr.borrow_mut();
            let cursor = rr.entry(format!("{g}\u{1}{inner}")).or_insert(0);
            let (key, sink, qos) = members[*cursor % members.len()].clone();
            *cursor = cursor.wrapping_add(1);
            match qos {
                v5::QoS::AtMostOnce => q0.push(sink),
                _ => q1.push((key, sink)),
            }
        }
    }
    let subscribers = q0.len() + q1.len();
    let track = topic_name.starts_with("in/") && subscribers > 0;
    for sink in q0 {
        let _ = sink
            .publish(topic_name.to_string())
            .send_at_most_once(line.as_bytes().to_vec().into());
    }
    if q1.is_empty() {
        if track {
            delivery_complete(st, topic_name, line, subscribers);
        }
        return;
    }
    // One countdown across the QoS 1 fan-out; each delivery still rides its
    // own task so a slow subscriber never delays the others' sends.
    let remaining = Rc::new(Cell::new(q1.len()));
    for (key, sink) in q1 {
        let fut = sink
            .publish(topic_name.to_string())
            .send_at_least_once(line.as_bytes().to_vec().into());
        let st = st.clone();
        let remaining = remaining.clone();
        let topic_name = topic_name.to_string();
        let line = line.to_string();
        ntex::rt::spawn(async move {
            if fut.await.is_err() {
                st.sessions.borrow_mut().remove(&key);
            }
            // A failed/dead subscriber still "completes" its slot: the spec
            // has no requeue, so waiting on a corpse would wedge fan-in.
            remaining.set(remaining.get() - 1);
            if remaining.get() == 0 && track {
                delivery_complete(&st, &topic_name, &line, subscribers);
            }
        });
    }
}

/// All deliveries of an in/# event have completed (QoS 0 at send, QoS 1 at
/// PUBACK): publish the kernel-owned completion observation the protocol
/// itself cannot express. Recorded per recorder rules, fanned out like any
/// other observation (an obs/ topic, so no recursion back through here).
fn delivery_complete(st: &Rc<Broker>, topic_name: &str, line: &str, subscribers: usize) {
    const COMPLETE_TOPIC: &str = "obs/harness/delivery/complete";
    let event_id = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("event_id").and_then(|x| x.as_i64()));
    let out = json!({
        "ts": trace::now_iso(),
        "kind": COMPLETE_TOPIC,
        "payload": { "topic": topic_name, "event_id": event_id, "subscribers": subscribers },
    })
    .to_string();
    if recorder::get(&st.root).sink_for(COMPLETE_TOPIC) == recorder::Sink::Trace {
        trace::append_line(&st.root, &out);
    }
    fan_out(st, COMPLETE_TOPIC, &out);
}

#[cfg(test)]
mod tests {
    use super::parse_share;

    #[test]
    fn share_parsing() {
        // Non-shared filters pass through.
        assert_eq!(
            parse_share("in/package/discord/send"),
            Some((None, "in/package/discord/send".into()))
        );
        // §4.8.2 shape: $share/<group>/<filter>.
        assert_eq!(
            parse_share("$share/discord/in/package/discord/send"),
            Some((Some("discord".into()), "in/package/discord/send".into()))
        );
        // Wildcards live in the inner filter, never the group.
        assert_eq!(
            parse_share("$share/g/in/package/discord/#"),
            Some((Some("g".into()), "in/package/discord/#".into()))
        );
        assert_eq!(parse_share("$share/+/in/x"), None);
        assert_eq!(parse_share("$share/#/in/x"), None);
        // Malformed: missing group or filter.
        assert_eq!(parse_share("$share/"), None);
        assert_eq!(parse_share("$share/g"), None);
        assert_eq!(parse_share("$share//in/x"), None);
        // "$share/g/" has an empty inner filter.
        assert_eq!(parse_share("$share/g/"), None);
    }
}

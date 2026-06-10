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
//! - `work/ signal/ human/`: materialized into the ledger via emit(); the
//!   PUBACK means "the ledger accepted it" (at-least-once handoff), then the
//!   event is announced to subscribers;
//! - everything else: wrapped in the standard envelope, recorder decides
//!   disk, fan-out either way.
//!
//! Subscriptions: per-filter grant; `$share` and invalid filters are denied
//! with the proper SUBACK reason. SUBSCRIBE user properties are accepted and
//! ignored for now — blocking-subscription declarations only become honored
//! capabilities once grants land (step 5). No auth: the loopback default is
//! the boundary until then.

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
    filter: String,
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
            next_key: Cell::new(1),
        }
    }

    /// May this actor publish here? The zero-cage floor (its own status
    /// subtree) is always allowed; everything else needs an approved
    /// publish grant under the current manifest hash.
    fn actor_may_publish(&self, pkg: &str, topic_name: &str) -> bool {
        if topic::matches(&format!("obs/skill/{pkg}/#"), topic_name) {
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
                if filter.starts_with("$share/") {
                    sub.fail(v5::codec::SubscribeAckReason::SharedSubscriptionNotSupported);
                    continue;
                }
                if !topic::valid_filter(&filter) {
                    sub.fail(v5::codec::SubscribeAckReason::TopicFilterInvalid);
                    continue;
                }
                if let Some(pkg) = &actor {
                    if !st.actor_may_subscribe(pkg, &filter) {
                        // Per-filter 0x87; the echo lets the variety ladder
                        // escalate (handler → human/ask → approval → retry).
                        sub.fail(v5::codec::SubscribeAckReason::NotAuthorized);
                        trace::write(
                            &st.root,
                            &format!("obs/skill/{pkg}/denied"),
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
                    rec.subs.push(SubRec { filter: filter.clone(), qos: granted });
                }
                sub.confirm(granted);
                // Retained replay, QoS 0: last known value, best effort.
                let matching: Vec<(String, String)> = st
                    .retained
                    .borrow()
                    .iter()
                    .filter(|(t, _)| topic::matches(&filter, t))
                    .map(|(t, p)| (t.clone(), p.clone()))
                    .collect();
                for (t, p) in matching {
                    let _ = session.sink.publish(t).send_at_most_once(p.into_bytes().into());
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
    if !topic::valid_name(&topic) {
        eprintln!("[bus] dropping inbound publish to invalid topic {topic:?}");
        return Ok(publish.ack());
    }
    // Actor sessions publish inside their approved filters (plus the status
    // floor); a deny is a drop with an obs echo, never silent.
    let actor: Option<String> =
        st.sessions.borrow().get(&session.key).and_then(|r| r.actor.clone());
    if let Some(pkg) = &actor {
        if !st.actor_may_publish(pkg, &topic) {
            trace::write(
                &st.root,
                &format!("obs/skill/{pkg}/denied"),
                &trace::Ids::default(),
                json!({ "kind": "publish", "value": topic }),
            );
            return Ok(publish.ack());
        }
    }
    let text = String::from_utf8_lossy(&payload).into_owned();

    let out_line = if mirror {
        // Our own process already recorded this; forward verbatim.
        text
    } else {
        let pv: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let first = topic.split('/').next().unwrap_or("");
        if matches!(first, "work" | "signal" | "human") {
            // Ledger topics: the PUBACK below is the at-least-once handoff.
            let Some(conn) = st.conn.as_ref() else {
                eprintln!("[bus] no db, dropping inbound {topic}");
                return Ok(publish.ack());
            };
            let mut opts = EmitOpts::new(&topic);
            opts.payload = Some(pv.clone());
            match events::emit(&st.root, conn, opts) {
                Ok(id) => json!({
                    "ts": trace::now_iso(), "kind": topic, "payload": pv, "event_id": id
                })
                .to_string(),
                Err(e) => {
                    eprintln!("[bus] inbound {topic} emit failed: {e:#}");
                    return Ok(publish.ack());
                }
            }
        } else {
            // Observation from an external client: standard envelope, the
            // recorder decides disk, fan-out regardless.
            let line = json!({ "ts": trace::now_iso(), "kind": topic, "payload": pv }).to_string();
            if recorder::get(&st.root).sink_for(&topic) == recorder::Sink::Trace {
                trace::append_line(&st.root, &line);
            }
            line
        }
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

/// Deliver to every session with a matching subscription, once per session at
/// the strongest granted QoS. QoS 1 deliveries resolve on the subscriber's
/// PUBACK; for now they complete in the background ("all deliveries done" as
/// control flow arrives when work moves onto the bus). Dead sinks are pruned
/// on send failure — the safety net under Control::Stop cleanup.
fn fan_out(st: &Rc<Broker>, topic_name: &str, line: &str) {
    let mut q0: Vec<v5::MqttSink> = Vec::new();
    let mut q1: Vec<(u64, v5::MqttSink)> = Vec::new();
    {
        let sessions = st.sessions.borrow();
        for (key, rec) in sessions.iter() {
            let mut best: Option<v5::QoS> = None;
            for sub in &rec.subs {
                if topic::matches(&sub.filter, topic_name) {
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
    }
    for sink in q0 {
        let _ = sink
            .publish(topic_name.to_string())
            .send_at_most_once(line.as_bytes().to_vec().into());
    }
    for (key, sink) in q1 {
        let fut = sink
            .publish(topic_name.to_string())
            .send_at_least_once(line.as_bytes().to_vec().into());
        let st = st.clone();
        ntex::rt::spawn(async move {
            if fut.await.is_err() {
                st.sessions.borrow_mut().remove(&key);
            }
        });
    }
}

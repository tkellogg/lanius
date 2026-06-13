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
//!
//! Resident hooks (docs/bus.md hook plane, [DECIDED 2026-06-11]): a SUBSCRIBE
//! carrying user properties `mode=blocking, order, timeout_ms, on_timeout`
//! on a filter under `obs/harness/hookreq/<point>/...` is a blocking-hook
//! registration — honored ONLY for a token-authed session whose package has
//! an approved `blocking` grant for that literal point. Everyone else gets
//! plain observation semantics (props ignored, loudly). The broker is the
//! chain coordinator: a hook REQUEST is a QoS 1 publish to
//! `obs/harness/hookreq/<point>/<matched>` with Response Topic + Correlation
//! Data (§4.10); the broker runs matching registrations in (order, seq)
//! order — each via a per-invocation Response Topic under
//! `obs/harness/hookresp/<id>` with a broker-side timeout — folds verdicts
//! (first deny stops; allow+rewrite feeds the next), and publishes the final
//! {decision, event, reason} to the requester's response topic. Requests live
//! under obs/ (NOT in/) on purpose: in/# materializes to the ledger by the
//! v3 routing rule, and hook round trips are sub-500ms ephemera that must
//! never be ledger-backed (topics.md decided 7); special-casing a reserved
//! in/ prefix would break "the delivery contract is decidable at segment 1".
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

/// One resident blocking-hook registration (a grant-approved SUBSCRIBE with
/// `mode=blocking` user properties). Lives and dies with the session —
/// crash-only: a dead hook client deregisters by disconnecting, and an
/// in-flight invocation falls to its broker-enforced timeout/default.
#[derive(Clone)]
struct BlockingReg {
    /// Subscription filter, under obs/harness/hookreq/<point>/...
    filter: String,
    /// The literal hook point (segment 4 of the filter); grant-checked.
    point: String,
    order: u32,
    timeout_ms: u64,
    /// on_timeout=allow|deny — also covers send failures and malformed
    /// verdicts. Fail-open vs fail-closed is the registrant's declaration.
    allow_on_timeout: bool,
    /// Registration sequence: stable tiebreak within equal `order`.
    seq: u64,
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
    /// Resident blocking-hook registrations held by this session.
    blocking: Vec<BlockingReg>,
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
    /// In-flight hook invocations awaiting a verdict, keyed by the minted
    /// response-topic id (obs/harness/hookresp/<id>). A verdict publish
    /// completes the sender; a timeout removes the entry — a late verdict
    /// after timeout is dropped on the floor, which is correct: the chain
    /// already applied the declared default.
    pending_verdicts: RefCell<HashMap<String, tokio::sync::oneshot::Sender<Value>>>,
    /// Registration sequence counter (ordering tiebreak).
    reg_seq: Cell<u64>,
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
        let b = Broker {
            root,
            conn,
            sessions: RefCell::new(HashMap::new()),
            retained: RefCell::new(HashMap::new()),
            actors: RefCell::new(HashMap::new()),
            shared_rr: RefCell::new(HashMap::new()),
            pending_verdicts: RefCell::new(HashMap::new()),
            reg_seq: Cell::new(1),
            next_key: Cell::new(1),
        };
        // A fresh broker has zero resident registrations by construction;
        // clear any stale active-points row a crashed daemon left behind so
        // exec's zero-overhead check doesn't chase ghosts.
        b.refresh_hooks_kv();
        b
    }

    /// Recompute the kv row exec/dispatcher read for the zero-overhead gate:
    /// the comma-joined set of hook points with at least one live resident
    /// registration. Updated on register/deregister/disconnect; cleared on
    /// broker start. Staleness window (documented in src/resident.rs): a
    /// registration that lands mid-tool-call is seen at the next tool call;
    /// a daemon crash leaves the row stale until the next daemon start (the
    /// consult path then fails fast toward allow — broker down means
    /// resident hooks don't exist, per the degradation order).
    fn refresh_hooks_kv(&self) {
        let Some(conn) = self.conn.as_ref() else { return };
        let mut points: Vec<String> = self
            .sessions
            .borrow()
            .values()
            .flat_map(|r| r.blocking.iter().map(|b| b.point.clone()))
            .collect();
        points.sort();
        points.dedup();
        if let Err(e) = crate::db::kv_set(conn, crate::resident::ACTIVE_KEY, &points.join(",")) {
            eprintln!("[bus] resident-hooks kv update failed: {e:#}");
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
        // Registrations die with the session (crash-only): refresh the
        // zero-overhead gate and leave an audit echo per detached hook. Any
        // in-flight invocation against this session times out on its own.
        if !rec.blocking.is_empty() {
            st.refresh_hooks_kv();
            for b in &rec.blocking {
                trace::write(
                    &st.root,
                    "obs/harness/hookreg/detach",
                    &trace::Ids::default(),
                    json!({ "hook": format!("resident:{}", rec.actor.as_deref().unwrap_or("?")),
                            "point": b.point, "filter": b.filter, "clean": clean }),
                );
            }
        }
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
                // disable_signals: ntex otherwise installs its own SIGINT/
                // SIGTERM handlers, which gracefully stop the MQTT server and
                // CONSUME the signal — leaving the daemon loop alive and
                // Ctrl+C apparently dead. We want the process default:
                // SIGINT kills the daemon. Crash-only is the design; there
                // is nothing to flush (WAL ledger, O_APPEND trace, retained
                // wills fire for connected clients on the dead socket).
                b.workers(1).disable_signals().run();
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
    st.sessions.borrow_mut().insert(
        key,
        SessionRec { sink: sink.clone(), subs: Vec::new(), actor, will, blocking: Vec::new() },
    );
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
            // SUBSCRIBE user properties (§3.8.2.1) are packet-level; they
            // apply to every filter in the packet (the CLI sends one).
            let props: HashMap<String, String> = s
                .packet()
                .user_properties
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let wants_blocking = props.get("mode").map(|m| m == "blocking").unwrap_or(false);
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
                // Blocking registration attempt. Blocking is a granted
                // capability, never ambient: it requires a token-authed
                // session AND an approved 'blocking' grant for the literal
                // point in the filter. Anything short of that degrades to
                // plain observation semantics (props ignored), echoed so the
                // operator can see why their hook never fires.
                if wants_blocking && group.is_none() {
                    match try_register_blocking(&st, session.key, &actor, &filter, &props) {
                        Ok(reg) => {
                            let granted = match sub.options().qos {
                                v5::QoS::AtMostOnce => v5::QoS::AtMostOnce,
                                _ => v5::QoS::AtLeastOnce,
                            };
                            sub.confirm(granted);
                            st.refresh_hooks_kv();
                            trace::write(
                                &st.root,
                                "obs/harness/hookreg/attach",
                                &trace::Ids::default(),
                                json!({
                                    "hook": format!("resident:{}", actor.as_deref().unwrap_or("?")),
                                    "point": reg.point, "filter": reg.filter,
                                    "order": reg.order, "timeout_ms": reg.timeout_ms,
                                    "on_timeout": if reg.allow_on_timeout { "allow" } else { "deny" },
                                }),
                            );
                            continue;
                        }
                        Err(why) => {
                            trace::write(
                                &st.root,
                                "obs/harness/hookreg/ignored",
                                &trace::Ids::default(),
                                json!({ "filter": filter, "reason": why,
                                        "actor": actor.as_deref() }),
                            );
                            // fall through: plain observation semantics
                        }
                    }
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
            let mut dropped_blocking = false;
            if let Some(rec) = st.sessions.borrow_mut().get_mut(&session.key) {
                for f in s.iter() {
                    rec.subs.retain(|x| x.filter != f.as_str());
                    let before = rec.blocking.len();
                    rec.blocking.retain(|b| b.filter != f.as_str());
                    dropped_blocking |= rec.blocking.len() != before;
                }
            }
            if dropped_blocking {
                st.refresh_hooks_kv();
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
    let (retain, mirror, resp_to, el_corr) = {
        let pkt = publish.packet();
        let mirror = pkt
            .properties
            .user_properties
            .iter()
            .any(|(k, _)| k.as_str() == MIRROR_PROP);
        // Response Topic (§4.10): only meaningful on hook requests.
        let resp_to = pkt.properties.response_topic.as_ref().map(|t| t.to_string());
        // el-correlation: how an external client attaches the ENVELOPE
        // correlation (flow/trace id) to a publish. Deliberately a user
        // property, not MQTT Correlation Data — the taxonomy in topics.md
        // reserves Correlation Data for the hook round trip; this is the
        // application-layer field, in the el-* namespace like el-mirror.
        let el_corr = pkt
            .properties
            .user_properties
            .iter()
            .find(|(k, _)| k.as_str() == "el-correlation")
            .map(|(_, v)| v.to_string());
        (pkt.retain, mirror, resp_to, el_corr)
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
    let actor: Option<String> =
        st.sessions.borrow().get(&session.key).and_then(|r| r.actor.clone());
    // Hook verdicts: point-to-point RPC plumbing, intercepted before generic
    // routing — never fanned out, never written to disk (the
    // obs/harness/hook/<point>/<outcome> echo is the recorded artifact).
    // ACL (topics.md decided 7): the blocking grant includes publish right
    // to this prefix — concretely, an actor session may publish here iff it
    // holds a live blocking registration. Anonymous local clients pass as
    // everywhere else.
    if let Some(id) = topic.strip_prefix("obs/harness/hookresp/") {
        if let Some(pkg) = &actor {
            let registered = st
                .sessions
                .borrow()
                .get(&session.key)
                .map(|r| !r.blocking.is_empty())
                .unwrap_or(false);
            if !registered {
                trace::write(
                    &st.root,
                    &format!("obs/package/{pkg}/denied"),
                    &trace::Ids::default(),
                    json!({ "kind": "publish", "value": topic }),
                );
                return nack(Nack::NotAuthorized);
            }
        }
        if let Some(tx) = st.pending_verdicts.borrow_mut().remove(id) {
            let v: Value = serde_json::from_str(&String::from_utf8_lossy(&payload))
                .unwrap_or(Value::Null);
            let _ = tx.send(v);
        }
        // No pending entry = a verdict after timeout; dropped, the chain
        // already applied the registration's declared default.
        return Ok(publish.ack());
    }
    // Actor sessions publish inside their approved filters (plus the status
    // floor); a deny is a 0x87 NACK plus an obs echo, never a silent success.
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
    // Stage RPC (docs/context.md, resident stages): request/response ride
    // plain topics — fanned out so the serving daemon's `bus sub` receives
    // them, NEVER written to disk (a context document is megabytes; the
    // per-stage obs delta is the recorded artifact). The actor publish ACL
    // above already ran: a package answers only with an approved publish
    // grant on stageresp/#; the kernel's consult publishes anonymously.
    if topic.starts_with("obs/harness/stagereq/") || topic.starts_with("obs/harness/stageresp/") {
        fan_out(&st, &topic, &text);
        return Ok(publish.ack());
    }
    // Hook requests: the broker is the chain coordinator. The PUBACK below
    // means "request accepted"; the verdict rides the Response Topic. The
    // raw request also fans out to any plain subscribers (observation
    // semantics for non-blocking clients) but is never written to disk —
    // requests are RPC, the hook echoes are the record. Note the actor ACL
    // above already ran: a package cannot fire hook chains without an
    // approved publish grant on this prefix (the kernel publishes
    // anonymously and passes).
    if topic.starts_with("obs/harness/hookreq/") {
        handle_hook_request(&st, &topic, &text, resp_to);
        fan_out(&st, &topic, &text);
        return Ok(publish.ack());
    }
    // The v3 routing rule (docs/topics.md): in/# and signal/# materialize to
    // the ledger; obs/# (and everything else) fans out only.
    let first = topic.split('/').next().unwrap_or("");
    let is_ledger = matches!(first, "in" | "signal");

    // The verified sender (docs/identity.md): who the broker confirmed this
    // connection belongs to — an authenticated package actor, or "human" for
    // an anonymous local session (until the identity model makes humans
    // authenticate positively). It is derived from the session, never read
    // from the message, so a client cannot claim to be someone else.
    let verified_sender = actor.clone().unwrap_or_else(|| "human".to_string());
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
        opts.correlation = el_corr.clone();
        // The broker is the trust anchor for identity: it sets the sender
        // from the connection it authenticated, so the ledger records who
        // really sent the event rather than who the event says sent it.
        opts.sender = Some(verified_sender.clone());
        // This same function fans the materialized event out below, so the
        // row is born already-announced; the dispatcher's announce sweep
        // must not publish it a second time.
        opts.pre_announced = true;
        match events::emit(&st.root, conn, opts) {
            Ok(id) => json!({
                "ts": trace::now_iso(), "kind": topic, "payload": pv, "event_id": id,
                "correlation_id": el_corr, "sender": verified_sender
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
        // recorder decides disk, fan-out regardless. The sender stamp rides
        // along so subscribers see the broker-vouched origin.
        let pv: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let line = json!({ "ts": trace::now_iso(), "kind": topic, "payload": pv, "sender": verified_sender }).to_string();
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

/// Run the resident-hook chain for one request and answer the requester.
///
/// Request topic: obs/harness/hookreq/<point>/<matched...>; payload
/// {point, matched, subject, correlation}. Matching registrations (filter
/// matches the request topic; grant re-checked at fire time so a revocation
/// takes effect immediately, not at the next reconnect) run in (order, seq)
/// order, each as its own §4.10 round trip: a direct publish to the
/// registrant's session carrying a per-invocation Response Topic
/// (obs/harness/hookresp/<id>) + Correlation Data, awaited under the
/// registration's broker-enforced timeout — one dead hook can never stall
/// the others, and a hook client killed mid-flight just falls to its
/// declared on_timeout. First deny stops the chain; an allow verdict's
/// `event` object rewrites the subject for the next hook. Every invocation
/// echoes to obs/harness/hook/<point>/<outcome>, exactly like exec hooks.
/// The final {decision, event, reason, denied_by, correlation} is published
/// to the requester's Response Topic (correlation echoed in the payload —
/// the requester's response topic is per-process and the correlation guards
/// against stale verdicts from an earlier, abandoned consult).
fn handle_hook_request(st: &Rc<Broker>, topic_name: &str, text: &str, resp_to: Option<String>) {
    let req: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let point = topic_name.split('/').nth(3).unwrap_or("").to_string();
    struct Inv {
        sink: v5::MqttSink,
        who: String,
        timeout_ms: u64,
        allow_on_timeout: bool,
        order: u32,
        seq: u64,
    }
    let mut invs: Vec<Inv> = Vec::new();
    {
        let sessions = st.sessions.borrow();
        for rec in sessions.values() {
            let Some(pkg) = &rec.actor else { continue };
            for b in &rec.blocking {
                if b.point != point || !topic::matches(&b.filter, topic_name) {
                    continue;
                }
                // Live revocation: the grant is re-checked per invocation.
                let granted = st
                    .conn
                    .as_ref()
                    .map(|c| crate::packages::is_approved(c, pkg, "blocking", &point).unwrap_or(false))
                    .unwrap_or(false);
                if !granted {
                    continue;
                }
                invs.push(Inv {
                    sink: rec.sink.clone(),
                    who: format!("resident:{pkg}"),
                    timeout_ms: b.timeout_ms,
                    allow_on_timeout: b.allow_on_timeout,
                    order: b.order,
                    seq: b.seq,
                });
            }
        }
    }
    invs.sort_by_key(|i| (i.order, i.seq));
    // Only answer requesters who minted their response topic in the
    // reserved prefix — anything else is a misdirected publish, not an RPC.
    let resp_to = resp_to
        .filter(|t| topic::valid_name(t) && t.starts_with("obs/harness/hookresp/"));
    let st = st.clone();
    let topic_name = topic_name.to_string();
    ntex::rt::spawn(async move {
        let matched = req["matched"].as_str().unwrap_or("").to_string();
        let correlation = req.get("correlation").cloned().unwrap_or(Value::Null);
        let mut subject = req.get("subject").cloned().unwrap_or(Value::Null);
        let ids = trace::Ids {
            session_id: subject["session"].as_str().map(String::from),
            ..Default::default()
        };
        let mut allow = true;
        let mut denied_by: Option<String> = None;
        let mut reason: Option<String> = None;
        for inv in invs {
            let id = format!("v{}", uuid::Uuid::new_v4().simple());
            let hook_resp = format!("obs/harness/hookresp/{id}");
            let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
            st.pending_verdicts.borrow_mut().insert(id.clone(), tx);
            let out = json!({
                "point": point, "matched": matched, "subject": subject,
                "timeout_ms": inv.timeout_ms,
            })
            .to_string();
            let started = std::time::Instant::now();
            let sent = inv
                .sink
                .publish(topic_name.clone())
                .properties(|p| {
                    p.response_topic = Some(hook_resp.clone().into());
                    p.correlation_data =
                        Some(ntex::util::Bytes::from(id.clone().into_bytes()));
                })
                .send_at_most_once(out.into_bytes().into());
            let verdict: Option<Value> = if sent.is_err() {
                None
            } else {
                match ntex::time::timeout(
                    std::time::Duration::from_millis(inv.timeout_ms.max(1)),
                    rx,
                )
                .await
                {
                    Ok(Ok(v)) => Some(v),
                    _ => None, // timeout, or the pending sender was dropped
                }
            };
            st.pending_verdicts.borrow_mut().remove(&id);
            let ms = started.elapsed().as_millis() as u64;
            // Fold the verdict exactly like exec hooks' settle(): deny stops
            // the chain; allow may rewrite; timeout/malformed/send-failure
            // apply the registration's on_timeout declaration.
            let (effect, detail) = match &verdict {
                Some(v) if v["decision"] == "allow" => match v.get("event") {
                    Some(ev) if ev.is_object() => {
                        subject = ev.clone();
                        (true, json!({ "mode": "rewrite" }))
                    }
                    _ => (true, json!({ "mode": "ok" })),
                },
                Some(v) if v["decision"] == "deny" => (
                    false,
                    json!({ "mode": "verdict",
                            "reason": v["reason"].as_str().unwrap_or("denied by resident hook") }),
                ),
                Some(_) => (
                    inv.allow_on_timeout,
                    json!({ "mode": "malformed",
                            "on_timeout": if inv.allow_on_timeout { "allow" } else { "deny" },
                            "reason": "malformed verdict payload" }),
                ),
                None => (
                    inv.allow_on_timeout,
                    json!({ "mode": "timeout",
                            "on_timeout": if inv.allow_on_timeout { "allow" } else { "deny" },
                            "reason": format!("hook timed out after {}ms", inv.timeout_ms) }),
                ),
            };
            trace::write(
                &st.root,
                &format!("obs/harness/hook/{point}/{}", if effect { "allow" } else { "deny" }),
                &ids,
                json!({ "hook": inv.who, "matched": matched, "ms": ms, "detail": detail }),
            );
            if !effect {
                allow = false;
                denied_by = Some(inv.who);
                reason = detail["reason"].as_str().map(String::from);
                break;
            }
        }
        if let Some(rt) = resp_to {
            let line = json!({
                "decision": if allow { "allow" } else { "deny" },
                "event": subject,
                "denied_by": denied_by,
                "reason": reason,
                "correlation": correlation,
            })
            .to_string();
            fan_out(&st, &rt, &line);
        }
    });
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

/// Validate and record a blocking-hook registration; Err(reason) means the
/// subscription degrades to plain observation semantics. Defaults: order 50,
/// timeout_ms 500, on_timeout deny (a dead policy hook must not silently
/// approve). The filter's point segment must be a literal hook point — the
/// grant vocabulary stays exactly the exec-hook one (blocking grant value =
/// point name), so one manifest line covers both registration styles.
fn try_register_blocking(
    st: &Rc<Broker>,
    key: u64,
    actor: &Option<String>,
    filter: &str,
    props: &HashMap<String, String>,
) -> Result<BlockingReg, String> {
    let Some(pkg) = actor else {
        return Err("blocking requires a token-authed session".into());
    };
    let segs: Vec<&str> = filter.split('/').collect();
    if segs.len() < 4 || segs[0] != "obs" || segs[1] != "harness" || segs[2] != "hookreq" {
        return Err("blocking filter must live under obs/harness/hookreq/<point>/...".into());
    }
    let point = segs[3].to_string();
    if !crate::manifest::HOOK_POINTS.contains(&point.as_str()) {
        return Err(format!("unknown or wildcard hook point {point:?} (must be literal)"));
    }
    let Some(conn) = st.conn.as_ref() else {
        return Err("broker has no db; cannot check the blocking grant".into());
    };
    if !crate::packages::is_approved(conn, pkg, "blocking", &point).unwrap_or(false) {
        return Err(format!("no approved blocking grant for point {point:?}"));
    }
    let on_timeout = props.get("on_timeout").map(|s| s.as_str()).unwrap_or("deny");
    if on_timeout != "allow" && on_timeout != "deny" {
        return Err("on_timeout must be allow|deny".into());
    }
    let seq = st.reg_seq.get();
    st.reg_seq.set(seq + 1);
    let reg = BlockingReg {
        filter: filter.to_string(),
        point,
        order: props.get("order").and_then(|s| s.parse().ok()).unwrap_or(50),
        timeout_ms: props.get("timeout_ms").and_then(|s| s.parse().ok()).unwrap_or(500),
        allow_on_timeout: on_timeout == "allow",
        seq,
    };
    let mut sessions = st.sessions.borrow_mut();
    let Some(rec) = sessions.get_mut(&key) else {
        return Err("session vanished mid-subscribe".into());
    };
    rec.blocking.retain(|b| b.filter != filter); // re-subscribe replaces
    rec.blocking.push(reg.clone());
    Ok(reg)
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

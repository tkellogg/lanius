//! `elanus web` — the dashboard server, in-process (the Rust port of
//! ui/web/server.mjs).
//!
//! Browsers cannot speak raw TCP MQTT, so this process is the ordinary anonymous
//! loopback MQTT 5 client and the browser talks to *it*: bus messages relayed
//! over SSE (`GET /api/stream`), publishes accepted over POST (`/api/publish`).
//! It also serves the built SPA — but, unlike server.mjs, the SPA is **embedded
//! in the binary** (`include_dir!` over ui/web/dist), so a `cargo install`
//! machine with no Node, no npm, and no source tree still serves the UI.
//!
//! AUTHORITY: the same as the terminal. Local channels are equally unforgeable
//! until the identity model fully lands (docs/security.md entries 3-5); the only
//! browser-specific boundary is hostile-origin traffic (CSRF, DNS rebinding), so
//! every mutating route checks Origin/Host (`origin_ok`) and UI-driven decisions
//! carry `decided_by=ui` in the ledger (`--by ui` on approve/revoke).
//!
//! RUNTIME: like the broker (src/broker.rs), this runs on its OWN ntex System —
//! ntex internals are `!Send` and the two runtimes must not be nested. The bus
//! relay (rumqttc) runs as one `ntex::rt::spawn` task on that system; SSE clients
//! and the ring buffer live in a shared `Hub`. Admin gestures and history reads
//! are offloaded to the blocking pool (`web::block`): admin shells out to THIS
//! binary (`current_exe`, not node — Phase 1 of the handoff), history proxies to
//! the userland history package over loopback HTTP.
//!
//! PARITY: web-packaging M1–M4. M4 (retiring ui/web/server.mjs + config.mjs) is
//! DONE — those files have been removed and this Rust server is the only path,
//! in production and in the test harness. The behavior the retired server.mjs
//! had — observability-M3 SSE seq tagging (each bus message tagged with a
//! MONOTONIC seq, a ring buffer replayed verbatim with stable seqs on every
//! reconnect — the CodeSessions live-merge and the chat transcript merge both
//! depend on that seq/ring contract), the /api/conversations endpoints, and the
//! RP-M3 /api/status read-camera fields (advisory/authoritative availability +
//! enabled) — is all reproduced here (the "port of server.mjs …" comments below
//! mark that lineage).

use crate::bus;
use crate::paths::Root;
use crate::secrets;
use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use ntex::util::Bytes;
use ntex::web::{self, HttpRequest, HttpResponse};
use rumqttc::v5::mqttbytes::v5::Packet;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, Event, MqttOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path as FsPath, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskCtx, Poll};
use std::time::Duration;

/// The built SPA, embedded at compile time. `dist` is a build output (gitignored),
/// so the publish workflow must `npm run build` it and `Cargo.toml`'s `include`
/// must ship it (see docs/handoffs/web-packaging.md M3).
static DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/ui/web/dist");

const RING_CAP: usize = 1000;
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);

/// Shared state for the worker: the live bus client, the recent-history ring,
/// connection status, and the registered SSE clients. Kept Send+Sync (tokio
/// channels, std Mutex, atomics, the cloneable rumqttc client) so the ntex
/// `HttpServer` factory may capture it.
struct Hub {
    root: Root,
    broker: String,
    agent: String,
    connected: AtomicBool,
    seq: AtomicU64,
    client: AsyncClient,
    ring: Mutex<VecDeque<String>>,
    next_client_id: AtomicU64,
    clients: Mutex<Vec<(u64, tokio::sync::mpsc::UnboundedSender<Bytes>)>>,
}

impl Hub {
    fn status_frame(&self) -> String {
        json!({
            "kind": "status",
            "connected": self.connected.load(Ordering::SeqCst),
            "broker": self.broker,
            "agent": self.agent,
        })
        .to_string()
    }

    /// Fan one already-serialized frame out to every live SSE client, dropping
    /// any whose receiver has gone away.
    fn broadcast(&self, frame: &str) {
        let bytes = Bytes::from(format!("data: {frame}\n\n"));
        self.clients
            .lock()
            .unwrap()
            .retain(|(_, tx)| tx.send(bytes.clone()).is_ok());
    }

    fn remove_client(&self, id: u64) {
        self.clients
            .lock()
            .unwrap()
            .retain(|(client_id, _)| *client_id != id);
    }

    /// A bus message: assign a seq, ring it for late joiners, broadcast it.
    fn ingest(&self, topic: String, env: Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let frame =
            json!({ "kind": "message", "seq": seq, "topic": topic, "env": env }).to_string();
        {
            let mut ring = self.ring.lock().unwrap();
            ring.push_back(frame.clone());
            while ring.len() > RING_CAP {
                ring.pop_front();
            }
        }
        self.broadcast(&frame);
    }
}

/// Entry point for `elanus web`. Mirrors the broker: spin an ntex System on this
/// thread, spawn the bus relay, run the HTTP server (one worker — everything is
/// single-threaded on the system). Blocks until the system stops (Ctrl-C, or
/// SIGTERM from `serve`'s supervisor).
pub fn serve_web(root: &Root, port: u16, agent: &str) -> Result<()> {
    let cfg = bus::config(root);
    let addr = bus::connect_addr(&cfg)
        .with_context(|| format!("unparseable bus bind address {:?}", cfg.bind))?;
    let broker = format!("mqtt://{addr}");
    let owner = secrets::owner_name(root);
    let cred = secrets::read(root, &owner);
    eprintln!(
        "[web] root={} owner={owner} credential={} broker={broker} port={port} agent={agent}",
        root.dir.display(),
        if cred.is_some() {
            "present"
        } else {
            "MISSING — will be refused (deny-by-default); is this the right --root?"
        }
    );

    let root = root.clone();
    let agent = agent.to_string();
    let sys = ntex::rt::System::new("elanus-web", ntex::rt::DefaultRuntime);
    let run = sys.run(move || {
        // The bus client: an anonymous loopback MQTT 5 client presenting the
        // owner identity (mirrors src/buscli.rs `client`). Absent credential →
        // connect credential-less and be refused, which is the point.
        let mut opts = MqttOptions::new(
            format!("el-web-{}", std::process::id()),
            addr.ip().to_string(),
            addr.port(),
        );
        opts.set_keep_alive(Duration::from_secs(10));
        opts.set_max_packet_size(Some(crate::resident::MAX_PACKET));
        if let Some(secret) = cred {
            opts.set_credentials(owner, secret);
        }
        let (client, eventloop) = AsyncClient::new(opts, 64);

        let hub = Arc::new(Hub {
            root,
            broker,
            agent,
            connected: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            client,
            ring: Mutex::new(VecDeque::new()),
            next_client_id: AtomicU64::new(0),
            clients: Mutex::new(Vec::new()),
        });

        // The bus relay runs on its OWN real-tokio thread, NOT the ntex runtime:
        // rumqttc drives tokio::net, which the ntex (Default/neon) runtime does
        // not host — the same two-runtimes-on-separate-threads separation the
        // broker documents (src/broker.rs). The Hub is Send+Sync, so the relay
        // pushes SSE frames / status into it and ntex handlers publish through
        // the cloneable client (sync `try_publish`).
        let relay_hub = hub.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("elanus-web-bus".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(relay(relay_hub, eventloop)),
                    Err(e) => eprintln!("[web] bus relay runtime failed: {e}"),
                }
            })
        {
            ntex::rt::System::current().stop();
            return Err(std::io::Error::other(format!(
                "spawning bus relay thread: {e}"
            )));
        }

        let hub_factory = hub.clone();
        let built = web::HttpServer::new(move || {
            // ntex 3.9 takes an async application factory.
            let hub = hub_factory.clone();
            async move {
                web::App::new()
                    .state(hub)
                    .route("/api/stream", web::get().to(stream))
                    .route("/api/status", web::get().to(status))
                    .route("/api/conversations", web::get().to(conversations))
                    .route("/api/conversations/{session}", web::get().to(conversation))
                    .route("/api/code/sessions", web::get().to(code_sessions))
                    .route("/api/code/sessions/{id}", web::get().to(code_session))
                    .route("/api/comms/mail", web::get().to(comms_mail))
                    .route("/api/comms/rooms", web::get().to(comms_rooms))
                    .route("/api/blocks", web::get().to(blocks))
                    .route("/api/blocks", web::post().to(block_set))
                    .route("/api/estimate/{session}", web::get().to(estimate_report))
                    .route("/api/publish", web::post().to(publish))
                    .service(
                        web::resource("/api/history")
                            .route(web::get().to(history))
                            .route(web::post().to(history)),
                    )
                    .service(web::resource("/api/admin/{tail}*").route(web::route().to(admin)))
                    .default_service(web::route().to(static_file))
            }
        })
        .bind(("127.0.0.1", port))
        .map(|s| s.workers(1).run());
        match built {
            Ok(_server) => {
                eprintln!("elanus web on http://127.0.0.1:{port}");
                Ok(())
            }
            Err(e) => {
                ntex::rt::System::current().stop();
                Err(e)
            }
        }
    });
    run.context("web system exited")?;
    Ok(())
}

// ---- SSE stream -----------------------------------------------------------

/// A streaming body fed by an unbounded channel. tokio's `UnboundedReceiver`
/// exposes `poll_recv`, so we implement `Stream` directly — no extra dep — and
/// hand it to `HttpResponseBuilder::streaming`.
struct SseBody {
    rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
}

impl ntex::util::Stream for SseBody {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskCtx<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

async fn stream(hub: web::types::State<Arc<Hub>>) -> HttpResponse {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let client_id = hub.next_client_id.fetch_add(1, Ordering::SeqCst) + 1;
    // Catch-up under the clients lock so no live message slips between the ring
    // snapshot and registration: status first, then the ring (matches mjs).
    {
        let mut clients = hub.clients.lock().unwrap();
        let _ = tx.send(Bytes::from("retry: 2000\n\n"));
        let _ = tx.send(Bytes::from(format!("data: {}\n\n", hub.status_frame())));
        for frame in hub.ring.lock().unwrap().iter() {
            let _ = tx.send(Bytes::from(format!("data: {frame}\n\n")));
        }
        clients.push((client_id, tx.clone()));
    }
    let heartbeat_hub = hub.get_ref().clone();
    ntex::rt::spawn(async move {
        loop {
            ntex::time::sleep(SSE_HEARTBEAT).await;
            if tx.send(Bytes::from("event: ping\ndata: {}\n\n")).is_err() {
                heartbeat_hub.remove_client(client_id);
                break;
            }
        }
    });
    HttpResponse::Ok()
        .content_type("text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .streaming(SseBody { rx })
}

/// The bus relay: one long-lived task driving the rumqttc event loop. On connect
/// it (re)subscribes obs/# in/# signal/# and announces status; each publish is
/// ringed + broadcast; a dropped connection flips status and we let the next
/// poll reconnect.
async fn relay(hub: Arc<Hub>, mut eventloop: rumqttc::v5::EventLoop) {
    let mut was_connected = false;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                hub.connected.store(true, Ordering::SeqCst);
                was_connected = true;
                eprintln!(
                    "[web:bus] connected to {} — subscribing obs/# in/# signal/#",
                    hub.broker
                );
                let _ = hub.client.subscribe("obs/#", QoS::AtMostOnce).await;
                let _ = hub.client.subscribe("in/#", QoS::AtLeastOnce).await;
                let _ = hub.client.subscribe("signal/#", QoS::AtLeastOnce).await;
                hub.broadcast(&hub.status_frame());
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                let topic = String::from_utf8_lossy(&p.topic).into_owned();
                // env = the parsed JSON payload, or {payload:<raw>} if it isn't
                // JSON (mjs parity).
                let env = serde_json::from_slice::<Value>(&p.payload).unwrap_or_else(
                    |_| json!({ "payload": String::from_utf8_lossy(&p.payload).into_owned() }),
                );
                hub.ingest(topic, env);
            }
            Ok(_) => {}
            Err(e) => {
                if hub.connected.swap(false, Ordering::SeqCst) {
                    hub.broadcast(&hub.status_frame());
                }
                if was_connected {
                    eprintln!("[web:bus] disconnected ({e}); reconnecting…");
                    was_connected = false;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

// ---- publish --------------------------------------------------------------

async fn publish(hub: web::types::State<Arc<Hub>>, req: HttpRequest, body: Bytes) -> HttpResponse {
    if !origin_ok(&req) {
        return json_resp(
            403,
            json!({ "ok": false, "error": "cross-origin request refused" }),
        );
    }
    let j: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return text_resp(400, "bad json"),
    };
    let topic = j.get("topic").and_then(Value::as_str).unwrap_or("");
    if topic.is_empty() || topic.contains('#') || topic.contains('+') {
        return text_resp(400, "bad topic");
    }
    let payload = j.get("payload").cloned().unwrap_or_else(|| json!({}));
    let payload = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    let correlation = j.get("correlation").and_then(|c| match c {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });
    let props = correlation.map(|c| rumqttc::v5::mqttbytes::v5::PublishProperties {
        user_properties: vec![("el-correlation".to_string(), c)],
        ..Default::default()
    });
    // Sync enqueue onto the relay's request channel — the publish handler runs
    // on the ntex worker thread, the eventloop drains it on the bus thread.
    match hub.client.try_publish_with_properties(
        topic,
        QoS::AtLeastOnce,
        false,
        payload.into_bytes(),
        props.unwrap_or_default(),
    ) {
        Ok(()) => json_resp(200, json!({ "ok": true })),
        Err(e) => json_resp(502, json!({ "ok": false, "error": e.to_string() })),
    }
}

// ---- status ---------------------------------------------------------------

async fn status(hub: web::types::State<Arc<Hub>>) -> HttpResponse {
    let root = &hub.root;
    let owner = secrets::owner_name(root);
    let cred = secrets::read(root, &owner).is_some();
    let history = history_endpoint(root);
    let bus = root.bus_file();
    let db = root.db();
    let trace = root.trace_file();
    let config = root.config();
    let run = root.run_dir();
    json_resp(
        200,
        json!({
            "ok": true,
            "root": root.dir.display().to_string(),
            "root_exists": root.dir.is_dir(),
            "owner": owner,
            "credential": if cred { "present" } else { "missing" },
            "broker": hub.broker,
            "broker_connected": hub.connected.load(Ordering::SeqCst),
            "web": { "port": Value::Null, "static_dir": "<embedded>", "dist_present": DIST.get_file("index.html").is_some() },
            "agent": hub.agent,
            "binary": std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            "history": { "available": history.is_some(), "endpoint": history },
            "read_camera": read_camera_status(root),
            "paths": {
                "bus": { "path": bus.display().to_string(), "exists": bus.exists() },
                "database": { "path": db.display().to_string(), "exists": db.exists() },
                "trace": { "path": trace.display().to_string(), "exists": trace.exists() },
                "config": { "path": config.display().to_string(), "exists": config.is_dir() },
                "run": { "path": run.display().to_string(), "exists": run.is_dir() },
            },
        }),
    )
}

/// READ CAMERA status (read-provenance M3) — the legibility surface server.mjs
/// `readCameraStatus()` exposes on `/api/status`. Unlike the mjs (which regex-
/// scrapes the `default` profile's `[sandbox]` table), this reuses the BACKEND's
/// own loader + computation (`crate::profile::load("default").sandbox` →
/// `crate::sandbox::read_camera_status`) — the exact path the broker gates
/// read-flavor subscribes on (src/broker.rs), so the surface can never drift from
/// the kernel. Same honest two-tier shape: advisory (available everywhere,
/// enabled = the `read_camera` toggle, default ON) + authoritative (available
/// only where the unprivileged mechanism could run — Linux; never enabled, M2
/// deferred). Absent/unreadable profile ⇒ defaults (advisory ON), matching mjs.
fn read_camera_status(root: &Root) -> Value {
    let cfg = crate::profile::load(root, "default")
        .map(|(p, _)| p.sandbox)
        .unwrap_or_default();
    let s = crate::sandbox::read_camera_status(&cfg);
    json!({
        "advisory": { "available": s.advisory.available, "enabled": s.advisory.enabled },
        "authoritative": { "available": s.authoritative.available, "enabled": s.authoritative.enabled },
    })
}

// ---- conversations (read-only sqlite projection) --------------------------

async fn conversations(hub: web::types::State<Arc<Hub>>, req: HttpRequest) -> HttpResponse {
    let agent = query_param(&req, "agent").unwrap_or_default();
    if !valid_profile_name(&agent) {
        return json_resp(400, json!({ "ok": false, "error": BAD_NAME_MSG }));
    }
    let Some(db) = db_path(&hub.root) else {
        return json_resp(
            503,
            json!({ "ok": false, "error": "conversation history unavailable — no elanus.db for this root" }),
        );
    };
    let owner = secrets::owner_name(&hub.root);
    match web::block(move || conversation_rows(&agent, &db, &owner)).await {
        Ok(rows) => json_resp(200, json!({ "ok": true, "conversations": rows })),
        Err(e) => json_resp(
            503,
            json!({ "ok": false, "error": format!("conversation history unavailable: {e}") }),
        ),
    }
}

async fn conversation(
    hub: web::types::State<Arc<Hub>>,
    path: web::types::Path<String>,
) -> HttpResponse {
    let session = path.into_inner();
    // mjs rejected a malformed %-escape (decodeURIComponent threw) with 400; ntex
    // pre-decodes, so a stray `%` reaches us literally — reject it too (no real
    // session id — web-*/evt-*/code-* — contains `%`, `\`, `"`, or NUL).
    if session.is_empty()
        || session.len() > 160
        || session.contains(['\\', '"', '%'])
        || session.contains('\0')
    {
        return json_resp(400, json!({ "ok": false, "error": "bad conversation" }));
    }
    let Some(db) = db_path(&hub.root) else {
        return json_resp(
            503,
            json!({ "ok": false, "error": "conversation history unavailable — no elanus.db for this root" }),
        );
    };
    let session_out = session.clone();
    match web::block(move || conversation_messages(&session, &db)).await {
        Ok(messages) => json_resp(
            200,
            json!({ "ok": true, "conversation": { "session": session_out, "messages": messages } }),
        ),
        Err(e) => json_resp(
            503,
            json!({ "ok": false, "error": format!("conversation history unavailable: {e}") }),
        ),
    }
}

// ---- code projection + history + admin (shell-out / proxy) ----------------

async fn code_sessions(hub: web::types::State<Arc<Hub>>) -> HttpResponse {
    let root = hub.root.clone();
    let out = web::block(move || cli(&root, &["code", "sessions", "--json"])).await;
    match out {
        Ok(r) if r.ok => {
            let trimmed = r.stdout.trim();
            let text = if trimmed.is_empty() { "[]" } else { trimmed };
            match serde_json::from_str::<Value>(text) {
                Ok(v) => json_resp(200, v),
                Err(_) => json_resp(
                    500,
                    json!({ "ok": false, "error": "bad projection output" }),
                ),
            }
        }
        Ok(r) => json_resp(500, json!({ "ok": false, "error": cli_err(&r) })),
        Err(_) => json_resp(
            500,
            json!({ "ok": false, "error": "code projection unavailable" }),
        ),
    }
}

async fn code_session(
    hub: web::types::State<Arc<Hub>>,
    path: web::types::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    let root = hub.root.clone();
    let out = web::block(move || cli(&root, &["code", "session", &id, "--json"])).await;
    match out {
        Ok(r) if r.ok => {
            let trimmed = r.stdout.trim();
            if trimmed.is_empty() || trimmed == "null" {
                return json_resp(
                    404,
                    json!({ "ok": false, "error": "no such coding session" }),
                );
            }
            match serde_json::from_str::<Value>(trimmed) {
                Ok(v) => json_resp(200, v),
                Err(_) => json_resp(
                    500,
                    json!({ "ok": false, "error": "bad projection output" }),
                ),
            }
        }
        Ok(r) => json_resp(500, json!({ "ok": false, "error": cli_err(&r) })),
        Err(_) => json_resp(
            500,
            json!({ "ok": false, "error": "code projection unavailable" }),
        ),
    }
}

// ---- comms / blocks / estimate (agent-comms-ui read routes) ---------------

/// Run a CLI projection on the blocking pool and map its stdout to a `(code,
/// Value)`. `empty_default` is the JSON text used when stdout is empty (`[]` for
/// the array projections, `null` for the estimate report) so an empty projection
/// is data, not an error — mirroring `code_sessions`. Returns a Send-safe tuple
/// (NOT an HttpResponse, which is `!Send` and cannot cross `web::block`).
fn cli_json(root: &Root, args: &[&str], empty_default: &str) -> (u16, Value) {
    match cli(root, args) {
        Ok(r) if r.ok => map_cli_json(&r.stdout, empty_default),
        Ok(r) => (500, json!({ "ok": false, "error": cli_err(&r) })),
        Err(_) => (
            500,
            json!({ "ok": false, "error": "projection unavailable" }),
        ),
    }
}

/// Map a successful CLI projection's stdout to `(code, Value)`: empty stdout maps
/// to `empty_default` (so an empty projection is data, not an error), valid JSON
/// passes through as 200, and unparseable output is a 500. Factored out so the
/// mapping the comms/blocks/estimate routes rely on is unit-testable without
/// spinning the ntex server (the shell-out itself is exercised by ui.spec.mjs).
fn map_cli_json(stdout: &str, empty_default: &str) -> (u16, Value) {
    let trimmed = stdout.trim();
    let text = if trimmed.is_empty() {
        empty_default
    } else {
        trimmed
    };
    match serde_json::from_str::<Value>(text) {
        Ok(v) => (200, v),
        Err(_) => (
            500,
            json!({ "ok": false, "error": "bad projection output" }),
        ),
    }
}

/// Await a `cli_json` call and build the response on the ntex thread (the
/// `!Send` HttpResponse is constructed here, never inside `web::block`).
async fn cli_json_resp(root: Root, args: Vec<String>, empty_default: &str) -> HttpResponse {
    let def = empty_default.to_string();
    let out = web::block(move || -> Result<(u16, Value)> {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Ok(cli_json(&root, &refs, &def))
    })
    .await;
    match out {
        Ok((code, v)) => json_resp(code, v),
        Err(_) => json_resp(
            500,
            json!({ "ok": false, "error": "projection unavailable" }),
        ),
    }
}

/// M1 — the agent-to-agent mail projection. Shells `elanus code mail --json`
/// (a pure ledger read over `in/agent/%`, threaded by correlation), exactly the
/// `code_sessions` shell-out shape. A root with no mail returns `[]`.
async fn comms_mail(hub: web::types::State<Arc<Hub>>) -> HttpResponse {
    cli_json_resp(
        hub.root.clone(),
        vec!["code".into(), "mail".into(), "--json".into()],
        "[]",
    )
    .await
}

/// M3 — the coordination-rooms projection. Shells `elanus code rooms --json`.
async fn comms_rooms(hub: web::types::State<Arc<Hub>>) -> HttpResponse {
    cli_json_resp(
        hub.root.clone(),
        vec!["code".into(), "rooms".into(), "--json".into()],
        "[]",
    )
    .await
}

/// Validate a session id the same way `conversation` does before it is handed to
/// the CLI (no `%`/`\`/`"`/NUL — no real session id contains them).
fn valid_session_id(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 160
        && !session.contains(['\\', '"', '%'])
        && !session.contains('\0')
}

/// M4 — the memory-block inspector (read-only). `?session=<code-id>` shells
/// `elanus code blocks --session <id> --json` (durable + recomputed ephemeral).
async fn blocks(hub: web::types::State<Arc<Hub>>, req: HttpRequest) -> HttpResponse {
    let Some(session) = query_param(&req, "session") else {
        return json_resp(
            400,
            json!({ "ok": false, "error": "need ?session=<code-id>" }),
        );
    };
    if !valid_session_id(&session) {
        return json_resp(400, json!({ "ok": false, "error": "bad session" }));
    }
    cli_json_resp(
        hub.root.clone(),
        vec![
            "code".into(),
            "blocks".into(),
            "--session".into(),
            session,
            "--json".into(),
        ],
        "[]",
    )
    .await
}

/// The documented follow-on to M4: a GUARDED human write to a DURABLE memory block.
/// Build the `elanus block set` argv from a validated edit request. Only DURABLE
/// blocks are writable (the ephemeral inbox/channel blocks are owner-less, computed
/// each turn, and never persisted — decision 2/3); the route rejects them before
/// reaching here. The block's KEY (scope/owner/name) is preserved; only the content
/// (and optionally priority/placement) changes. Always stamps `--by ui` so the edit
/// is attributable in `context_build_log`. Returns `Err` with a human reason on a
/// bad request. Factored out so it is unit-testable without spinning the server.
fn block_set_args(body: &Value) -> Result<Vec<String>, String> {
    let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let session = s("session");
    let name = s("name");
    let owner = s("owner");
    let scope = s("scope");
    if session.is_empty() {
        return Err("need session".into());
    }
    if !valid_session_id(session) {
        return Err("bad session".into());
    }
    if name.is_empty() {
        return Err("need block name".into());
    }
    // The ephemeral inbox/channel blocks (decision 2) are owner-less, session-computed,
    // and never stored — they are not editable. A durable block always carries an owner.
    if owner.is_empty() {
        return Err("ephemeral blocks are not editable (no owner)".into());
    }
    // content may be empty (clearing a block is a legitimate edit); it is required
    // to be present so a malformed request can't silently write an empty block.
    let content = match body.get("content").and_then(Value::as_str) {
        Some(c) => c,
        None => return Err("need content".into()),
    };
    let mut args = vec![
        "block".to_string(),
        "set".to_string(),
        name.to_string(),
        content.to_string(),
        "--session".to_string(),
        session.to_string(),
        "--owner".to_string(),
        owner.to_string(),
        // Attribution: a UI edit is decided-by `ui`, recorded in context_build_log.
        "--by".to_string(),
        "ui".to_string(),
    ];
    // scope defaults to the block's own key; the inspector echoes it back so the
    // (scope, owner, name) key is preserved across the edit.
    if !scope.is_empty() {
        args.push("--scope".to_string());
        args.push(scope.to_string());
    }
    if let Some(p) = body
        .get("placement")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        args.push("--placement".to_string());
        args.push(p.to_string());
    }
    if let Some(p) = body.get("priority").and_then(Value::as_i64) {
        args.push("--priority".to_string());
        args.push(p.to_string());
    }
    Ok(args)
}

/// `POST /api/blocks` — the guarded inline-editor write. Mirrors the `/api/admin`
/// POST contract: a cross-origin POST is refused by `origin_ok` (CSRF/DNS-rebind),
/// the body is shelled to `elanus block set ... --by ui`, and the persisted value is
/// re-read so the editor reflects what was actually stored. Only DURABLE blocks are
/// writable (ephemeral blocks have no owner and are rejected by `block_set_args`).
async fn block_set(
    hub: web::types::State<Arc<Hub>>,
    req: HttpRequest,
    body: Bytes,
) -> HttpResponse {
    if !origin_ok(&req) {
        return json_resp(
            403,
            json!({ "ok": false, "error": "cross-origin request refused (CSRF/DNS-rebinding guard)" }),
        );
    }
    let body_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return text_resp(400, "bad json"),
    };
    let args = match block_set_args(&body_json) {
        Ok(a) => a,
        Err(e) => return json_resp(400, json!({ "ok": false, "error": e })),
    };
    let root = hub.root.clone();
    let session = body_json
        .get("session")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let out = web::block(move || -> Result<(u16, Value)> {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let r = cli(&root, &refs)?;
        if !r.ok {
            return Ok((500, json!({ "ok": false, "error": cli_err(&r) })));
        }
        // Re-read the durable blocks so the editor reflects the persisted value.
        let (code, blocks) = cli_json(
            &root,
            &["code", "blocks", "--session", &session, "--json"],
            "[]",
        );
        if code == 200 {
            Ok((200, json!({ "ok": true, "blocks": blocks })))
        } else {
            Ok((200, json!({ "ok": true })))
        }
    })
    .await;
    match out {
        Ok((code, v)) => json_resp(code, v),
        Err(_) => json_resp(500, json!({ "ok": false, "error": "block write failed" })),
    }
}

/// M5 — the estimate-vs-actual report for one session. Shells
/// `elanus estimate actual --session <id> --json`, which prints the `Report` JSON
/// or `null` when the session has no recorded estimate. `null` → 200 with body
/// `null` so the runs view simply omits the estimate group (no crash, no 404).
async fn estimate_report(
    hub: web::types::State<Arc<Hub>>,
    path: web::types::Path<String>,
) -> HttpResponse {
    let session = path.into_inner();
    if !valid_session_id(&session) {
        return json_resp(400, json!({ "ok": false, "error": "bad session" }));
    }
    cli_json_resp(
        hub.root.clone(),
        vec![
            "estimate".into(),
            "actual".into(),
            "--session".into(),
            session,
            "--json".into(),
        ],
        "null",
    )
    .await
}

/// `/api/history` → POST <history endpoint>/query. The endpoint is re-read per
/// request from run/pkg-history/http.json (heals across actor restarts). GET maps
/// query params onto the flat kinds; POST passes the query DSL body through.
async fn history(hub: web::types::State<Arc<Hub>>, req: HttpRequest, body: Bytes) -> HttpResponse {
    let query: Value = if req.method() == ntex::http::Method::POST {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return text_resp(400, "bad json"),
        }
    } else {
        let mut q = serde_json::Map::new();
        if let Some(kind) = query_param(&req, "kind") {
            q.insert("kind".into(), Value::String(kind));
        }
        for k in ["agent", "session", "correlation", "limit", "before_id"] {
            if let Some(v) = query_param(&req, k) {
                q.insert(k.into(), Value::String(v));
            }
        }
        Value::Object(q)
    };
    const HIST_KINDS: [&str; 5] = ["agents", "sessions", "transcript", "conversation", "search"];
    let kind = query.get("kind").and_then(Value::as_str).unwrap_or("");
    if !HIST_KINDS.contains(&kind) {
        return json_resp(
            400,
            json!({ "ok": false, "error": format!("kind must be one of {}", HIST_KINDS.join("|")) }),
        );
    }
    let Some(base) = history_endpoint(&hub.root) else {
        return json_resp(
            503,
            json!({ "ok": false, "error": "history view unavailable — is the history package running and approved? (no run/pkg-history/http.json)" }),
        );
    };
    let out = web::block(move || proxy_history(&base, &query)).await;
    match out {
        Ok((code, text)) => HttpResponse::build(status_code(code))
            .content_type("application/json")
            .body(text),
        Err(_) => json_resp(
            503,
            json!({ "ok": false, "error": "history view unreachable — approve the history package if it is parked" }),
        ),
    }
}

/// Privileged human gestures. Phase 1 of the handoff: shell out to THIS binary
/// (`current_exe`), not node — one code path, no subprocess toolchain, and the
/// `--by ui` decided_by trail preserved on every mutating route. Mutations
/// require a same-origin/local Host (`origin_ok`); GETs are reads.
async fn admin(
    hub: web::types::State<Arc<Hub>>,
    req: HttpRequest,
    path: web::types::Path<String>,
    body: Bytes,
) -> HttpResponse {
    let method = req.method().clone();
    if method != ntex::http::Method::GET && !origin_ok(&req) {
        return json_resp(
            403,
            json!({ "ok": false, "error": "cross-origin request refused (CSRF/DNS-rebinding guard)" }),
        );
    }
    let tail = path.into_inner();
    let body_json: Value = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return text_resp(400, "bad json"),
        }
    };
    let root = hub.root.clone();
    // HttpRequest is !Send; extract the (owned) query string before offloading.
    let query = req.uri().query().unwrap_or("").to_string();
    let out = web::block(move || admin_dispatch(&root, &method, &tail, &query, &body_json)).await;
    match out {
        Ok((code, v)) => json_resp(code, v),
        Err(_) => json_resp(500, json!({ "ok": false, "error": "admin call failed" })),
    }
}

/// The admin verb table — a faithful port of server.mjs `handleAdmin`. Runs on
/// the blocking pool; every CLI gesture goes through `cli` (current_exe) with the
/// exact same args and JSON response shapes the SPA expects.
fn admin_dispatch(
    root: &Root,
    method: &ntex::http::Method,
    tail: &str,
    query: &str,
    body: &Value,
) -> Result<(u16, Value)> {
    use ntex::http::Method;
    let get = *method == Method::GET;
    let post = *method == Method::POST;
    let q = |k: &str| query_param_str(query, k);

    let r = match (tail, get, post) {
        ("models", true, _) => {
            let r = cli(root, &["models", "--json"])?;
            return Ok((
                200,
                if r.ok {
                    json!({ "ok": true, "models": json_lines(&r.stdout) })
                } else {
                    json!({ "ok": true, "models": [], "note": cli_err(&r).trim() })
                },
            ));
        }
        // ---- model providers (docs/handoffs/model-providers.md M4) ----------
        // The named, encrypted credential surface. Every gesture shells `elanus
        // provider …` (the same current_exe shell-out the rest of admin uses);
        // the secret never rides argv — `add` pipes the key on the CLI's stdin
        // safe path, and `list`/`test` only ever return the redaction the CLI
        // prints.
        ("providers", true, _) => {
            let r = cli(root, &["provider", "list", "--json"])?;
            return Ok(ok_or_err(
                &r,
                500,
                |s| json!({ "ok": true, "providers": json_lines(s) }),
            ));
        }
        ("providers", _, true) => {
            return Ok(provider_add(root, body));
        }
        ("providers/rm", _, true) => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            if !valid_provider_name(name) {
                return Ok((400, json!({ "ok": false, "error": "bad provider name" })));
            }
            cli(root, &["provider", "rm", name])?
        }
        ("providers/test", true, _) => {
            let Some(name) = q("name") else {
                return Ok((400, json!({ "ok": false, "error": "need ?name=" })));
            };
            if !valid_provider_name(&name) {
                return Ok((400, json!({ "ok": false, "error": "bad provider name" })));
            }
            let r = cli(root, &["provider", "test", &name, "--json"])?;
            if !r.ok {
                return Ok((500, json!({ "ok": false, "error": cli_err(&r) })));
            }
            // `provider test --json` prints a single JSON object (reachability +
            // the model list, or {reachable:false,error}). Pass it through; an
            // empty/garbled line is a 500, never a silent empty list.
            let line = r.stdout.trim();
            return Ok(match serde_json::from_str::<Value>(line) {
                Ok(v) => (200, v),
                Err(_) => (500, json!({ "ok": false, "error": "bad provider test output" })),
            });
        }
        ("approve", _, true) | ("revoke", _, true) => {
            let pkg = body.get("package").and_then(Value::as_str).unwrap_or("");
            if !valid_pkg_name(pkg) {
                return Ok((400, json!({ "ok": false, "error": "need {package}" })));
            }
            let verb = if tail == "approve" {
                "approve"
            } else {
                "revoke"
            };
            cli(root, &[verb, pkg, "--by", "ui"])?
        }
        ("agents", true, _) => {
            let r = cli(root, &["profile", "list"])?;
            return Ok(ok_or_err(&r, 500, |s| {
                json!({ "ok": true, "profiles": profiles_with_helper(root, s) })
            }));
        }
        ("agents", _, true) => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            if !valid_profile_name(name) {
                return Ok((400, json!({ "ok": false, "error": BAD_NAME_MSG })));
            }
            let mut args = vec!["profile", "new", name];
            let agent = body.get("agent").and_then(Value::as_str);
            let model = body.get("model").and_then(Value::as_str);
            if let Some(a) = agent {
                args.push("--agent");
                args.push(a);
            }
            if let Some(m) = model {
                args.push("--model");
                args.push(m);
            }
            let r = cli(root, &args)?;
            return Ok(profile_result(&r));
        }
        ("agents/set", _, true) => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            if !valid_profile_name(name) {
                return Ok((400, json!({ "ok": false, "error": BAD_NAME_MSG })));
            }
            let Some(set) = body
                .get("set")
                .and_then(Value::as_object)
                .filter(|m| !m.is_empty())
            else {
                return Ok((400, json!({ "ok": false, "error": "need {set}" })));
            };
            let pairs: Vec<String> = set
                .iter()
                .map(|(k, v)| format!("{k}={}", toml_value(v)))
                .collect();
            let mut args = vec!["profile".to_string(), "set".to_string(), name.to_string()];
            args.extend(pairs);
            let r = cli_owned(root, &args)?;
            return Ok(profile_result(&r));
        }
        ("kits/readme", true, _) => {
            let Some(kit) = q("kit") else {
                return Ok((400, json!({ "ok": false, "error": "need ?kit=" })));
            };
            let r = cli(root, &["kit", "show", &kit])?;
            return Ok(ok_or_code(&r, 404, |s| json!({ "ok": true, "readme": s })));
        }
        ("kits/packages", true, _) => {
            let kit = q("kit").unwrap_or_default();
            if !valid_pkg_name(&kit) {
                return Ok((400, json!({ "ok": false, "error": "bad kit" })));
            }
            return Ok(match kit_packages(root, &kit)? {
                Some(summary) => (200, json!({ "ok": true, "kit": summary })),
                None => (404, json!({ "ok": false, "error": "kit not found" })),
            });
        }
        ("kits", true, _) => {
            let r = cli(root, &["kit", "list", "--json"])?;
            return Ok(ok_or_err(
                &r,
                500,
                |s| json!({ "ok": true, "kits": json_lines(s) }),
            ));
        }
        ("kits/add", _, true) => {
            let kit = body.get("kit").and_then(Value::as_str).unwrap_or("");
            if kit.is_empty() {
                return Ok((400, json!({ "ok": false, "error": "need {kit}" })));
            }
            let mut args = vec!["kit", "add", kit];
            if body.get("copy").and_then(Value::as_bool).unwrap_or(false) {
                args.push("--copy");
            }
            cli(root, &args)?
        }
        ("kits/unlink", _, true) => {
            let kit = body.get("kit").and_then(Value::as_str).unwrap_or("");
            if !valid_pkg_name(kit) {
                return Ok((400, json!({ "ok": false, "error": "need {kit}" })));
            }
            cli(root, &["kit", "unlink", kit])?
        }
        ("packages", true, _) => {
            let profile = q("profile").unwrap_or_else(|| "default".into());
            if !valid_profile_name(&profile) {
                return Ok((400, json!({ "ok": false, "error": BAD_NAME_MSG })));
            }
            let r = cli(root, &["packages", "--json", "--profile", &profile])?;
            return Ok(ok_or_err(
                &r,
                500,
                |s| json!({ "ok": true, "packages": json_lines(s) }),
            ));
        }
        ("configs", true, _) => {
            let pkg = q("package");
            let r = match &pkg {
                Some(p) => cli(root, &["config", "list", p])?,
                None => cli(root, &["config", "list"])?,
            };
            if !r.ok {
                return Ok((500, json!({ "ok": false, "error": cli_err(&r) })));
            }
            return Ok((
                200,
                match pkg {
                    Some(p) => {
                        json!({ "ok": true, "config": json_lines(&r.stdout).into_iter().next().unwrap_or(json!({ "package": p, "toml": "" })) })
                    }
                    None => json!({ "ok": true, "configs": json_lines(&r.stdout) }),
                },
            ));
        }
        ("configs/set", _, true) => {
            let pkg = body.get("package").and_then(Value::as_str).unwrap_or("");
            let key = body.get("key").and_then(Value::as_str).unwrap_or("").trim();
            let value = body.get("value").and_then(Value::as_str);
            if !valid_pkg_name(pkg) {
                return Ok((400, json!({ "ok": false, "error": "need {package}" })));
            }
            if key.is_empty() {
                return Ok((400, json!({ "ok": false, "error": "need {key}" })));
            }
            let Some(value) = value else {
                return Ok((400, json!({ "ok": false, "error": "need {value}" })));
            };
            cli(root, &["config", "set", pkg, key, value])?
        }
        // Filesystem existence/writability probe for workdir/path fields. The web
        // is the user's terminal (loopback + same-origin), so checking a path a
        // person just typed is the same authority as `ls` in their shell.
        // Read-only. Port of server.mjs `/api/admin/path-check`.
        ("path-check", true, _) => {
            let p = q("path").unwrap_or_default();
            let p = p.trim();
            if p.is_empty() {
                return Ok((200, json!({ "ok": true, "exists": false, "empty": true })));
            }
            if p.len() > 1024 || p.contains('\0') {
                return Ok((400, json!({ "ok": false, "error": "bad path" })));
            }
            // path::absolute mirrors Node's path.resolve (lexical, no symlink
            // resolution) so a non-existent path still yields an absolute form.
            let abs = std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p));
            let abs_s = abs.display().to_string();
            return Ok(match std::fs::metadata(&abs) {
                Err(_) => (200, json!({ "ok": true, "exists": false, "path": abs_s })),
                Ok(stat) => {
                    // W_OK probe via libc::access — matches fs.accessSync(W_OK).
                    let writable = {
                        let c = std::ffi::CString::new(abs.as_os_str().as_encoded_bytes()).ok();
                        match c {
                            Some(c) => unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 },
                            None => false,
                        }
                    };
                    (
                        200,
                        json!({
                            "ok": true,
                            "exists": true,
                            "isDir": stat.is_dir(),
                            "writable": writable,
                            "path": abs_s,
                        }),
                    )
                }
            });
        }
        ("proposals", true, _) => {
            let r = cli(root, &["config", "proposals"])?;
            return Ok(ok_or_err(
                &r,
                500,
                |s| json!({ "ok": true, "proposals": json_lines(s) }),
            ));
        }
        ("proposals/show", true, _) => {
            let id = q("id").unwrap_or_default();
            if !valid_request_id(&id) {
                return Ok((400, json!({ "ok": false, "error": "bad request id" })));
            }
            let r = cli(root, &["config", "show", &id])?;
            return Ok(ok_or_code(&r, 404, |s| json!({ "ok": true, "diff": s })));
        }
        ("proposals/accept", _, true) | ("proposals/decline", _, true) => {
            let id = body.get("id").and_then(Value::as_str).unwrap_or("");
            if !valid_request_id(id) {
                return Ok((400, json!({ "ok": false, "error": "bad request id" })));
            }
            let verb = if tail.ends_with("accept") {
                "accept"
            } else {
                "decline"
            };
            cli(root, &["config", verb, id])?
        }
        ("profile", true, _) | ("profile", _, _) if tail == "profile" => {
            return admin_profile(root, method, query, body);
        }
        _ => {
            return Ok((
                404,
                json!({ "ok": false, "error": "unknown admin endpoint" }),
            ))
        }
    };
    // Shared shape for the simple mutating verbs above (approve/revoke, kits
    // add/unlink, config set, proposals accept/decline): {ok, output, error}.
    Ok(action_result(&r))
}

/// Build the `elanus provider add …` argv from a validated request body and run
/// it. The API KEY is NEVER placed on argv — it is piped on the CLI's stdin safe
/// path (`resolve_key`'s stdin fallback) so it stays off the process table and
/// out of any obs line. `kind=native` builds a no-secret native-login provider;
/// `kind=apikey` (default) needs base_url + a key. Extra headers ride as
/// repeated `--header Name=Value` (values encrypted at rest by the vault).
fn provider_add(root: &Root, body: &Value) -> (u16, Value) {
    let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let name = s("name");
    if !valid_provider_name(name) {
        return (
            400,
            json!({ "ok": false, "error": "bad provider name — use lowercase letters, digits, and hyphens" }),
        );
    }
    let kind = {
        let k = s("kind");
        if k.is_empty() { "apikey" } else { k }
    };
    let mut args: Vec<String> = vec!["provider".into(), "add".into(), name.into()];
    let mut stdin_key: Option<String> = None;
    match kind {
        "native" | "native_login" | "nativelogin" => {
            args.push("--native".into());
            let tool = s("tool");
            if !tool.is_empty() {
                args.push("--tool".into());
                args.push(tool.into());
            }
        }
        "apikey" | "api_key" => {
            let base_url = s("base_url");
            if base_url.is_empty() {
                return (400, json!({ "ok": false, "error": "an api-key provider needs a base URL" }));
            }
            let wire = s("wire");
            if !wire.is_empty() {
                args.push("--wire".into());
                args.push(wire.into());
            }
            args.push("--base-url".into());
            args.push(base_url.into());
            // Headers: accept [{name,value}] objects or "Name=Value" strings.
            if let Some(arr) = body.get("headers").and_then(Value::as_array) {
                for h in arr {
                    let pair = match h {
                        Value::String(s) => s.trim().to_string(),
                        Value::Object(_) => {
                            let n = h.get("name").and_then(Value::as_str).unwrap_or("").trim();
                            let v = h.get("value").and_then(Value::as_str).unwrap_or("");
                            if n.is_empty() {
                                continue;
                            }
                            format!("{n}={v}")
                        }
                        _ => continue,
                    };
                    if pair.is_empty() || !pair.contains('=') {
                        continue;
                    }
                    args.push("--header".into());
                    args.push(pair);
                }
            }
            // The key rides stdin (the CLI's off-argv safe path), never argv.
            let key = body.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() {
                return (400, json!({ "ok": false, "error": "an api-key provider needs a key" }));
            }
            stdin_key = Some(key.to_string());
        }
        other => {
            return (400, json!({ "ok": false, "error": format!("unknown provider kind {other:?}") }));
        }
    }
    let r = match cli_stdin(root, &args, stdin_key.as_deref()) {
        Ok(r) => r,
        Err(_) => return (500, json!({ "ok": false, "error": "provider add failed to run" })),
    };
    action_result(&r)
}

/// Provider names are lowercase `[a-z0-9][a-z0-9-]*`, ≤64 — the same gate the
/// vault enforces (src/provider.rs `valid_name`). Mirrored here so a bad name is
/// a clean 400 before any shell-out.
fn valid_provider_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// GET/PUT /api/admin/profile — read or validate-and-write a profile.toml. PUT
/// writes through `profile put` (the kernel validates, commits to config/live,
/// records the acceptance) so a malformed file can never silently vanish an
/// agent.
fn admin_profile(
    root: &Root,
    method: &ntex::http::Method,
    query: &str,
    body: &Value,
) -> Result<(u16, Value)> {
    use ntex::http::Method;
    let name = query_param_str(query, "name").unwrap_or_else(|| "default".into());
    if !valid_profile_name(&name) {
        return Ok((400, json!({ "ok": false, "error": BAD_NAME_MSG })));
    }
    if *method == Method::GET {
        let file = profile_toml_path(root, &name);
        let Ok(toml) = std::fs::read_to_string(&file) else {
            return Ok((
                404,
                json!({ "ok": false, "error": format!("no profile.toml for {name}") }),
            ));
        };
        let parsed = cli(root, &["profile", "get", &name])?;
        return Ok((
            200,
            json!({
                "ok": true,
                "name": name,
                "toml": toml,
                "profile": if parsed.ok { json_lines(&parsed.stdout).into_iter().next().unwrap_or(Value::Null) } else { Value::Null },
                "profile_error": if parsed.ok { Value::Null } else { Value::String(human_profile_error(&cli_err(&parsed))) },
            }),
        ));
    }
    if *method == Method::PUT {
        let Some(toml) = body.get("toml").and_then(Value::as_str) else {
            return Ok((400, json!({ "ok": false, "error": "need {toml}" })));
        };
        let tmp =
            std::env::temp_dir().join(format!("el-profile-candidate-{}.toml", std::process::id()));
        std::fs::write(&tmp, toml).context("writing profile candidate")?;
        let v = cli(root, &["profile", "put", &name, &tmp.display().to_string()])?;
        let _ = std::fs::remove_file(&tmp);
        return Ok(if v.ok {
            (200, json!({ "ok": true, "name": name }))
        } else {
            (
                400,
                json!({ "ok": false, "error": human_profile_error(&cli_err(&v)) }),
            )
        });
    }
    Ok((
        404,
        json!({ "ok": false, "error": "unknown admin endpoint" }),
    ))
}

// ---- kit package summary (port of server.mjs kitPackages/manifestSummary) ----

/// `{ ...kit, packages: [{name, dir, skill, manifest}] }` for the kit-preview
/// modal. Resolves the kit's dir from `kit list --json`, then reads each package
/// dir's SKILL.md (skill name/description) and elanus.toml (a typed manifest
/// summary). Returns None when the kit isn't found.
fn kit_packages(root: &Root, name: &str) -> Result<Option<Value>> {
    let listed = cli(root, &["kit", "list", "--json"])?;
    if !listed.ok {
        return Ok(None);
    }
    let Some(kit) = json_lines(&listed.stdout)
        .into_iter()
        .find(|k| k.get("name").and_then(Value::as_str) == Some(name))
    else {
        return Ok(None);
    };
    let Some(dir) = kit.get("dir").and_then(Value::as_str) else {
        return Ok(None);
    };
    let packages_dir = FsPath::new(dir).join("packages");
    let mut packages: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&packages_dir) {
        let mut dirs: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        dirs.sort();
        for ent in dirs {
            let pkg_dir = packages_dir.join(&ent);
            let skill = std::fs::read_to_string(pkg_dir.join("SKILL.md"))
                .ok()
                .map(|raw| {
                    let meta = frontmatter(&raw);
                    json!({
                        "name": meta.get("name").cloned().unwrap_or_else(|| ent.clone()),
                        "description": meta.get("description").cloned().unwrap_or_default(),
                    })
                })
                .unwrap_or(Value::Null);
            let manifest = std::fs::read_to_string(pkg_dir.join("elanus.toml"))
                .ok()
                .map(|raw| manifest_summary(&raw))
                .unwrap_or(Value::Null);
            packages.push(json!({
                "name": ent,
                "dir": pkg_dir.display().to_string(),
                "skill": skill,
                "manifest": manifest,
            }));
        }
    }
    let mut summary = kit;
    if let Value::Object(map) = &mut summary {
        map.insert("packages".into(), Value::Array(packages));
    }
    Ok(Some(summary))
}

/// Parse a leading `---\n…\n---` YAML-ish frontmatter block into key→value.
fn frontmatter(raw: &str) -> HashMap<String, String> {
    let mut meta = HashMap::new();
    let Some(rest) = raw.strip_prefix("---\n") else {
        return meta;
    };
    let Some(end) = rest.find("\n---") else {
        return meta;
    };
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            if !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                let v = v.trim().trim_matches(|c| c == '\'' || c == '"').trim();
                meta.insert(k.to_string(), v.to_string());
            }
        }
    }
    meta
}

/// A typed summary of a package's elanus.toml: actor role, request capabilities,
/// and a one-line description — the same shape server.mjs `manifestSummary` built
/// (used by the kit-preview modal to label package actors).
fn manifest_summary(raw: &str) -> Value {
    let scalar = |key: &str| -> Option<String> {
        for line in raw.lines() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let v = rest.trim().trim_matches('"');
                    return Some(v.to_string());
                }
            }
        }
        None
    };
    let count = |marker: &str| {
        raw.lines()
            .filter(|l| l.trim_start().starts_with(marker))
            .count()
    };
    let mode = scalar("mode");
    let run = scalar("run");
    let http = raw.lines().any(|l| {
        let t = l.trim();
        t == "http = true" || t == "http=true"
    });
    let hooks = count("[[hook]]");
    let stages = count("[[stage]]");
    let mcps = count("[[mcp]]");
    let comment = leading_comment(raw);

    let mut labels: Vec<String> = Vec::new();
    if let Some(m) = &mode {
        labels.push(if m == "daemon" {
            "actor daemon".into()
        } else {
            format!("{m} actor")
        });
    }
    if http {
        labels.push("http service".into());
    }
    if hooks > 0 {
        labels.push(format!("{hooks} hook{}", if hooks == 1 { "" } else { "s" }));
    }
    if stages > 0 {
        labels.push(format!(
            "{stages} stage{}",
            if stages == 1 { "" } else { "s" }
        ));
    }
    if mcps > 0 {
        labels.push(format!(
            "{mcps} mcp server{}",
            if mcps == 1 { "" } else { "s" }
        ));
    }
    let actor = if labels.is_empty() {
        Value::Null
    } else {
        Value::String(labels.join(", "))
    };
    let fallback = run
        .as_ref()
        .map(|r| match &mode {
            Some(m) => format!("Runs {r} as {m}."),
            None => format!("Runs {r}."),
        })
        .unwrap_or_default();
    let description = if comment.is_empty() {
        fallback
    } else {
        comment
    };

    json!({
        "actor": actor,
        "mode": mode,
        "run": run,
        "http": http,
        "request": {
            "subscribe": array_values(raw, "subscribe"),
            "publish": array_values(raw, "publish"),
            "blocking": array_values(raw, "blocking"),
            "fs_write": array_values(raw, "fs_write"),
        },
        "description": description,
    })
}

/// Leading `#` comment lines (the package's header doc), joined and whitespace-
/// collapsed — stops at the first non-comment line.
fn leading_comment(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        if !t.starts_with('#') {
            break;
        }
        lines.push(t.trim_start_matches('#').trim().to_string());
    }
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quoted string values from a single-line TOML array `key = ["a", "b"]`.
fn array_values(raw: &str, key: &str) -> Value {
    for line in raw.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                if let (Some(open), Some(close)) = (rest.find('['), rest.find(']')) {
                    if open < close {
                        let inner = &rest[open + 1..close];
                        let vals: Vec<Value> = inner
                            .split(',')
                            .filter_map(|s| {
                                let s = s.trim().trim_matches('"');
                                (!s.is_empty()).then(|| Value::String(s.to_string()))
                            })
                            .collect();
                        return Value::Array(vals);
                    }
                }
            }
        }
    }
    Value::Array(Vec::new())
}

// ---- static (embedded) ----------------------------------------------------

async fn static_file(req: HttpRequest) -> HttpResponse {
    let raw = req.path();
    let rel = if raw == "/" {
        "index.html"
    } else {
        raw.trim_start_matches('/')
    };
    // include_dir lookups can't traverse out of the embedded tree, but reject
    // obvious traversal so behavior matches the mjs static guard.
    if rel.split('/').any(|seg| seg == "..") {
        return text_resp(404, "not found");
    }
    match DIST.get_file(rel) {
        Some(f) => HttpResponse::Ok()
            .content_type(mime_for(rel))
            .body(f.contents().to_vec()),
        None => text_resp(404, "not found"),
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("js") | Some("mjs") => "text/javascript",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

// ---- shell-out + proxy helpers --------------------------------------------

struct CliOut {
    ok: bool,
    stdout: String,
    stderr: String,
    error: Option<String>,
}

/// Append a tagged observability line to `$ELANUS_WEB_LOG` when set (mjs parity).
/// Logging must never break a request, so all errors are swallowed.
fn weblog(tag: &str, msg: &str) {
    if let Ok(path) = std::env::var("ELANUS_WEB_LOG") {
        if !path.is_empty() {
            use std::io::Write as _;
            let line = format!("{} [web:{tag}] {msg}\n", chrono::Utc::now().to_rfc3339());
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }
}

fn cli_err(r: &CliOut) -> String {
    let stderr = r.stderr.trim();
    if !stderr.is_empty() {
        stderr.to_string()
    } else {
        r.error.clone().unwrap_or_default()
    }
}

/// Run THIS binary (current_exe) as the elanus CLI with ELANUS_ROOT set, exactly
/// as mjs ran the sibling binary — but in-process there is no node and no PATH
/// lookup. Provider credentials are inherited from the launching environment
/// (the web server already presents the owner identity).
fn cli(root: &Root, args: &[&str]) -> Result<CliOut> {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    cli_owned(root, &owned)
}

fn cli_owned(root: &Root, args: &[String]) -> Result<CliOut> {
    // Backend observability (mjs parity): one greppable line per gesture. Goes to
    // $ELANUS_WEB_LOG when set, matching server.mjs's `[web:cli] elanus …` format
    // so the same QA tail / e2e assertions work against this server.
    weblog("cli", &format!("elanus {}", args.join(" ")));
    let exe = std::env::current_exe().context("locating the running elanus binary")?;
    let out = std::process::Command::new(exe)
        .args(args)
        .env("ELANUS_ROOT", root.dir.display().to_string())
        .output()
        .context("spawning elanus")?;
    Ok(CliOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        error: if out.status.success() {
            None
        } else {
            Some(format!("exited {}", out.status))
        },
    })
}

/// Like `cli`, but feeds `stdin` (when Some) to the child on its stdin pipe — the
/// safe path for a secret (`provider add`'s key) so it never lands on argv or in
/// the `[web:cli]` obs line. The logged command line is the argv only (no key).
fn cli_stdin(root: &Root, args: &[String], stdin: Option<&str>) -> Result<CliOut> {
    use std::io::Write as _;
    weblog("cli", &format!("elanus {}", args.join(" ")));
    let exe = std::env::current_exe().context("locating the running elanus binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env("ELANUS_ROOT", root.dir.display().to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn().context("spawning elanus")?;
    if let Some(secret) = stdin {
        if let Some(mut sink) = child.stdin.take() {
            sink.write_all(secret.as_bytes())
                .context("piping the provider key on stdin")?;
            // Drop closes the pipe so the child's stdin read sees EOF.
        }
    }
    let out = child.wait_with_output().context("awaiting elanus")?;
    Ok(CliOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        error: if out.status.success() {
            None
        } else {
            Some(format!("exited {}", out.status))
        },
    })
}

/// {ok, output, error} — the shape mjs returns for simple mutating verbs.
fn action_result(r: &CliOut) -> (u16, Value) {
    if r.ok {
        (200, json!({ "ok": true, "output": r.stdout }))
    } else {
        (
            400,
            json!({ "ok": false, "output": r.stdout, "error": cli_err(r) }),
        )
    }
}

/// Profile mutations translate the kernel's error text to product language.
fn profile_result(r: &CliOut) -> (u16, Value) {
    if r.ok {
        (200, json!({ "ok": true, "output": r.stdout }))
    } else {
        (
            400,
            json!({ "ok": false, "error": human_profile_error(&cli_err(r)) }),
        )
    }
}

fn ok_or_err(r: &CliOut, err_code: u16, ok: impl FnOnce(&str) -> Value) -> (u16, Value) {
    if r.ok {
        (200, ok(&r.stdout))
    } else {
        (err_code, json!({ "ok": false, "error": cli_err(r) }))
    }
}

fn ok_or_code(r: &CliOut, err_code: u16, ok: impl FnOnce(&str) -> Value) -> (u16, Value) {
    if r.ok {
        (200, ok(&r.stdout))
    } else {
        (err_code, json!({ "ok": false, "error": cli_err(r) }))
    }
}

/// POST the query DSL to the history package and return (status, body). A fresh
/// current-thread runtime drives reqwest on this blocking-pool thread (no nested
/// runtime — `web::block` runs us off the ntex reactor).
fn proxy_history(base: &str, query: &Value) -> Result<(u16, String)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let res = reqwest::Client::new()
            .post(format!("{base}/query"))
            .header("content-type", "application/json")
            .json(query)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;
        let code = res.status().as_u16();
        let body = res.text().await?;
        Ok((code, body))
    })
}

fn history_endpoint(root: &Root) -> Option<String> {
    let path = root.run_dir().join("pkg-history").join("http.json");
    let j: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let port = j.get("port").and_then(Value::as_u64)?;
    Some(format!("http://127.0.0.1:{port}"))
}

// ---- small helpers --------------------------------------------------------

const BAD_NAME_MSG: &str = "names can use letters, numbers, dashes and underscores — no spaces";

fn json_resp(code: u16, v: Value) -> HttpResponse {
    HttpResponse::build(status_code(code))
        .content_type("application/json")
        .body(v.to_string())
}

fn text_resp(code: u16, body: &str) -> HttpResponse {
    HttpResponse::build(status_code(code)).body(body.to_string())
}

fn status_code(code: u16) -> ntex::http::StatusCode {
    ntex::http::StatusCode::from_u16(code).unwrap_or(ntex::http::StatusCode::INTERNAL_SERVER_ERROR)
}

fn query_param(req: &HttpRequest, key: &str) -> Option<String> {
    query_param_str(req.uri().query()?, key)
}

fn query_param_str(q: &str, key: &str) -> Option<String> {
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            let raw = it.next().unwrap_or("");
            return Some(percent_decode(raw));
        }
    }
    None
}

/// Minimal application/x-www-form-urlencoded decode for query values: `+`→space
/// and `%XX`→byte. Good enough for the agent/profile/kit/id params we accept.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Browser-borne threats a terminal doesn't have: a hostile page POSTing to
/// localhost (CSRF) and DNS rebinding. Mutations require a genuinely-local Host
/// and — when a browser supplies Origin — an Origin whose host is ALSO local
/// (any loopback port). The `elanus dev` loop serves the UI from Vite on one
/// loopback port and proxies to the relay on another, a legitimate same-machine
/// cross-origin; requiring a byte-equal Origin==Host (incl. port) wrongly refused
/// every dev mutation. A FOREIGN Origin (evil.com) is still refused here, and a
/// rebound Host (evil.com → 127.0.0.1) fails the Host check above — so neither CSRF
/// nor DNS-rebinding is weakened. curl and local agents send no Origin and pass
/// (entry 3 already owns local processes).
fn origin_ok(req: &HttpRequest) -> bool {
    let headers = req.headers();
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !host_is_local(host) {
        return false;
    }
    match headers.get("origin").and_then(|h| h.to_str().ok()) {
        None => true,
        // The Origin's host need only be LOCAL, not byte-equal to Host: the Vite
        // dev proxy is a local cross-PORT origin. A foreign Origin is still refused.
        Some(origin) => origin_host(origin)
            .map(|h| host_is_local(&h))
            .unwrap_or(false),
    }
}

fn host_is_local(host: &str) -> bool {
    let bare = host.split(':').next().unwrap_or("");
    matches!(bare, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

fn origin_host(origin: &str) -> Option<String> {
    // origin is scheme://host[:port]; strip the scheme, take up to the next '/'.
    let after = origin.split("://").nth(1)?;
    Some(after.split('/').next().unwrap_or("").to_string())
}

fn db_path(root: &Root) -> Option<PathBuf> {
    let p = root.db();
    p.exists().then_some(p)
}

fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_pkg_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

fn profiles_with_helper(root: &Root, text: &str) -> Vec<Value> {
    let mut profiles = json_lines(text);
    let has_helper = profiles
        .iter()
        .any(|p| p["profile"].as_str() == Some("helper"));
    if has_helper {
        return profiles;
    }
    let Some(default) = profiles
        .iter()
        .find(|p| p["profile"].as_str() == Some("default"))
        .cloned()
        .or_else(|| profiles.first().cloned())
    else {
        return profiles;
    };
    let mut helper = default;
    if let Some(obj) = helper.as_object_mut() {
        obj.insert("profile".into(), json!("helper"));
        obj.insert("mirrors".into(), json!("default"));
        obj.insert(
            "dir".into(),
            json!(root.profile_dir("helper").display().to_string()),
        );
    }
    profiles.push(helper);
    profiles.sort_by(|a, b| {
        a["profile"]
            .as_str()
            .unwrap_or("")
            .cmp(b["profile"].as_str().unwrap_or(""))
    });
    profiles
}

fn profile_toml_path(root: &Root, name: &str) -> PathBuf {
    let canonical = root.config_agents().join(name).join("profile.toml");
    if canonical.exists() {
        canonical
    } else {
        root.profile_dir(name).join("profile.toml")
    }
}

/// The product says "agents"; the kernel says "profiles". Translate at the
/// boundary (docs/layering.md) so a person sees plain language.
fn human_profile_error(raw: &str) -> String {
    let s = raw.trim_start_matches("error:").trim();
    if s.is_empty() {
        return "that did not work".into();
    }
    if s.to_lowercase().contains("bad profile name") {
        return BAD_NAME_MSG.into();
    }
    s.replace("profiles", "agents").replace("profile", "agent")
}

/// Encode a JSON value as TOML value text for `profile set k=v`: arrays become
/// real TOML arrays, strings get quoted, scalars pass bare.
fn toml_value(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(toml_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{ {} }}",
            o.iter()
                .map(|(k, val)| format!("{k} = {}", toml_value(val)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}")),
        other => other.to_string(),
    }
}

// ---- conversation projection (port of server.mjs) -------------------------
//
// Read-only sqlite. The SAME logical message reaches the converse feed from up
// to three sources (live bus tail, the durable `messages` table, the in/* event
// projection) whose ids never line up; dedup by (class, text) — and
// (type, correlation) for asks/failures — is the only attribute all share. Keep
// `conv_key` in lockstep with convMessageKey in App.tsx.

fn open_ro(db: &FsPath) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

/// One ledger event row, columns read loosely (correlation_id / payload may be
/// text or null) to match the JS that treated them as untyped.
struct EventRow {
    id: i64,
    correlation_id: Option<String>,
    payload: Option<String>,
    sender: Option<String>,
    created_at: Option<String>,
}

fn col_string(row: &rusqlite::Row, idx: usize) -> Option<String> {
    match row.get_ref(idx).ok()? {
        rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        rusqlite::types::ValueRef::Integer(i) => Some(i.to_string()),
        rusqlite::types::ValueRef::Real(f) => Some(f.to_string()),
        _ => None,
    }
}

// SELECT id, type, correlation_id, payload, state, sender, created_at FROM events
fn map_event(row: &rusqlite::Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get::<_, i64>(0)?,
        correlation_id: col_string(row, 2),
        payload: col_string(row, 3),
        sender: col_string(row, 5),
        created_at: col_string(row, 6),
    })
}

fn parse_payload(raw: &Option<String>) -> Value {
    raw.as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn parse_stored(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn message_text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => {
            if let Some(t) = content.get("text").and_then(Value::as_str) {
                return t.to_string();
            }
            if let Some(c) = content.get("content").and_then(Value::as_str) {
                return c.to_string();
            }
            if content.get("truncated") == Some(&Value::Bool(true)) {
                if let Some(p) = content.get("preview") {
                    if !p.is_null() {
                        return p
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| p.to_string());
                    }
                }
            }
            content.to_string()
        }
    }
}

fn truncate_text(value: &str, max: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max {
        let kept: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        collapsed
    }
}

fn short_iso(value: &str) -> String {
    value.replacen('T', " ", 1).chars().take(16).collect()
}

fn session_for_event(row: &EventRow) -> String {
    let payload = parse_payload(&row.payload);
    if let Some(s) = payload.get("session").and_then(Value::as_str) {
        return s.to_string();
    }
    match &row.correlation_id {
        Some(c) => format!("evt-{c}"),
        None => format!("evt-{}", row.id),
    }
}

// A conversation is dropped from the comms list only when it is a *worker
// session* — a coding run, identified by its bus-derived `code-*` session id —
// NOT by the agent's noun (docs/handoffs/chat-rendering.md M2). Gating on noun
// (codex/claude-code) wrongly drops a coding-noun agent's genuine comms-plane
// conversation (an `in/agent/<agent>` prompt on a non-`code-*` session with a
// correlated `in/human/<owner>` reply); the decision must be derivable from the
// ledger shape alone so a third-party UI reproduces it. All real coding runs
// carry `code-*` sessions, so they stay evicted; a curated conversation under
// any agent (coding-noun or not) on a non-`code-*` session is preserved.
fn is_worker_session(session: &str) -> bool {
    session.starts_with("code-") && session.len() > 5
}

fn source_for(session: &str, sender: &Option<String>, payload: &Value, owner: &str) -> String {
    if let Some(claimed) = payload.get("source").and_then(Value::as_str) {
        let claimed = claimed.trim().to_lowercase();
        if !claimed.is_empty() {
            return claimed;
        }
    }
    let s = session.to_lowercase();
    let from = sender.as_deref().unwrap_or("").to_lowercase();
    if s.starts_with("web-") {
        return "web".into();
    }
    if from.contains("github") || from.contains("jira") || from.contains("linear") {
        return "github".into();
    }
    if from.contains("cron") || from.contains("timer") || from.contains("schedule") {
        return "cron".into();
    }
    if from.is_empty() || from == owner.to_lowercase() || from == "owner" {
        return "you".into();
    }
    let cleaned: String = from
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned: String = cleaned.chars().take(20).collect();
    if cleaned.is_empty() {
        "you".into()
    } else {
        cleaned
    }
}

#[derive(Default, Clone)]
struct Conv {
    title: String,
    source: String,
    last_ts: String,
    message_count: u64,
    preview: String,
    last_role: String,
    first_ts: String,
}

struct Convs {
    map: HashMap<String, Conv>,
    order: Vec<String>,
}

impl Convs {
    fn new() -> Self {
        Convs {
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn ensure(&mut self, session: &str, seed_source: Option<&str>, seed_ts: &str) -> &mut Conv {
        if !self.map.contains_key(session) {
            self.order.push(session.to_string());
            self.map.insert(
                session.to_string(),
                Conv {
                    source: seed_source.unwrap_or("you").to_string(),
                    last_ts: seed_ts.to_string(),
                    first_ts: seed_ts.to_string(),
                    ..Default::default()
                },
            );
        }
        self.map.get_mut(session).unwrap()
    }

    fn touch(&mut self, session: &str, role: &str, text: &str, count: bool, ts: &str) {
        if text.is_empty() {
            return;
        }
        let item = self.ensure(session, None, ts);
        if item.title.is_empty() && role == "you" {
            item.title = truncate_text(text, 72);
        }
        item.preview = truncate_text(text, 110);
        item.last_role = role.to_string();
        if !ts.is_empty() {
            item.last_ts = ts.to_string();
        }
        if item.first_ts.is_empty() {
            item.first_ts = ts.to_string();
        }
        if count {
            item.message_count += 1;
        }
    }
}

fn conversation_rows(agent: &str, db: &FsPath, owner: &str) -> Result<Value> {
    let conn = open_ro(db)?;
    // `source_for` labels the owner's own messages as "you"; it also matches the
    // literal "owner", so a default root works either way, but a renamed owner
    // (.secrets/.owner-name) is honored here exactly as server.mjs does.
    let mut convs = Convs::new();
    let mut corr_to_session: HashMap<String, String> = HashMap::new();

    let inbound = {
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id, payload, state, sender, created_at FROM events WHERE type = ? ORDER BY id ASC LIMIT 5000",
        )?;
        let rows = stmt.query_map([format!("in/agent/{agent}")], map_event)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    for row in &inbound {
        let payload = parse_payload(&row.payload);
        let session = session_for_event(row);
        if is_worker_session(&session) {
            continue;
        }
        if let Some(c) = &row.correlation_id {
            corr_to_session.insert(c.clone(), session.clone());
        }
        let source = source_for(&session, &row.sender, &payload, owner);
        let created = row.created_at.clone().unwrap_or_default();
        convs.ensure(&session, Some(&source), &created);
        if let Some(prompt) = payload
            .get("prompt")
            .and_then(Value::as_str)
            .or_else(|| payload.get("text").and_then(Value::as_str))
        {
            convs.touch(&session, "you", prompt, true, &created);
        }
    }

    if !corr_to_session.is_empty() {
        let corrs: Vec<String> = corr_to_session.keys().cloned().collect();
        let human_rows = query_human_by_corr(&conn, &corrs, 5000)?;
        for row in &human_rows {
            let Some(corr) = &row.correlation_id else {
                continue;
            };
            let Some(session) = corr_to_session.get(corr).cloned() else {
                continue;
            };
            let payload = parse_payload(&row.payload);
            let created = row.created_at.clone().unwrap_or_default();
            if payload.get("failed").is_some_and(truthy) {
                let err = payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("the agent failed");
                convs.touch(&session, "failed", err, true, &created);
            } else if payload.get("question").is_some_and(|v| !v.is_null()) {
                let q = payload
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                convs.touch(&session, "ask", q, true, &created);
            } else if let Some(t) = payload.get("text").and_then(Value::as_str) {
                convs.touch(&session, "agent", t, true, &created);
            } else if let Some(a) = payload.get("answer").filter(|v| !v.is_null()) {
                let a = a
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| a.to_string());
                convs.touch(&session, "you", &a, true, &created);
            }
        }

        let sessions: Vec<String> = convs.order.clone();
        if !sessions.is_empty() {
            let placeholders = placeholders(sessions.len());
            let sql = format!(
                "SELECT m.id, m.session_id, m.role, m.content, m.event_id, m.created_at, e.correlation_id, e.type AS event_type \
                   FROM messages m LEFT JOIN events e ON m.event_id = e.id \
                  WHERE m.session_id IN ({placeholders}) \
                  ORDER BY m.id ASC LIMIT 5000"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(sessions.iter()), |row| {
                Ok((
                    col_string(row, 1).unwrap_or_default(), // session_id
                    col_string(row, 2).unwrap_or_default(), // role
                    col_string(row, 3),                     // content
                    col_string(row, 5).unwrap_or_default(), // created_at
                ))
            })?;
            for r in rows {
                let (session_id, role, content, created) = r?;
                let text = content
                    .as_deref()
                    .map(|c| message_text(&parse_stored(c)))
                    .unwrap_or_default();
                let role = normalize_role(&role);
                // count=false: turns already counted from the in/agent prompt +
                // in/human reply events; counting messages too double-counts.
                convs.touch(&session_id, &role, &text, false, &created);
            }
        }
    }

    let mut out: Vec<Value> = convs
        .order
        .iter()
        .map(|k| {
            let c = &convs.map[k];
            let source = if c.source.is_empty() { "you" } else { &c.source };
            let title = if c.title.is_empty() {
                let when = short_iso(if !c.first_ts.is_empty() { &c.first_ts } else { &c.last_ts });
                format!("{source} conversation {when}").trim().to_string()
            } else {
                c.title.clone()
            };
            json!({
                "session": k,
                "agent": agent,
                "title": title,
                "source": source,
                "last_ts": if !c.last_ts.is_empty() { c.last_ts.clone() } else { c.first_ts.clone() },
                "message_count": c.message_count,
                "preview": c.preview,
                "last_role": c.last_role,
            })
        })
        .collect();
    // Sort by last_ts desc (stable — ties keep insertion order), top 100.
    out.sort_by(|a, b| {
        let av = a.get("last_ts").and_then(Value::as_str).unwrap_or("");
        let bv = b.get("last_ts").and_then(Value::as_str).unwrap_or("");
        bv.cmp(av)
    });
    out.truncate(100);
    Ok(Value::Array(out))
}

fn conversation_messages(session: &str, db: &FsPath) -> Result<Value> {
    let conn = open_ro(db)?;
    let mut messages: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    {
        let mut stmt = conn.prepare(
            "SELECT id, role, content, event_id, created_at FROM messages WHERE session_id = ? ORDER BY id ASC LIMIT 2000",
        )?;
        let rows = stmt.query_map([session], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                col_string(row, 1).unwrap_or_default(),
                col_string(row, 2),
                col_string(row, 3),
                col_string(row, 4).unwrap_or_default(),
            ))
        })?;
        for r in rows {
            let (id, role, content, event_id, created) = r?;
            // Converse is the human chat: only you/agent turns belong here.
            if role != "user" && role != "assistant" {
                continue;
            }
            let text = content
                .as_deref()
                .map(|c| message_text(&parse_stored(c)))
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let cls = if role == "user" { "you" } else { "agent" };
            add_message(
                &mut messages,
                &mut seen,
                json!({
                    "id": format!("m-{id}"),
                    "type": "msg",
                    "who": cls,
                    "cls": cls,
                    "text": text,
                    "ts": created,
                    "event_id": event_id,
                }),
            );
        }
    }

    let is_evt = session.starts_with("evt-");
    let agent_rows: Vec<EventRow> = if is_evt {
        let suffix = &session[4..];
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id, payload, state, sender, created_at FROM events WHERE type LIKE 'in/agent/%' AND (correlation_id = ? OR id = ?) ORDER BY id ASC LIMIT 4000",
        )?;
        let rows = stmt
            .query_map([suffix, suffix], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    } else {
        let like = format!("%\"session\":\"{session}\"%");
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id, payload, state, sender, created_at FROM events WHERE type LIKE 'in/agent/%' AND payload LIKE ? ORDER BY id ASC LIMIT 4000",
        )?;
        let rows = stmt
            .query_map([like], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut corrs: Vec<String> = Vec::new();
    for row in &agent_rows {
        if session_for_event(row) != session {
            continue;
        }
        if let Some(c) = &row.correlation_id {
            if !corrs.contains(c) {
                corrs.push(c.clone());
            }
        }
        let payload = parse_payload(&row.payload);
        if let Some(text) = payload
            .get("prompt")
            .and_then(Value::as_str)
            .or_else(|| payload.get("text").and_then(Value::as_str))
        {
            add_message(
                &mut messages,
                &mut seen,
                json!({
                    "id": format!("e-{}", row.id),
                    "type": "msg",
                    "who": "you",
                    "cls": "you",
                    "text": text,
                    "corr": row.correlation_id,
                    "ts": row.created_at,
                    "event_id": row.id,
                }),
            );
        }
    }

    if !corrs.is_empty() {
        let human_rows = query_human_by_corr(&conn, &corrs, 4000)?;
        for row in &human_rows {
            let payload = parse_payload(&row.payload);
            if payload.get("failed").is_some_and(truthy) {
                add_message(
                    &mut messages,
                    &mut seen,
                    json!({
                        "id": format!("e-{}", row.id),
                        "key": format!("event:{}:failed", row.id),
                        "type": "msg",
                        "who": "agent failed",
                        "cls": "failed",
                        "text": payload.get("error").and_then(Value::as_str).unwrap_or("the agent failed with no detail."),
                        "corr": row.correlation_id,
                        "failed": true,
                        "ts": row.created_at,
                        "event_id": row.id,
                    }),
                );
            } else if payload.get("question").is_some_and(|v| !v.is_null()) {
                add_message(
                    &mut messages,
                    &mut seen,
                    json!({
                        "id": format!("e-{}", row.id),
                        "key": format!("event:{}:ask", row.id),
                        "type": "ask",
                        "corr": row.correlation_id,
                        "payload": payload,
                        "answered": Value::Null,
                        "ts": row.created_at,
                        "event_id": row.id,
                    }),
                );
            } else if let Some(t) = payload.get("text").and_then(Value::as_str) {
                add_message(
                    &mut messages,
                    &mut seen,
                    json!({
                        "id": format!("e-{}", row.id),
                        "key": format!("event:{}:agent", row.id),
                        "type": "msg",
                        "who": "agent",
                        "cls": "agent",
                        "text": t,
                        "corr": row.correlation_id,
                        "ts": row.created_at,
                        "event_id": row.id,
                    }),
                );
            }
        }
    }

    messages.sort_by(|a, b| {
        let av = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let bv = b.get("ts").and_then(Value::as_str).unwrap_or("");
        av.cmp(bv)
    });
    Ok(Value::Array(messages))
}

fn query_human_by_corr(
    conn: &rusqlite::Connection,
    corrs: &[String],
    limit: i64,
) -> Result<Vec<EventRow>> {
    if corrs.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(corrs.len());
    let sql = format!(
        "SELECT id, type, correlation_id, payload, state, sender, created_at FROM events WHERE type LIKE 'in/human/%' AND correlation_id IN ({placeholders}) ORDER BY id ASC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(corrs.iter()), map_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Content-identity dedup key. Mirrors convMessageKey / convKey in the SPA and
/// server.mjs: asks key by correlation, failures by correlation, everything else
/// by (class, text).
fn conv_key(m: &Value) -> String {
    let who = m.get("who").and_then(Value::as_str).unwrap_or("");
    let cls = m
        .get("cls")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if who == "you" {
                "you".into()
            } else {
                "agent".into()
            }
        });
    let corr = m.get("corr").and_then(Value::as_str);
    let event_id = m.get("event_id").map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    let id_part = corr.map(str::to_string).or(event_id).unwrap_or_default();
    if m.get("type").and_then(Value::as_str) == Some("ask") {
        return format!("ask:{id_part}");
    }
    if cls == "failed" {
        return format!("failed:{id_part}");
    }
    let text = m.get("text").and_then(Value::as_str).unwrap_or("");
    format!("{cls}:{text}")
}

fn add_message(list: &mut Vec<Value>, seen: &mut HashSet<String>, mut msg: Value) {
    // Always recompute the dedup key from message content and ignore any
    // explicit `key` field — matches node's addConversationMessage, which calls
    // convKey(msg) unconditionally. Honoring an explicit `key` would diverge:
    // an agent reply present in both the messages table (`agent:<text>`) and the
    // in/human event projection (`event:N:agent`) would key differently and
    // emit the same logical message twice in the server payload.
    let key = conv_key(&msg);
    if seen.contains(&key) {
        return;
    }
    seen.insert(key.clone());
    if let Value::Object(map) = &mut msg {
        map.insert("key".into(), Value::String(key));
    }
    list.push(msg);
}

fn normalize_role(role: &str) -> String {
    match role {
        "user" => "you".into(),
        "assistant" => "agent".into(),
        other => other.into(),
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    // M1/M3/M4: the comms/blocks read routes shell a CLI projection that prints a
    // JSON array; `map_cli_json` is the mapping the route bodies rely on. Empty
    // stdout → the `[]` default (an empty projection is data, never an error),
    // mirroring how `code_sessions` treats an empty list. Valid JSON passes
    // through verbatim so the CLI's `MailRow`/`RoomRow`/`BlockRow` shape reaches
    // the browser unchanged.
    #[test]
    fn array_route_maps_cli_json_through() {
        // Empty → the array default.
        let (code, v) = map_cli_json("", "[]");
        assert_eq!(code, 200);
        assert_eq!(v, json!([]));
        // Whitespace-only is still empty.
        let (_, v) = map_cli_json("   \n", "[]");
        assert_eq!(v, json!([]));
        // A real mail projection passes through unchanged (from/to/priority/state/
        // failed threaded by the CLI).
        let mail = r#"[{"id":7,"from":"code-a","to":"code-b","to_noun":"claude-code","correlation":"c1","priority":9,"state":"pending","failed":true,"mid_cycle":true,"preview":"urgent","ts":"2026-06-24T00:00:00Z"}]"#;
        let (code, v) = map_cli_json(mail, "[]");
        assert_eq!(code, 200);
        assert_eq!(v[0]["from"], "code-a");
        assert_eq!(v[0]["priority"], 9);
        assert_eq!(v[0]["failed"], true);
        assert_eq!(v[0]["mid_cycle"], true);
        // Garbage stdout is a 500 (bad projection output), not a silent pass.
        let (code, _) = map_cli_json("not json", "[]");
        assert_eq!(code, 500);
    }

    // M5: the estimate route uses the `null` default so a session with no estimate
    // returns 200 with body `null` (the runs view then omits the estimate group),
    // never a 404 or a crash. A real Report passes through.
    #[test]
    fn estimate_route_null_default_and_passthrough() {
        let (code, v) = map_cli_json("null", "null");
        assert_eq!(code, 200);
        assert_eq!(v, Value::Null);
        let (code, v) = map_cli_json("", "null");
        assert_eq!(code, 200);
        assert_eq!(v, Value::Null);
        let report = r#"{"session":"code-x","dollars":{"estimate":0.4,"actual":0.6,"delta":0.2},"turns":{"estimate":8.0,"actual":13.0,"delta":5.0},"tool_calls":{"actual":20.0},"tokens":{},"wall_clock_ms":{},"dollars_unavailable":false}"#;
        let (code, v) = map_cli_json(report, "null");
        assert_eq!(code, 200);
        assert_eq!(v["session"], "code-x");
        assert_eq!(v["dollars"]["delta"], 0.2);
        assert_eq!(v["dollars_unavailable"], false);
    }

    // The session-id guard the blocks/estimate routes apply before shelling the
    // CLI (no `%`/`\`/`"`/NUL — the same gate `conversation` uses).
    #[test]
    fn session_id_guard_rejects_unsafe_names() {
        assert!(valid_session_id("code-2af51b7e"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("code-..%2Fowner"));
        assert!(!valid_session_id("code\"x"));
        assert!(!valid_session_id("code\\x"));
        assert!(!valid_session_id(&"x".repeat(200)));
    }

    // agent-comms-ui follow-on: the guarded block-editor write. `block_set_args`
    // builds the `elanus block set ... --by ui` argv a valid edit shells, preserving
    // the (scope, owner, name) key and stamping the `--by ui` attribution trail.
    #[test]
    fn block_set_args_builds_attributed_write() {
        let body = json!({
            "session": "code-2af51b7e",
            "name": "note",
            "owner": "claude-code",
            "scope": "agent",
            "content": "edited from the UI",
            "priority": 5
        });
        let args = block_set_args(&body).expect("valid durable edit");
        // The verb + key are preserved.
        assert_eq!(&args[0..4], &["block", "set", "note", "edited from the UI"]);
        // The write is attributed `--by ui` (the decided-by trail for context_build_log).
        let mut it = args.iter();
        assert!(it.any(|a| a == "--by"));
        assert!(args.iter().any(|a| a == "ui"));
        // The key fields ride through so (scope, owner, name) is unchanged.
        assert!(args.windows(2).any(|w| w == ["--owner", "claude-code"]));
        assert!(args.windows(2).any(|w| w == ["--scope", "agent"]));
        assert!(args.windows(2).any(|w| w == ["--session", "code-2af51b7e"]));
        assert!(args.windows(2).any(|w| w == ["--priority", "5"]));
    }

    // EPHEMERAL inbox/channel blocks are owner-less (decision 2) — the write path
    // refuses them, and the bad-input cases are rejected before any shell-out.
    #[test]
    fn block_set_args_rejects_ephemeral_and_bad_input() {
        // Owner-less (ephemeral inbox/channel) block — not editable.
        let eph = json!({ "session": "code-1", "name": "inbox", "owner": "", "content": "x" });
        assert!(block_set_args(&eph).is_err());
        // Missing content (a malformed request must not silently write empty).
        let no_content = json!({ "session": "code-1", "name": "note", "owner": "a" });
        assert!(block_set_args(&no_content).is_err());
        // Unsafe session id is rejected by the same guard the read routes use.
        let bad_sess =
            json!({ "session": "code\"x", "name": "note", "owner": "a", "content": "y" });
        assert!(block_set_args(&bad_sess).is_err());
        // Empty content IS allowed (clearing a block is a legitimate edit).
        let ok = json!({ "session": "code-1", "name": "note", "owner": "a", "content": "" });
        assert!(block_set_args(&ok).is_ok());
    }

    // model-providers M4: the provider-name gate the admin routes apply before
    // any `elanus provider …` shell-out — the same lowercase `[a-z0-9][a-z0-9-]*`
    // (≤64) the vault enforces, so a bad name is a clean 400, never an injection.
    #[test]
    fn provider_name_guard() {
        assert!(valid_provider_name("deepseek"));
        assert!(valid_provider_name("gpt-5-litellm"));
        assert!(valid_provider_name("a"));
        assert!(!valid_provider_name(""));
        assert!(!valid_provider_name("Deepseek")); // no uppercase
        assert!(!valid_provider_name("-leading")); // must start alnum
        assert!(!valid_provider_name("has_underscore"));
        assert!(!valid_provider_name("has space"));
        assert!(!valid_provider_name("dot.name"));
        assert!(!valid_provider_name(&"x".repeat(65)));
    }

    // model-providers M4: `provider_add` builds the `elanus provider add …` argv
    // and — crucially — keeps the api KEY off argv (it rides stdin). Exercise the
    // body→argv shaping and the fail-closed validation without spawning the CLI.
    #[test]
    fn provider_add_validates_and_keeps_key_off_argv() {
        // A bad name is a clean 400 before any shell-out.
        let (code, _) = provider_add(&Root { dir: "/tmp/x".into() }, &json!({ "name": "BAD", "kind": "apikey" }));
        assert_eq!(code, 400);
        // An api-key provider with no base URL is refused.
        let (code, _) = provider_add(&Root { dir: "/tmp/x".into() }, &json!({ "name": "p", "kind": "apikey", "key": "sk-x" }));
        assert_eq!(code, 400);
        // An api-key provider with no key is refused (the key never defaults).
        let (code, _) = provider_add(&Root { dir: "/tmp/x".into() }, &json!({ "name": "p", "kind": "apikey", "base_url": "https://h/anthropic" }));
        assert_eq!(code, 400);
    }

    // The CSRF/DNS-rebinding guard `origin_ok` enforces on mutations, via its
    // `host_is_local`/`origin_host` predicates: a local Host is required; when a
    // browser supplies Origin, that Origin's host must be LOCAL (any loopback port,
    // so the Vite dev proxy passes) but a FOREIGN origin is refused.
    #[test]
    fn origin_guard_allows_local_cross_port_refuses_foreign() {
        // Local host, no Origin (curl / local agent) → allowed (host check only).
        assert!(host_is_local("127.0.0.1:8080"));
        // Same-origin browser POST → Origin host is local. allowed.
        assert!(host_is_local(&origin_host("http://127.0.0.1:7182/").unwrap()));
        // Vite dev proxy: UI on :5174, relay on :7182 — a local cross-PORT origin.
        // Different port, but still local → origin_ok accepts it (the dev-loop fix).
        let vite = origin_host("http://127.0.0.1:5174/").unwrap();
        assert_ne!(vite, "127.0.0.1:7182");
        assert!(host_is_local(&vite));
        // A foreign Origin (an attacker page) is refused: its host is not local.
        assert!(!host_is_local(&origin_host("http://evil.example/page").unwrap()));
        // A rebound / non-local Host is refused outright by the Host check.
        assert!(!host_is_local("evil.example"));
    }
}

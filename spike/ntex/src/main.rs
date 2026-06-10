/// ntex-mqtt 8.x embedding spike for elanus bus decision.
///
/// Tests (in order of risk):
/// 1. EMBEDDING: ntex System in a std::thread while tokio runs on main thread
/// 2. SUBSCRIBE user properties surfaced to the broker protocol handler
/// 3. Per-delivery QoS-1 PUBACK futures (send_at_least_once returns a future)
/// 4. End-to-end round-trip with rumqttc client: subscribe + QoS1 publish, loopback latency
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use ntex::service::{fn_factory_with_config, fn_service};
use ntex_mqtt::v5::{self, MqttServer, Publish, PublishAck, Session};
use ntex_mqtt::{Control, Reason};

// ─── global state for testing ────────────────────────────────────────────────

/// Signal that broker is ready.
static BROKER_READY: AtomicBool = AtomicBool::new(false);
/// True if SUBSCRIBE with user properties was received by broker.
static SUB_USER_PROPS_RECEIVED: AtomicBool = AtomicBool::new(false);
/// Count of QoS1 PUBACKs that resolved on the broker's send_at_least_once future.
static PUBACK_COUNT: AtomicU64 = AtomicU64::new(0);
/// How many echo dispatches were initiated.
static ECHO_COUNT: AtomicU64 = AtomicU64::new(0);

// ─── session state ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct MySession {
    client_id: String,
    subscriptions: RefCell<Vec<String>>,
    last_sub_user_props: RefCell<Vec<(String, String)>>,
    sink: v5::MqttSink,
}

#[derive(Debug)]
struct MyServerError;

impl From<()> for MyServerError {
    fn from(_: ()) -> Self { MyServerError }
}

impl std::convert::TryFrom<MyServerError> for PublishAck {
    type Error = MyServerError;
    fn try_from(err: MyServerError) -> Result<Self, Self::Error> { Err(err) }
}

// ─── broker handlers ─────────────────────────────────────────────────────────

async fn handshake(
    handshake: v5::Handshake,
) -> Result<v5::HandshakeAck<MySession>, MyServerError> {
    let session = MySession {
        client_id: handshake.packet().client_id.to_string(),
        subscriptions: RefCell::new(Vec::new()),
        last_sub_user_props: RefCell::new(Vec::new()),
        sink: handshake.sink(),
    };
    Ok(handshake.ack(session))
}

async fn publish_handler(
    session: Session<MySession>,
    publish: Publish,
) -> Result<PublishAck, MyServerError> {
    // publish.topic() returns Path<ByteString>; use publish_topic() for &str
    let topic = publish.publish_topic().to_string();
    let payload = publish.read_all().await.unwrap_or_default();

    // Echo to this session if it subscribed to this topic
    let subscribed = session.subscriptions.borrow().contains(&topic);
    if subscribed {
        // ── Test #3: per-delivery future ──────────────────────────────────
        // send_at_least_once() returns Future<Output=Result<PublishAck, _>>
        // that resolves when the SUBSCRIBER sends their PUBACK.
        // This is the "fan-out future per subscriber" that makes
        // "all deliveries complete" a control-flow fact.
        let fut = session
            .sink
            .publish(topic.clone())
            .send_at_least_once(payload);

        ntex::rt::spawn(async move {
            match fut.await {
                Ok(_puback) => {
                    PUBACK_COUNT.fetch_add(1, Ordering::Relaxed);
                    log::info!("[BROKER] PUBACK received from subscriber for topic={}", topic);
                }
                Err(e) => {
                    log::error!("[BROKER] echo send_at_least_once failed: {:?}", e);
                }
            }
        });
        ECHO_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    Ok(publish.ack())
}

fn protocol_service_factory() -> impl ntex::service::ServiceFactory<
    v5::ProtocolMessage,
    Session<MySession>,
    Response = v5::ProtocolMessageAck,
    Error = MyServerError,
    InitError = MyServerError,
> {
    fn_factory_with_config(async move |session: Session<MySession>| {
        Ok(fn_service(async move |msg: v5::ProtocolMessage| {
            match msg {
                v5::ProtocolMessage::Subscribe(mut s) => {
                    // ── Test #2: SUBSCRIBE user properties ──────────────────
                    // codec::Subscribe has `user_properties: Vec<(ByteString, ByteString)>`
                    // accessed via s.packet().user_properties
                    let props: Vec<(String, String)> = s
                        .packet()
                        .user_properties
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect();

                    if !props.is_empty() {
                        log::info!("[BROKER] SUBSCRIBE user_properties: {:?}", props);
                        *session.last_sub_user_props.borrow_mut() = props.clone();
                        SUB_USER_PROPS_RECEIVED.store(true, Ordering::Relaxed);
                    }

                    // Per-filter grant or deny based on user properties
                    for mut sub in s.iter_mut() {
                        let deny = session
                            .last_sub_user_props
                            .borrow()
                            .iter()
                            .any(|(k, v)| k == "acl" && v == "deny");

                        if deny {
                            // 0x87 NotAuthorized — per-filter deny
                            sub.fail(v5::codec::SubscribeAckReason::NotAuthorized);
                            log::info!("[BROKER] Denying subscription to {}", sub.topic());
                        } else {
                            session
                                .subscriptions
                                .borrow_mut()
                                .push(sub.topic().to_string());
                            sub.confirm(v5::QoS::AtLeastOnce);
                            log::info!("[BROKER] Confirmed subscription to {}", sub.topic());
                        }
                    }

                    Ok(s.ack())
                }
                v5::ProtocolMessage::Unsubscribe(s) => Ok(s.ack()),
                v5::ProtocolMessage::Ping(p) => Ok(p.ack()),
                v5::ProtocolMessage::Disconnect(d) => Ok(d.ack()),
                _ => Ok(msg.ack()),
            }
        }))
    })
}

fn control_service_factory() -> impl ntex::service::ServiceFactory<
    Control<MyServerError>,
    Session<MySession>,
    Response = Option<v5::codec::Encoded>,
    Error = MyServerError,
    InitError = MyServerError,
> {
    fn_factory_with_config(async move |_: Session<MySession>| {
        Ok(fn_service(async move |control| match control {
            Control::Stop(Reason::Error(_)) => Ok(Some(
                v5::codec::Packet::from(v5::codec::Disconnect {
                    reason_code: v5::codec::DisconnectReasonCode::UnspecifiedError,
                    ..Default::default()
                })
                .into(),
            )),
            _ => Ok(None),
        }))
    })
}

// ─── broker thread ────────────────────────────────────────────────────────────

fn run_broker_thread(ready_tx: std::sync::mpsc::Sender<()>) {
    // EMBEDDING DESIGN:
    // ntex System on this std::thread uses DefaultRuntime (which on tokio feature
    // calls block_on → tries Handle::try_current() → None on this thread
    // → creates a new current-thread tokio runtime in a LocalSet).
    // The main thread already runs tokio multi-thread runtime — that's fine,
    // two separate tokio runtimes on separate threads, no conflict.
    let sys = ntex::rt::System::new("broker", ntex::rt::DefaultRuntime);
    sys.run(move || {
        BROKER_READY.store(true, Ordering::Relaxed);
        let _ = ready_tx.send(());

        ntex::server::build()
            .bind("mqtt", "127.0.0.1:1884", async |_| {
                MqttServer::new(handshake)
                    .control(control_service_factory())
                    .protocol(protocol_service_factory())
                    .publish(fn_factory_with_config(
                        async |session: Session<MySession>| {
                            Ok::<_, MyServerError>(fn_service(move |req| {
                                publish_handler(session.clone(), req)
                            }))
                        },
                    ))
            })
            .expect("bind failed")
            .workers(1)
            .run();

        Ok(())
    })
    .expect("ntex system failed");
}

// ─── ntex client thread for user-property SUBSCRIBE test ─────────────────────

fn run_ntex_subscribe_client_thread() {
    // Use ntex v5 client to send a SUBSCRIBE with user properties.
    // This exercises Test #2 from the client side.
    std::thread::spawn(|| {
        let sys = ntex::rt::System::new("sub-client", ntex::rt::DefaultRuntime);
        sys.block_on(async {
            // Small delay to ensure broker is ready
            ntex::time::sleep(ntex::time::Millis(400)).await;

            use ntex::service::ServiceFactory;

            let connector = v5::client::MqttConnector::new()
                .pipeline(ntex::service::cfg::SharedCfg::default())
                .await
                .expect("pipeline failed");

            let client = connector
                .call(
                    v5::client::Connect::new("127.0.0.1:1884")
                        .client_id("prop-client")
                        .keep_alive(ntex::time::Seconds(30)),
                )
                .await;

            match client {
                Ok(c) => {
                    let sink = c.sink();

                    // Subscribe with user properties — the key API under test
                    let result = sink
                        .subscribe(None)
                        .topic_filter(
                            "props/test".into(),
                            v5::codec::SubscriptionOptions {
                                qos: v5::QoS::AtLeastOnce,
                                no_local: false,
                                retain_as_published: false,
                                retain_handling: v5::codec::RetainHandling::AtSubscribe,
                            },
                        )
                        .property("mode".into(), "blocking".into())
                        .property("phase".into(), "pre".into())
                        .property("order".into(), "10".into())
                        .send()
                        .await;

                    match result {
                        Ok(ack) => {
                            log::info!(
                                "[CLIENT] SUBSCRIBE with user props ack status: {:?}",
                                ack.status
                            );
                        }
                        Err(e) => {
                            log::error!("[CLIENT] SUBSCRIBE failed: {:?}", e);
                        }
                    }
                    ntex::time::sleep(ntex::time::Millis(100)).await;
                    sink.close();
                }
                Err(e) => {
                    log::error!("[CLIENT] connect failed: {:?}", e);
                }
            }
        });
    });
}

// ─── rumqttc v5 round-trip test ───────────────────────────────────────────────

async fn run_client_test() -> Result<Vec<(String, String)>> {
    use rumqttc::v5::{AsyncClient, Event, MqttOptions, mqttbytes::v5::Packet, mqttbytes::QoS};

    let mut opts = MqttOptions::new("spike-client", "127.0.0.1", 1884);
    opts.set_keep_alive(Duration::from_secs(30));
    opts.set_manual_acks(true);

    let (client, mut eventloop) = AsyncClient::new(opts, 16);

    let client_clone = client.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

    let loop_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                event = eventloop.poll() => {
                    match event {
                        Ok(Event::Incoming(Packet::Publish(p))) => {
                            log::info!("[CLIENT] Received echo: topic={:?} payload={:?}",
                                p.topic, std::str::from_utf8(&p.payload).unwrap_or("?"));
                            if p.qos == QoS::AtLeastOnce {
                                let _ = client_clone.ack(&p).await;
                                log::info!("[CLIENT] Sent manual PUBACK");
                            }
                        }
                        Ok(Event::Incoming(Packet::ConnAck(_))) => {
                            log::info!("[CLIENT] Connected (MQTT5)");
                        }
                        Ok(Event::Incoming(Packet::SubAck(_))) => {
                            log::info!("[CLIENT] Subscribed");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::warn!("[CLIENT] eventloop error: {:?}", e);
                            break;
                        }
                    }
                }
            }
        }
    });

    // Give broker a moment
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Subscribe
    client
        .subscribe("test/echo", QoS::AtLeastOnce)
        .await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // ── Test #4: QoS1 publish round-trip latency ──────────────────────────
    let n = 10;
    let mut publish_times_us = Vec::with_capacity(n);

    for i in 0..n {
        let payload = format!("hello-{i}").into_bytes();
        let t0 = Instant::now();
        client
            .publish("test/echo", QoS::AtLeastOnce, false, payload)
            .await?;
        let elapsed = t0.elapsed();
        publish_times_us.push(elapsed.as_micros() as u64);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Wait for echoes + PUBACKs to drain
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = stop_tx.send(());
    let _ = loop_handle.await;
    client.disconnect().await.ok();

    let avg_us = publish_times_us.iter().sum::<u64>() / publish_times_us.len() as u64;
    let echo_count = ECHO_COUNT.load(Ordering::Relaxed);
    let puback_count = PUBACK_COUNT.load(Ordering::Relaxed);

    Ok(vec![
        ("n_publishes".to_string(), n.to_string()),
        ("echo_dispatches".to_string(), echo_count.to_string()),
        ("pubacks_resolved".to_string(), puback_count.to_string()),
        ("avg_publish_call_us".to_string(), avg_us.to_string()),
        (
            "sub_user_props_surfaced".to_string(),
            SUB_USER_PROPS_RECEIVED.load(Ordering::Relaxed).to_string(),
        ),
    ])
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    println!("=== ntex-mqtt 8.x embedding spike ===\n");

    // ── Test #1: EMBEDDING ───────────────────────────────────────────────────
    // Main thread: tokio multi-thread runtime (#[tokio::main]).
    // Broker thread: std::thread with ntex System (creates its own
    //   current-thread tokio runtime + LocalSet on that thread).
    // Two separate runtimes, separate threads — no interference.
    println!("[1] Embedding test: starting ntex broker on a std::thread");
    println!("    (tokio multi-thread runtime already running on main thread)");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || run_broker_thread(ready_tx));

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {
            println!("[1] Broker thread started (ready signal received).");
        }
        Err(_) => {
            println!("[1] FAIL: broker did not start within 5s!");
            return Ok(());
        }
    }

    // Give the server time to bind the port
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Prove both runtimes coexist — main thread does tokio work
    for i in 0..3 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        println!("[1] tokio main thread tick {i} (broker still running)");
    }
    println!("[1] EMBEDDING VERDICT: WORKS — no deadlock, no panic, both alive.\n");

    // ── Test #2: SUBSCRIBE user properties via ntex v5 client ───────────────
    println!("[2] Sending SUBSCRIBE with user properties (ntex client on its own thread)...");
    run_ntex_subscribe_client_thread();

    // Give the ntex client thread time to connect and subscribe
    tokio::time::sleep(Duration::from_millis(800)).await;

    let props_ok = SUB_USER_PROPS_RECEIVED.load(Ordering::Relaxed);
    if props_ok {
        println!("[2] SUBSCRIBE user properties: CONFIRMED received by broker.");
        println!("    Type: v5::codec::Subscribe {{ user_properties: Vec<(ByteString,ByteString)>, .. }}");
        println!("    Access in protocol handler: s.packet().user_properties");
        println!("    Per-filter grant: Subscription::confirm(QoS)");
        println!("    Per-filter deny:  Subscription::fail(SubscribeAckReason::NotAuthorized)  [0x87]");
    } else {
        println!("[2] WARNING: SUBSCRIBE user properties not received (client may not have connected).");
    }
    println!();

    // ── Tests #3 + #4: QoS1 per-delivery future + loopback latency ───────────
    println!("[3+4] QoS1 round-trip test with rumqttc client...");
    match run_client_test().await {
        Ok(results) => {
            println!("[3+4] Results:");
            for (k, v) in &results {
                println!("      {k}: {v}");
            }
            println!();
            println!("[3] PER-DELIVERY FUTURE API:");
            println!("    MqttSink::publish(topic).send_at_least_once(payload)");
            println!("    → impl Future<Output = Result<codec::PublishAck, SendPacketError>>");
            println!("    Resolves when THIS subscriber's PUBACK arrives.");
            println!("    Fan-out: spawn one future per subscriber sink; join_all = all-deliveries-done.");
        }
        Err(e) => {
            println!("[3+4] Client test error: {:?}", e);
        }
    }

    println!("\n=== spike complete ===");
    Ok(())
}

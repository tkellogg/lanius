//! `elanus bus pub|sub` — mosquitto_pub/sub for the elanus listener.
//!
//! Deliberately built on rumqttc (the client we recommend to skills,
//! docs/bus.md) rather than the kernel's hand-rolled mirror: every CLI use is
//! an interop test of the broker against a real-world client. Runs in main()
//! on its own current_thread runtime — never on the flight path.

use crate::bus;
use crate::paths::Root;
use crate::topic;
use anyhow::{bail, Context, Result};
use rumqttc::v5::mqttbytes::v5::Packet;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, Event, MqttOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

fn client(addr: SocketAddr, tag: &str) -> (AsyncClient, rumqttc::v5::EventLoop) {
    let mut opts = MqttOptions::new(
        format!("el-{tag}-{}", std::process::id()),
        addr.ip().to_string(),
        addr.port(),
    );
    opts.set_keep_alive(Duration::from_secs(10));
    // Inside a supervised package actor, identity rides the environment the
    // supervisor injected: the CLI authenticates as the actor and the
    // broker scopes it to the package's grants.
    if let (Ok(pkg), Ok(token)) = (std::env::var("ELANUS_PACKAGE"), std::env::var("ELANUS_BUS_TOKEN")) {
        opts.set_credentials(pkg, token);
    }
    AsyncClient::new(opts, 64)
}

fn addr(root: &Root) -> Result<SocketAddr> {
    let cfg = bus::config(root);
    if !cfg.enabled {
        bail!("bus is disabled in bus.toml");
    }
    bus::connect_addr(&cfg).with_context(|| format!("unparseable bind address {:?}", cfg.bind))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// Publish once. QoS 1 (default) returns only after the broker's PUBACK —
/// for in/# and signal/# topics that means "the ledger accepted it".
pub fn publish(
    root: &Root,
    topic_name: &str,
    payload: Option<&str>,
    qos: u8,
    retain: bool,
    correlation: Option<&str>,
) -> Result<()> {
    if !topic::valid_name(topic_name) {
        bail!("invalid topic name {topic_name:?} (wildcards are for filters)");
    }
    let qos = match qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        q => bail!("qos {q} unsupported (0 or 1)"),
    };
    let addr = addr(root)?;
    let correlation = correlation.map(str::to_owned);
    runtime()?.block_on(async move {
        let (client, mut eventloop) = client(addr, "pub");
        // Envelope correlation rides the el-correlation user property
        // (topics.md ID taxonomy: MQTT Correlation Data stays reserved for
        // the hook round trip).
        let props = correlation.map(|c| rumqttc::v5::mqttbytes::v5::PublishProperties {
            user_properties: vec![("el-correlation".to_string(), c)],
            ..Default::default()
        });
        client
            .publish_with_properties(
                topic_name,
                qos,
                retain,
                payload.unwrap_or("").as_bytes().to_vec(),
                props.unwrap_or_default(),
            )
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ev = tokio::time::timeout_at(deadline, eventloop.poll())
                .await
                .map_err(|_| anyhow::anyhow!("no broker response within 5s (daemon running?)"))?
                .context("connection failed (daemon running?)")?;
            match ev {
                Event::Incoming(Packet::PubAck(p)) => {
                    // The reason code is the broker's real answer. A failure
                    // (ACL deny, ledger emit failed, no db) must surface as a
                    // nonzero exit so the caller — a crash-only ingress
                    // bridge — does NOT delete its source line believing the
                    // ledger took it. This is what makes "PUBACK = accepted"
                    // true rather than aspirational.
                    use rumqttc::v5::mqttbytes::v5::PubAckReason;
                    if p.reason != PubAckReason::Success
                        && p.reason != PubAckReason::NoMatchingSubscribers
                    {
                        bail!("broker rejected publish to {topic_name:?}: {:?}", p.reason);
                    }
                    break;
                }
                Event::Outgoing(rumqttc::Outgoing::Publish(_)) if qos == QoS::AtMostOnce => break,
                _ => {}
            }
        }
        client.disconnect().await.ok();
        Ok(())
    })
}

/// Resident blocking-hook registration options for `bus sub --blocking`
/// (docs/bus.md hook plane). These become SUBSCRIBE user properties
/// (§3.8.2.1); the broker honors them only for a token-authed session whose
/// package holds the blocking grant — inside a supervised actor that's the
/// injected ELANUS_PACKAGE/ELANUS_BUS_TOKEN environment.
pub struct BlockingOpts {
    pub order: u32,
    pub timeout_ms: u64,
    pub on_timeout: String,
    /// Informational: phase/point ride along as user properties, but the
    /// subscription filter is authoritative for what the hook intercepts
    /// (obs/harness/hookreq/<point>/...).
    pub phase: Option<String>,
    pub point: Option<String>,
}

/// Subscribe and print one JSON line per message: {"topic":..., "payload":...}.
/// Exits after --count messages; --timeout without the count met is an error
/// (with no --count it's just the end of the observation window).
///
/// With --blocking this is a shell-scriptable resident hook: each incoming
/// hook request prints its payload (the request JSON) on stdout, then ONE
/// line is read from stdin — "allow", "deny", "deny:<reason>", or a JSON
/// object (the rewritten subject) — and the verdict is published to the
/// request's Response Topic with its Correlation Data echoed. stdin EOF
/// answers nothing: the broker's per-registration timeout applies, exactly
/// as if the hook client had died.
pub fn subscribe(
    root: &Root,
    filter: &str,
    count: Option<u64>,
    timeout_secs: Option<u64>,
    blocking: Option<BlockingOpts>,
) -> Result<()> {
    if !topic::valid_filter(filter) {
        bail!("invalid topic filter {filter:?}");
    }
    let addr = addr(root)?;
    runtime()?.block_on(async move {
        let (client, mut eventloop) = client(addr, "sub");
        match &blocking {
            Some(b) => {
                let mut user_properties = vec![
                    ("mode".to_string(), "blocking".to_string()),
                    ("order".to_string(), b.order.to_string()),
                    ("timeout_ms".to_string(), b.timeout_ms.to_string()),
                    ("on_timeout".to_string(), b.on_timeout.clone()),
                ];
                if let Some(p) = &b.phase {
                    user_properties.push(("phase".to_string(), p.clone()));
                }
                if let Some(p) = &b.point {
                    user_properties.push(("point".to_string(), p.clone()));
                }
                let props = rumqttc::v5::mqttbytes::v5::SubscribeProperties {
                    id: None,
                    user_properties,
                };
                client.subscribe_with_properties(filter, QoS::AtLeastOnce, props).await?;
            }
            None => client.subscribe(filter, QoS::AtLeastOnce).await?,
        }
        let deadline = timeout_secs.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
        let mut got = 0u64;
        // Verdict publishes in flight: queueing on the client is not sending,
        // so --count must not let us return before the broker's PUBACK — an
        // unflushed verdict would silently become the hook's on_timeout.
        let mut awaiting_ack = 0u32;
        loop {
            if count.is_some_and(|c| got >= c) && awaiting_ack == 0 {
                return Ok(());
            }
            // Once the count is met we only linger for acks; cap that linger
            // so a dead broker can't hold the process open.
            let flush_by = (count.is_some_and(|c| got >= c))
                .then(|| tokio::time::Instant::now() + Duration::from_secs(5));
            let polled = match (deadline, flush_by) {
                (Some(d), f) => {
                    let d = f.map_or(d, |f| d.min(f));
                    match tokio::time::timeout_at(d, eventloop.poll()).await {
                        Ok(ev) => ev,
                        Err(_) => {
                            if count.is_some_and(|c| got >= c) {
                                return Ok(()); // count met; ack linger expired
                            }
                            if let Some(want) = count {
                                bail!("timed out with {got} message(s), wanted {want}");
                            }
                            return Ok(());
                        }
                    }
                }
                (None, Some(f)) => match tokio::time::timeout_at(f, eventloop.poll()).await {
                    Ok(ev) => ev,
                    Err(_) => return Ok(()),
                },
                (None, None) => eventloop.poll().await,
            };
            let ev = polled.context("connection failed (daemon running?)")?;
            if let Event::Incoming(Packet::PubAck(_)) = ev {
                awaiting_ack = awaiting_ack.saturating_sub(1);
                continue;
            }
            if let Event::Incoming(Packet::Publish(p)) = ev {
                let topic_name = String::from_utf8_lossy(&p.topic).into_owned();
                let resp_to = p
                    .properties
                    .as_ref()
                    .and_then(|pr| pr.response_topic.clone())
                    .filter(|_| blocking.is_some());
                if let Some(resp_to) = resp_to {
                    // A hook request: print the event line, read the verdict.
                    let line = String::from_utf8_lossy(&p.payload).into_owned();
                    println!("{line}");
                    use std::io::Write as _;
                    std::io::stdout().flush().ok();
                    if let Some(verdict) = read_verdict() {
                        let corr =
                            p.properties.as_ref().and_then(|pr| pr.correlation_data.clone());
                        let props = rumqttc::v5::mqttbytes::v5::PublishProperties {
                            correlation_data: corr,
                            ..Default::default()
                        };
                        client
                            .publish_with_properties(
                                resp_to,
                                QoS::AtLeastOnce,
                                false,
                                verdict.to_string(),
                                props,
                            )
                            .await?;
                        awaiting_ack += 1;
                    } else {
                        eprintln!(
                            "[bus sub] no verdict on stdin; broker timeout/default applies"
                        );
                    }
                } else {
                    let payload: Value = serde_json::from_slice(&p.payload).unwrap_or_else(|_| {
                        Value::String(String::from_utf8_lossy(&p.payload).into_owned())
                    });
                    println!("{}", json!({ "topic": topic_name, "payload": payload }));
                    use std::io::Write as _;
                    std::io::stdout().flush().ok();
                }
                got += 1;
                // The count check lives at the top of the loop so a final
                // verdict's PUBACK is awaited before exit.
            }
        }
    })
}

/// One line of stdin → a verdict payload. None = no answer (EOF/IO error):
/// deliberately silent toward the broker so its declared default decides.
fn read_verdict() -> Option<Value> {
    use std::io::BufRead as _;
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line).ok()?;
    if n == 0 {
        return None;
    }
    let line = line.trim();
    if line == "allow" {
        return Some(json!({ "decision": "allow" }));
    }
    if line == "deny" {
        return Some(json!({ "decision": "deny" }));
    }
    if let Some(reason) = line.strip_prefix("deny:") {
        return Some(json!({ "decision": "deny", "reason": reason.trim() }));
    }
    match serde_json::from_str::<Value>(line) {
        Ok(v) if v.is_object() => Some(json!({ "decision": "allow", "event": v })),
        _ => {
            eprintln!("[bus sub] unintelligible verdict {line:?}; staying silent");
            None
        }
    }
}

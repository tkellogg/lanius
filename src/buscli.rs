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
pub fn publish(root: &Root, topic_name: &str, payload: Option<&str>, qos: u8, retain: bool) -> Result<()> {
    if !topic::valid_name(topic_name) {
        bail!("invalid topic name {topic_name:?} (wildcards are for filters)");
    }
    let qos = match qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        q => bail!("qos {q} unsupported (0 or 1)"),
    };
    let addr = addr(root)?;
    runtime()?.block_on(async move {
        let (client, mut eventloop) = client(addr, "pub");
        client
            .publish(topic_name, qos, retain, payload.unwrap_or("").as_bytes().to_vec())
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

/// Subscribe and print one JSON line per message: {"topic":..., "payload":...}.
/// Exits after --count messages; --timeout without the count met is an error
/// (with no --count it's just the end of the observation window).
pub fn subscribe(root: &Root, filter: &str, count: Option<u64>, timeout_secs: Option<u64>) -> Result<()> {
    if !topic::valid_filter(filter) {
        bail!("invalid topic filter {filter:?}");
    }
    let addr = addr(root)?;
    runtime()?.block_on(async move {
        let (client, mut eventloop) = client(addr, "sub");
        client.subscribe(filter, QoS::AtLeastOnce).await?;
        let deadline = timeout_secs.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
        let mut got = 0u64;
        loop {
            let polled = match deadline {
                Some(d) => match tokio::time::timeout_at(d, eventloop.poll()).await {
                    Ok(ev) => ev,
                    Err(_) => {
                        if let Some(want) = count {
                            bail!("timed out with {got} message(s), wanted {want}");
                        }
                        return Ok(());
                    }
                },
                None => eventloop.poll().await,
            };
            let ev = polled.context("connection failed (daemon running?)")?;
            if let Event::Incoming(Packet::Publish(p)) = ev {
                let topic_name = String::from_utf8_lossy(&p.topic).into_owned();
                let payload: Value = serde_json::from_slice(&p.payload)
                    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&p.payload).into_owned()));
                println!("{}", json!({ "topic": topic_name, "payload": payload }));
                use std::io::Write as _;
                std::io::stdout().flush().ok();
                got += 1;
                if count.is_some_and(|c| got >= c) {
                    return Ok(());
                }
            }
        }
    })
}

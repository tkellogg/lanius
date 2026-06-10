# ntex-mqtt 8.x Embedding Spike — Decision Report

**Date:** 2026-06-10
**Spike crate:** `spike/ntex/` (standalone, own Cargo.toml)
**Question:** ntex-mqtt 8.x for elanus embedded MQTT 5 broker, or fall back to rmqtt 0.21+?
**Fallback trigger (from bus.md):** broker logic > ~2k LOC, OR the ntex System embedding fights tokio.

---

## (a) Embedding Verdict: WORKS

**Status: No conflict. No deadlock. No panic.**

ntex's broker runs on a dedicated `std::thread`. The main process thread runs a tokio
multi-thread runtime (#[tokio::main]). These are two separate runtimes on separate threads.

**Mechanism (source-verified, ntex-net-3.12.0/src/tokio/mod.rs:29-43):**

When the broker's `std::thread` calls `block_on`, `Handle::try_current()` returns `Err`
(no tokio handle on that thread). ntex creates a fresh `current_thread` tokio runtime
wrapped in a `LocalSet`. The main thread's multi-thread runtime is completely unaffected.

**Pattern that works (verified live in spike):**
```rust
#[tokio::main]
async fn main() {
    std::thread::spawn(|| {
        let sys = ntex::rt::System::new("broker", ntex::rt::DefaultRuntime);
        sys.run(|| { ntex::server::build()...run(); Ok(()) })
    });
    // tokio work here runs concurrently — confirmed
}
```

Both runtimes observed alive simultaneously: broker accepted connections while tokio's
main thread looped. 10/10 clean runs.

---

## (b) Per-Requirement Findings

### Req 2: SUBSCRIBE User Properties

**CONFIRMED. Full API present.**

`v5::codec::Subscribe` has `user_properties: Vec<(ByteString, ByteString)>`.

In the protocol handler:
```rust
v5::ProtocolMessage::Subscribe(mut s) => {
    let props = &s.packet().user_properties;  // Vec<(ByteString, ByteString)>
    for mut sub in s.iter_mut() {
        sub.confirm(v5::QoS::AtLeastOnce);            // grant
        sub.fail(SubscribeAckReason::NotAuthorized);  // deny — 0x87
    }
    Ok(s.ack())
}
```

Spike confirmed: `mode=blocking, phase=pre, order=10` user properties sent by ntex v5 client
were received and logged by the broker handler. `SubscribeAckReason::NotAuthorized` (0x87)
is the per-filter deny code.

### Req 3: Per-Delivery Completion Future

**CONFIRMED. Exact API:**

```rust
MqttSink::publish(topic).send_at_least_once(payload)
// → impl Future<Output = Result<codec::PublishAck, SendPacketError>>
```

The future resolves when the subscriber's PUBACK arrives, not when the PUBLISH is written
to the socket. Fan-out pattern: one future per subscriber sink, collected with
`FuturesUnordered` or `join_all`. "All deliveries complete" is a control-flow fact.

Spike confirmed: 10/10 publishes → 10 echo dispatches → 10 PUBACK futures resolved.

### Req 4: Round-Trip Demo

Clean. `rumqttc::v5` client (MQTT 5.0, manual acks, QoS 1):
connect → subscribe `test/echo` → publish 10 × QoS1 → broker echoes each back →
client PUBACK → broker `send_at_least_once` future resolves.

---

## (c) LOC Estimate

| Component | Estimate |
|-----------|----------|
| Subscription table (filter → Vec<(ClientId, MqttSink)>) | ~200 LOC |
| Retained message store | ~100 LOC |
| Session state + expiry timers | ~200 LOC |
| Shared subscription groups ($share) | ~150 LOC |
| Will delay timers | ~100 LOC |
| QoS1 fan-out dispatch | ~100 LOC |
| ACL (user props → grant table) | ~150 LOC |
| **Total** | **~1000 LOC** |

ntex-mqtt provides: full MQTT5 codec, in-flight window management, topic filter parsing
and matching (`TopicFilter`, `matches_topic`), per-connection sinks, keep-alive, TCP accept.

**1000 LOC is under the 2k LOC fallback trigger.**

---

## (d) Latency

| Mode | avg `client.publish()` latency |
|------|-------------------------------|
| debug build | ~63 µs |
| release build | ~11 µs |

Full echo round-trip (cross-thread) observed < 10ms on loopback.
Well within 500ms hook timeout.

---

## (e) Recommendation: USE NTEX

**Both fallback triggers are clear — neither is triggered:**

1. Broker logic ~1k LOC < 2k LOC budget.
2. Embedding works cleanly, no runtime conflict.

**Why not rmqtt:** bus.md notes rmqtt drops SUBSCRIBE user properties before hooks
(requires a localized patch); ntex-mqtt surfaces them natively. ntex-mqtt is a
smaller surface to own. rmqtt brings a complete broker framework — more to navigate,
harder to maintain the bus invariant that recorder/hooks are in-process, not clients.

**Residual risks:**
- Single-maintainer (fafhrd91) framework risk — keep broker logic thin for migration portability.
- `!Send` futures internally (Rc<MqttShared>) — broker logic stays on the ntex thread;
  cross-thread via channels (already planned in bus.md).
- The "two runtimes on two threads" pattern is a documented contract; avoid starting the
  ntex System from within a tokio task.

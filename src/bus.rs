//! The bus, kernel side: how every process gets its happenings onto the live
//! topic stream (docs/bus.md). The daemon owns the broker (src/broker.rs);
//! everything here is the publish path and the boundary config.
//!
//! Three shapes of process, one publish() call:
//! - the daemon publishes in-process over a channel to the broker thread —
//!   no framing, no loopback hop;
//! - every other process (exec, emit, handlers via `elanus trace`) mirrors
//!   over loopback MQTT with a deliberately tiny hand-rolled QoS 0 publisher.
//!   No client library, no async runtime: trace::write is called from inside
//!   genai's tokio context in exec, and the flight path must never grow a
//!   nested-runtime panic. Mirrored publishes carry an `el-mirror` user
//!   property so the broker forwards them verbatim instead of re-recording;
//! - bus disabled or unreachable: publish() is a no-op. The black box doesn't
//!   depend on the radio — every error here is swallowed by design.

use crate::paths::Root;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// User-property key marking a publish as already recorded by its origin
/// process; the broker fans it out without writing it to disk again.
pub const MIRROR_PROP: &str = "el-mirror";

const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
/// After a failed connect or write, stay quiet this long before retrying —
/// an obs/fs/ flood against a dead daemon must not pay a connect per event.
const RETRY_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BusConfig {
    pub enabled: bool,
    /// Listener bind address. Loopback by default; binding wider is possible
    /// but discouraged until grants land (no authentication yet).
    pub bind: String,
}

impl Default for BusConfig {
    fn default() -> Self {
        BusConfig { enabled: true, bind: "127.0.0.1:1883".into() }
    }
}

pub fn config(root: &Root) -> BusConfig {
    let path = root.bus_file();
    let Ok(s) = std::fs::read_to_string(&path) else {
        return BusConfig::default();
    };
    match toml::from_str(&s) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[bus] {} parse error, using defaults: {e}", path.display());
            BusConfig::default()
        }
    }
}

/// Where local processes connect: the configured port on loopback when the
/// listener covers it (loopback or unspecified bind), else the bound address
/// itself (a machine address still reachable locally).
pub fn connect_addr(cfg: &BusConfig) -> Option<SocketAddr> {
    let bound: SocketAddr = cfg.bind.parse().ok()?;
    let ip = if bound.ip().is_unspecified() || bound.ip().is_loopback() {
        IpAddr::from([127, 0, 0, 1])
    } else {
        bound.ip()
    };
    Some(SocketAddr::new(ip, bound.port()))
}

/// One happening headed for the live stream: the topic plus the already
/// rendered envelope line (same JSON the recorder writes).
pub struct KernelPub {
    pub topic: String,
    pub line: String,
    /// Retained: late subscribers get the last value (liveness topics).
    pub retain: bool,
}

/// Kernel → broker-thread messages. Publishes share the channel with the
/// few control messages the supervisor sends (actor identity registration).
pub enum BusMsg {
    Publish(KernelPub),
    /// The supervisor minted a connection token for a package actor it is
    /// about to spawn; the broker uses it to authenticate CONNECT and to
    /// scope that session's ACL to the package's approved capabilities.
    RegisterActor { name: String, token: String },
    /// Actor exited; its token dies with it.
    UnregisterActor { name: String },
}

enum Handle {
    Local(tokio::sync::mpsc::UnboundedSender<BusMsg>),
    Mirror(Mutex<Mirror>),
    Off,
}

static HANDLE: OnceLock<Handle> = OnceLock::new();

/// Daemon-only: start the broker and claim the in-process publish path.
/// Must run before the daemon's first trace::write, or publish() will have
/// already fallen back to mirroring at a listener that doesn't exist yet.
/// Failure to bind degrades to Off with a warning — the daemon never dies
/// for the radio.
pub fn init_daemon(root: &Root) {
    let cfg = config(root);
    if !cfg.enabled {
        let _ = HANDLE.set(Handle::Off);
        return;
    }
    if let Ok(bound) = cfg.bind.parse::<SocketAddr>() {
        if !bound.ip().is_loopback() {
            eprintln!(
                "[bus] WARNING: listener bound to {} — beyond loopback with NO authentication; \
                 anyone who can reach the port can read everything and publish work. \
                 Discouraged until capability grants land.",
                cfg.bind
            );
        }
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    match crate::broker::spawn(root.clone(), cfg.clone(), rx) {
        Ok(()) => {
            eprintln!("[bus] mqtt listener on {}", cfg.bind);
            let _ = HANDLE.set(Handle::Local(tx));
        }
        Err(e) => {
            eprintln!("[bus] listener disabled: {e:#}");
            let _ = HANDLE.set(Handle::Off);
        }
    }
}

/// Best-effort live publish, with the retain flag (kernel liveness topics —
/// late subscribers see the last value). Never blocks meaningfully, never
/// errors: the recorder (disk) has already made its own decision
/// independently of this. The mirror
/// path is QoS 0 non-retained only; retained kernel publishes exist only in
/// the daemon, which owns the broker — enforced here by simply dropping the
/// flag on the mirror path.
pub fn publish_with(root: &Root, topic: &str, line: &str, retain: bool) {
    let handle = HANDLE.get_or_init(|| {
        let cfg = config(root);
        if !cfg.enabled {
            return Handle::Off;
        }
        match connect_addr(&cfg) {
            Some(addr) => Handle::Mirror(Mutex::new(Mirror::new(addr))),
            None => Handle::Off,
        }
    });
    match handle {
        Handle::Local(tx) => {
            let _ = tx.send(BusMsg::Publish(KernelPub {
                topic: topic.to_string(),
                line: line.to_string(),
                retain,
            }));
        }
        Handle::Mirror(m) => {
            if let Ok(mut mirror) = m.lock() {
                mirror.publish(topic, line.as_bytes());
            }
        }
        Handle::Off => {}
    }
}

/// Supervisor-only: tell the broker about an actor's connection token.
/// No-op outside the daemon.
pub fn register_actor(name: &str, token: Option<&str>) {
    if let Some(Handle::Local(tx)) = HANDLE.get() {
        let msg = match token {
            Some(t) => BusMsg::RegisterActor { name: name.to_string(), token: t.to_string() },
            None => BusMsg::UnregisterActor { name: name.to_string() },
        };
        let _ = tx.send(msg);
    }
}

/// Minimal MQTT 5 QoS 0 publisher over a plain TcpStream. Encodes exactly
/// three packets (CONNECT, PUBLISH, and reads one CONNACK); anything fancier
/// belongs in a real client library — but real client libraries bring their
/// own runtime, and this runs on the flight path.
struct Mirror {
    addr: SocketAddr,
    conn: Option<TcpStream>,
    retry_after: Option<Instant>,
}

impl Mirror {
    fn new(addr: SocketAddr) -> Mirror {
        Mirror { addr, conn: None, retry_after: None }
    }

    fn publish(&mut self, topic: &str, payload: &[u8]) {
        if self.conn.is_none() {
            if self.retry_after.is_some_and(|t| Instant::now() < t) {
                return;
            }
            match connect(self.addr) {
                Ok(s) => {
                    self.conn = Some(s);
                    self.retry_after = None;
                }
                Err(_) => {
                    self.retry_after = Some(Instant::now() + RETRY_AFTER);
                    return;
                }
            }
        }
        let frame = encode_publish(topic, payload);
        if self.conn.as_mut().unwrap().write_all(&frame).is_err() {
            self.conn = None;
            self.retry_after = Some(Instant::now() + RETRY_AFTER);
        }
    }
}

fn connect(addr: SocketAddr) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(IO_TIMEOUT))?;
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    s.write_all(&encode_connect(&format!("el-mirror-{}", std::process::id())))?;
    read_connack(&mut s)?;
    Ok(s)
}

// ── MQTT 5 wire encoding (the three packets we speak) ──────────────────────

fn varint(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn utf8(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn frame(byte0: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![byte0];
    varint(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn encode_connect(client_id: &str) -> Vec<u8> {
    let mut body = Vec::new();
    utf8("MQTT", &mut body);
    body.push(0x05); // protocol version 5
    body.push(0x02); // clean start, no will, no auth
    body.extend_from_slice(&[0, 0]); // keep alive disabled
    body.push(0x00); // empty properties
    utf8(client_id, &mut body);
    frame(0x10, &body)
}

fn encode_publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut props = Vec::new();
    props.push(0x26); // user property
    utf8(MIRROR_PROP, &mut props);
    utf8("1", &mut props);
    let mut body = Vec::new();
    utf8(topic, &mut body);
    varint(props.len(), &mut body);
    body.extend_from_slice(&props);
    body.extend_from_slice(payload);
    frame(0x30, &body) // QoS 0, no dup, no retain — no packet id
}

fn read_connack(s: &mut TcpStream) -> std::io::Result<()> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut b0 = [0u8; 1];
    s.read_exact(&mut b0)?;
    if b0[0] != 0x20 {
        return Err(bad("expected CONNACK"));
    }
    let mut len: usize = 0;
    let mut shift = 0u32;
    loop {
        let mut b = [0u8; 1];
        s.read_exact(&mut b)?;
        len |= ((b[0] & 0x7f) as usize) << shift;
        if b[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return Err(bad("varint overflow"));
        }
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    if body.len() < 2 || body[1] != 0x00 {
        return Err(bad("CONNACK refused"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint_of(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        varint(n, &mut v);
        v
    }

    #[test]
    fn varint_spec_examples() {
        // MQTT 5 §1.5.5 boundary values
        assert_eq!(varint_of(0), vec![0x00]);
        assert_eq!(varint_of(127), vec![0x7f]);
        assert_eq!(varint_of(128), vec![0x80, 0x01]);
        assert_eq!(varint_of(16_383), vec![0xff, 0x7f]);
        assert_eq!(varint_of(16_384), vec![0x80, 0x80, 0x01]);
    }

    #[test]
    fn publish_frame_shape() {
        let f = encode_publish("a/b", b"x");
        assert_eq!(f[0], 0x30); // PUBLISH, QoS 0
        // remaining length = topic(2+3) + props(1 varint + 1+2+9+2+1) + payload(1)
        let props_len = 1 + 2 + MIRROR_PROP.len() + 2 + 1;
        assert_eq!(f[1] as usize, 5 + 1 + props_len + 1);
        assert_eq!(&f[2..4], &[0x00, 0x03]); // topic length
        assert_eq!(&f[4..7], b"a/b");
        assert_eq!(f[7] as usize, props_len); // properties length
        assert_eq!(f[8], 0x26); // user property id
        assert_eq!(*f.last().unwrap(), b'x');
    }

    #[test]
    fn connect_frame_shape() {
        let f = encode_connect("me");
        assert_eq!(f[0], 0x10);
        assert_eq!(&f[2..8], &[0x00, 0x04, b'M', b'Q', b'T', b'T']);
        assert_eq!(f[8], 0x05); // version
        assert_eq!(f[9], 0x02); // clean start
    }

    #[test]
    fn connect_addr_resolution() {
        let cfg = |bind: &str| BusConfig { enabled: true, bind: bind.into() };
        assert_eq!(
            connect_addr(&cfg("127.0.0.1:1883")).unwrap().to_string(),
            "127.0.0.1:1883"
        );
        assert_eq!(
            connect_addr(&cfg("0.0.0.0:1900")).unwrap().to_string(),
            "127.0.0.1:1900"
        );
        assert_eq!(
            connect_addr(&cfg("192.168.1.5:1883")).unwrap().to_string(),
            "192.168.1.5:1883"
        );
        assert!(connect_addr(&cfg("not-an-addr")).is_none());
    }

    #[test]
    fn config_defaults_are_loopback() {
        let c = BusConfig::default();
        assert!(c.enabled);
        assert!(c.bind.starts_with("127.0.0.1:"));
    }
}

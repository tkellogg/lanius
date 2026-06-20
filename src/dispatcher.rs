use crate::config_repo;
use crate::db;
use crate::envcompat::EnvDual;
use crate::events::{self, EmitOpts};
use crate::hooks;
use crate::packages;
use crate::paths::Root;
use crate::profile;
use crate::sandbox;
use crate::trace;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr as _;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// One spawned handler process being supervised.
struct Running {
    child: Child,
    dispatch_id: i64,
    event_id: i64,
    etype: String,
    correlation: Option<String>,
    out_path: PathBuf,
    err_path: PathBuf,
}

/// One resident package actor (process.mode = "daemon"), crash-only:
/// discovery boots it zero-caged, exits restart it with backoff, the
/// supervisor publishes its retained liveness either way. (A self-connecting
/// actor's own LWT also covers the crash case at the bus level.)
struct Actor {
    name: String,
    child: Child,
    started: Instant,
    /// Fingerprint of the package's config file at spawn (docs/config.md D3).
    /// When the live config changes under a running daemon, this stops matching
    /// and the supervisor restarts the actor so it re-reads.
    config_fp: String,
}

#[derive(Default)]
struct Actors {
    running: Vec<Actor>,
    /// Consecutive failures per package; cleared by a healthy run.
    strikes: HashMap<String, u32>,
    backoff_until: HashMap<String, Instant>,
}

/// A run shorter than this counts as a failure for backoff purposes.
const HEALTHY_RUN: Duration = Duration::from_secs(10);
const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_CAP: Duration = Duration::from_secs(300);

// ── Coding-session inbound delivery (M2-B) ───────────────────────────────────
//
// A message addressed to an idle coding session's mailbox (`in/agent/<tool>/<conv>`,
// `<conv>` = a recorded `code-*` session) makes the daemon resume that session with
// the message. The daemon is the kernel — it sees the materialized `in/` delivery
// directly and already holds the authority the emit-only session lacks, so it
// drives `codeagent::resume_capture` itself (which mints the session's own
// emit-only token; the session gains NO read authority).
//
// Concurrency follows the dispatcher's "don't block the tick" discipline without
// the fork/exec process model (resume runs in-process to mint/retire the scoped
// token and parse the JSONL stream). Instead each session gets a dedicated WORKER
// THREAD that owns a FIFO queue: a given session runs exactly one resume at a time
// (the native tool isn't concurrent-safe), two rapid deliveries to the same session
// SERIALIZE behind that single thread, and a slow resume on one session never
// stalls the tick loop or another session. Durability rides the ledger: a claimed
// delivery is marked `running` before hand-off and settled `done`/`failed` only
// when its worker reports back, so a daemon restart mid-resume re-pends the event
// (boot's `state='running' -> 'pending'` sweep) and replays it — at-least-once,
// never a lost or silently double-run message (the in-flight guard below stops a
// same-tick double-run; the ledger state stops a cross-tick one).

/// One queued delivery handed to a session's worker thread.
struct CodeJob {
    event_id: i64,
    correlation: Option<String>,
    message: String,
}

/// A worker thread's outcome for one delivery, reported back to the tick loop so
/// it can settle the event state on the main connection (workers never touch the
/// dispatcher's connection; they open their own for the resume).
struct CodeDone {
    session: String,
    event_id: i64,
    correlation: Option<String>,
    /// None = the resume primitive errored (missing record / spawn / credential);
    /// Some(success) = the tool ran and exited (success=false on a non-zero/timeout).
    success: Option<bool>,
    detail: String,
}

/// A live per-session worker: its job queue sender plus how many deliveries are
/// outstanding (queued or running) on it, so we never enqueue the same event
/// twice and can retire an idle worker.
struct CodeWorker {
    tx: Sender<CodeJob>,
    inflight: usize,
}

/// Per-session worker threads + the shared completion channel. Held in the tick
/// loop like `Actors`; crash-only (a dead worker thread just stops draining — its
/// claimed event stays `running` and replays on the next daemon start).
struct CodeDrivers {
    workers: HashMap<String, CodeWorker>,
    done_tx: Sender<CodeDone>,
    done_rx: Receiver<CodeDone>,
    /// Event ids handed to a worker this process lifetime — the same-tick /
    /// same-process double-claim guard (the ledger `running` state guards across
    /// restarts; this guards within one run, where the row is briefly still
    /// visible as the worker drains it).
    claimed: std::collections::HashSet<i64>,
}

impl Default for CodeDrivers {
    fn default() -> Self {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        CodeDrivers {
            workers: HashMap::new(),
            done_tx,
            done_rx,
            claimed: std::collections::HashSet::new(),
        }
    }
}

/// The dispatcher does *nothing* but: notice pending events, match type to
/// handlers, check throttles, fork/exec, record exits, write trace lines.
/// It is a supervisor, not a doer.
pub fn run(root: &Root, interval_ms: u64) -> Result<()> {
    // Before the first trace::write, or the publish path falls back to
    // mirroring at a listener that doesn't exist yet.
    crate::bus::init_daemon(root);
    // Ensure the config repo exists (docs/config.md): idempotent, so a root
    // created before the config model gains it on the next daemon start without
    // a re-init. Best-effort — a git-less host degrades (config writes will
    // fail) rather than taking the daemon down with it.
    if let Err(e) = config_repo::init(root) {
        eprintln!("[daemon] config repo unavailable (config writes disabled): {e:#}");
    }
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    merge_profile_throttles(root, &conn);
    std::fs::create_dir_all(root.run_dir())?;
    // Orphaned 'running' rows from a previous daemon are unrecoverable: the
    // children died with that process. They get a distinct state — NOT
    // 'failed' — so a successful replay isn't poisoned by stale rows when
    // recompute_event_state aggregates, and NOT counted as failures by
    // monitors. Replay is the cure.
    conn.execute(
        "UPDATE dispatches SET state='orphaned', exit_code=-3, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE state='running'",
        [],
    )?;
    // Events with a surviving suspended dispatch are parked, not replayable:
    // re-pending them would re-run the suspender from scratch (duplicate ask)
    // while the old dispatch stays armed for resume — double execution.
    conn.execute(
        "UPDATE events SET state='waiting_on_human' WHERE state='running'
         AND EXISTS (SELECT 1 FROM dispatches d WHERE d.event_id = events.id AND d.state='suspended')",
        [],
    )?;
    conn.execute(
        "UPDATE events SET state='pending' WHERE state='running'",
        [],
    )?;
    // Stale leases from dead holders: release anything whose dispatch is no
    // longer running and whose pid is gone. Crash-only, same as everything.
    release_dead_leases(&conn)?;
    // Orphaned coding-session credentials: a launcher SIGKILL'd mid-session
    // leaks its scoped token (the best-effort retire never ran). Reap any whose
    // owning launcher pid is gone so a dead session's credential stops
    // authenticating (docs/security.md). Crash-only, like the lease reaper.
    for orphan in crate::codesession::reap_orphans(root) {
        eprintln!("[daemon] reaped orphaned coding-session credential {orphan}");
    }
    // Register what's on the package path; requests only, never grants.
    if let Err(e) = packages::sync(root, &conn) {
        eprintln!("[daemon] package sync: {e:#}");
    }
    eprintln!(
        "[daemon] root={} interval={}ms (let-it-crash; ctrl-c to stop)",
        root.dir.display(),
        interval_ms
    );
    let mut running: Vec<Running> = Vec::new();
    let mut actors = Actors::default();
    let mut code = CodeDrivers::default();
    loop {
        if let Err(e) = tick(root, &conn, &mut running, &mut actors, &mut code) {
            eprintln!("[daemon] tick error: {e:#}");
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

fn tick(
    root: &Root,
    conn: &Connection,
    running: &mut Vec<Running>,
    actors: &mut Actors,
    code: &mut CodeDrivers,
) -> Result<()> {
    // Linked packages can change on disk under a running daemon; drift
    // detection re-enters review within a tick (reads only when steady).
    if let Err(e) = packages::sync_if_drifted(root, conn) {
        eprintln!("[daemon] drift sync: {e:#}");
    }
    tick_crons(root, conn)?;
    expire_deadlines(root, conn)?;
    announce_ledger_events(root, conn)?;
    reap(root, conn, running)?;
    settle_code_deliveries(root, conn, code)?;
    drive_code_deliveries(root, conn, code)?;
    resume_suspended(root, conn, running)?;
    dispatch_pending(root, conn, running)?;
    tick_actors(root, conn, actors)?;
    release_dead_leases(conn)?;
    Ok(())
}

/// Announce kernel-minted ledger events on the bus under their own topic —
/// the work-plane-on-bus delivery piece (docs/bus.md "[KNOWN GAP — as built,
/// step 5/6]"). The daemon is the single announcement authority for every
/// emit that did NOT arrive over the bus: CLI `elanus emit`, cron, the
/// dispatcher's own emits, exec handlers' emits — and events emitted while
/// the daemon was down, which this sweep picks up on the next start.
///
/// Exactly-once is a row-level fact: events::emit inserts announced=0; the
/// broker's inbound path inserts announced=1 because it fans the
/// materialized event out itself at inbound time (a bus-origin event must
/// never be announced twice). The kernel's announcements deliberately do NOT
/// ride the el-mirror loopback path — the broker re-materializes in/# and
/// signal/# mirrors into the ledger by design (the fc4fab1 security fix: the
/// mirror marker is never a license to inject un-ledgered work), so the
/// in-process channel (bus::publish_with in the daemon) is the only correct
/// route. publish_with is fan-out only in pump(); it cannot re-enter emit().
///
/// Best-effort by design: listener down → daemon actors miss the live
/// announcement, but the ledger row, exec dispatch, and recording are
/// untouched (degradation order). The row is marked announced either way —
/// the live stream is at-most-once; the ledger is the durable copy.
fn announce_ledger_events(root: &Root, conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, Option<i64>, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, type, cause_id, correlation_id, payload FROM events
             WHERE announced = 0 ORDER BY id LIMIT 500",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, etype, cause, corr, payload) in rows {
        // in/# and signal/# ride the bus under their own topic, as does
        // obs/config/# — a config acceptance is a live notification a dashboard
        // wants (docs/config.md D3), and it is kernel-emitted so it would
        // otherwise never reach a subscriber. Other obs/ types (e.g. obs/channel
        // receipts via `elanus emit`) keep their obs/harness/ledger/emit echo
        // only. A bus-origin event is already announced=1, so it never reaches
        // this sweep — no double-publish. Mark the row either way so we move on.
        if etype.starts_with("in/")
            || etype.starts_with("signal/")
            || etype.starts_with("obs/config/")
        {
            let pv: Value = payload
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.into())))
                .unwrap_or(Value::Null);
            // Same line shape the broker fans out for bus-origin events, so
            // a subscriber sees one format regardless of origin.
            let mut line = json!({
                "ts": trace::now_iso(), "kind": etype, "payload": pv, "event_id": id
            });
            if let Some(c) = cause {
                line["cause_id"] = json!(c);
            }
            if let Some(c) = &corr {
                line["correlation_id"] = json!(c);
            }
            crate::bus::publish_with(root, &etype, &line.to_string(), false);
        }
        conn.execute("UPDATE events SET announced = 1 WHERE id = ?1", [id])?;
    }
    Ok(())
}

/// Supervise resident package actors. Discovery boots them (zero cage:
/// scratch dir + approved fs_write; capabilities attach live via the
/// ledger); exits restart with exponential backoff unless restart="never".
/// The supervisor publishes retained obs/package/<name>/status — it is the
/// one process that authoritatively knows spawn and exit.
fn tick_actors(root: &Root, conn: &Connection, actors: &mut Actors) -> Result<()> {
    // Reap exits.
    let mut i = 0;
    while i < actors.running.len() {
        match actors.running[i].child.try_wait() {
            Ok(Some(status)) => {
                let a = actors.running.swap_remove(i);
                let healthy = a.started.elapsed() >= HEALTHY_RUN;
                let strikes = if healthy {
                    0
                } else {
                    *actors.strikes.get(&a.name).unwrap_or(&0) + 1
                };
                actors.strikes.insert(a.name.clone(), strikes);
                let delay = BACKOFF_BASE
                    .saturating_mul(2u32.saturating_pow(strikes.min(8)))
                    .min(BACKOFF_CAP);
                actors
                    .backoff_until
                    .insert(a.name.clone(), Instant::now() + delay);
                crate::bus::register_actor(&a.name, None);
                status_event(root, &a.name, "dead", json!({ "exit_code": status.code() }));
            }
            Ok(None) => i += 1,
            Err(e) => {
                eprintln!("[daemon] actor wait error: {e}");
                i += 1;
            }
        }
    }
    // Config reload (docs/config.md D3): a running daemon whose package config
    // changed is restarted so it re-reads. Kill it here; the boot loop below
    // respawns it this same tick with the fresh fingerprint. This is an
    // intentional restart, so it must NOT count as a crash — clear any strike
    // and backoff so the reboot is immediate.
    let mut i = 0;
    while i < actors.running.len() {
        let name = actors.running[i].name.clone();
        if config_repo::fingerprint(root, &name) != actors.running[i].config_fp {
            let mut a = actors.running.swap_remove(i);
            // Kill the whole process group, not just the direct child: a shell
            // daemon's descendants and the sandbox-exec wrapper would otherwise
            // outlive the "reload" (the actor was spawned with process_group(0),
            // so its pgid == its pid).
            unsafe { libc::killpg(a.child.id() as i32, libc::SIGKILL) };
            let _ = a.child.wait();
            actors.strikes.remove(&name);
            actors.backoff_until.remove(&name);
            crate::bus::register_actor(&name, None);
            status_event(
                root,
                &name,
                "reloading",
                json!({ "reason": "config changed" }),
            );
        } else {
            i += 1;
        }
    }
    // Boot what's discovered and not running.
    for pkg in packages::discover(root)? {
        let Some(lm) = &pkg.manifest else { continue };
        let Some(proc_) = &lm.manifest.process else {
            continue;
        };
        if proc_.mode != "daemon" {
            continue;
        }
        if actors.running.iter().any(|a| a.name == pkg.name) {
            continue;
        }
        if proc_.restart == "never" && actors.strikes.contains_key(&pkg.name) {
            continue; // ran once, died, stays down
        }
        if actors
            .backoff_until
            .get(&pkg.name)
            .is_some_and(|t| Instant::now() < *t)
        {
            continue;
        }
        let script = pkg.dir.join(&proc_.run);
        if !script.exists() {
            continue;
        }
        let scratch = root.run_dir().join(format!("pkg-{}", pkg.name));
        std::fs::create_dir_all(&scratch)?;
        // Zero-cage floor: write scratch (+ approved durable fs_write).
        // Daemon actors talk to the kernel over the bus, not the db — the
        // ledger is deliberately outside their cage.
        let mut write_roots = vec![scratch.clone()];
        for w in packages::approved(conn, &pkg.name, "fs_write")? {
            let p = PathBuf::from(&w);
            let p = if p.is_absolute() { p } else { root.dir.join(p) };
            if let Ok(c) = p.canonicalize() {
                write_roots.push(c);
            }
        }
        let cage = sandbox::Cage::from_roots(
            write_roots,
            Vec::new(),
            true,
            &sandbox::Protect::for_root(root),
        );
        let token = uuid::Uuid::new_v4().to_string();
        crate::bus::register_actor(&pkg.name, Some(&token));
        let bus_cfg = crate::bus::config(root);
        let addr = crate::bus::connect_addr(&bus_cfg)
            .map(|a| a.to_string())
            .unwrap_or_default();
        // Harness-negotiated HTTP port (manifest.rs ProcessDecl.http): bind
        // 127.0.0.1:0 to pick a free port, record it in the run dir — the
        // discovery channel is harness state, never a retained bus message
        // (docs/security.md entry 11). The bind is dropped before spawn (the
        // package binds it itself); the tiny race is the standard one.
        let http_port: Option<u16> = if proc_.http {
            match std::net::TcpListener::bind("127.0.0.1:0") {
                Ok(l) => l.local_addr().ok().map(|a| a.port()),
                Err(e) => {
                    eprintln!("[daemon] {}: http port alloc failed: {e}", pkg.name);
                    None
                }
            }
        } else {
            None
        };
        if let Some(p) = http_port {
            std::fs::write(
                scratch.join("http.json"),
                json!({ "port": p, "package": pkg.name, "bind": "127.0.0.1" }).to_string(),
            )?;
        }
        let out = std::fs::File::create(scratch.join("stdout.log"))?;
        let err = std::fs::File::create(scratch.join("stderr.log"))?;
        let mut cmd = cage.command(&script);
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default();
        // Own process group so a reload/stop can kill the whole tree, not just
        // the direct child (the sandbox-exec wrapper or a shell daemon's
        // descendants would otherwise survive) — same discipline as exec.rs.
        cmd.process_group(0)
            .current_dir(&pkg.dir)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .env_dual("ROOT", &root.dir)
            // Read-actors (e.g. history) open the ledger directly; hand them the
            // path rather than make them guess <root>/<dbname>. The cage allows
            // db reads (write-fenced only). DB env was always "supervisor-given"
            // by contract — only the fallback ever filled it, which broke when
            // the file was renamed; now it is set for real.
            .env_dual("DB", root.db())
            .env("ELANUS_PACKAGE", &pkg.name)
            .env("ELANUS_SCRATCH", &scratch)
            .env("ELANUS_BUS_ADDR", &addr)
            .env("ELANUS_BUS_TOKEN", &token)
            .env(
                "ELANUS_SESSION_EXPIRY_S",
                proc_.session_expiry_s.to_string(),
            )
            .env(
                "ELANUS_HTTP_PORT",
                http_port.map(|p| p.to_string()).unwrap_or_default(),
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    exe_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        match cmd.spawn() {
            Ok(child) => {
                status_event(root, &pkg.name, "alive", json!({ "pid": child.id() }));
                actors.running.push(Actor {
                    config_fp: config_repo::fingerprint(root, &pkg.name),
                    name: pkg.name.clone(),
                    child,
                    started: Instant::now(),
                });
            }
            Err(e) => {
                crate::bus::register_actor(&pkg.name, None);
                actors.strikes.insert(
                    pkg.name.clone(),
                    actors.strikes.get(&pkg.name).unwrap_or(&0) + 1,
                );
                actors
                    .backoff_until
                    .insert(pkg.name.clone(), Instant::now() + BACKOFF_BASE);
                status_event(
                    root,
                    &pkg.name,
                    "dead",
                    json!({ "spawn_error": e.to_string() }),
                );
            }
        }
    }
    Ok(())
}

/// Retained liveness: late subscribers always see the last known state.
fn status_event(root: &Root, name: &str, state: &str, mut extra: Value) {
    let payload = match extra.as_object_mut() {
        Some(o) => {
            o.insert("state".into(), json!(state));
            extra
        }
        None => json!({ "state": state }),
    };
    trace::write_opts(
        root,
        &format!("obs/package/{}/status", crate::topic::encode_segment(name)),
        &trace::Ids::default(),
        payload,
        true,
    );
}

/// Leases die with their holders: a lease whose dispatch finished, or whose
/// pid is gone, is released by the supervisor. No lock leaks, no manual
/// unlock path to forget.
fn release_dead_leases(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE leases SET released_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE released_at IS NULL
           AND dispatch_id IS NOT NULL
           AND dispatch_id IN (SELECT id FROM dispatches WHERE state NOT IN ('running','suspended'))",
        [],
    )?;
    let stale: Vec<(i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, pid FROM leases WHERE released_at IS NULL AND dispatch_id IS NULL AND pid IS NOT NULL",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, pid) in stale {
        // Signal 0: existence probe, no effect on the process.
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !alive {
            conn.execute(
                "UPDATE leases SET released_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
                [id],
            )?;
        }
    }
    Ok(())
}

fn tick_crons(root: &Root, conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, String, Option<String>, Option<String>)> = {
        let mut stmt =
            conn.prepare("SELECT id, schedule, emit_type, payload, last_fired FROM crons")?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let now = Utc::now();
    for (id, schedule, emit_type, payload, last_fired) in rows {
        let Ok(cron) = croner::Cron::from_str(&schedule) else {
            continue;
        };
        // A cron emit is a publish; the capability check happens at fire
        // time so approvals attach and detach live.
        let pkg: String = conn
            .query_row("SELECT skill FROM crons WHERE id=?1", [id], |r| r.get(0))
            .unwrap_or_default();
        if !packages::may(conn, &pkg, "publish", &emit_type)? {
            continue;
        }
        match last_fired {
            None => {
                // Arm on first sight; don't fire for the past.
                conn.execute(
                    "UPDATE crons SET last_fired = ?1 WHERE id = ?2",
                    params![trace::now_iso(), id],
                )?;
            }
            Some(lf) => {
                let Ok(lf_dt) = DateTime::parse_from_rfc3339(&lf) else {
                    continue;
                };
                let lf_utc = lf_dt.with_timezone(&Utc);
                if let Ok(next) = cron.find_next_occurrence(&lf_utc, false) {
                    if next <= now {
                        events::emit(
                            root,
                            conn,
                            EmitOpts {
                                payload: payload
                                    .as_deref()
                                    .and_then(|s| serde_json::from_str(s).ok()),
                                // Dedupes the same scheduled firing across daemon restarts.
                                idempotency: Some(format!("cron:{}:{}", id, next.to_rfc3339())),
                                ..EmitOpts::new(&emit_type)
                            },
                        )?;
                        conn.execute(
                            "UPDATE crons SET last_fired = ?1 WHERE id = ?2",
                            params![trace::now_iso(), id],
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Defaults are the big unblock: an expired ask executes its default and logs
/// the assumption as an ordinary answer event (mail to the agent) —
/// auditable, vetoable.
fn expire_deadlines(root: &Root, conn: &Connection) -> Result<()> {
    let mb = profile::mailboxes(root);
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.correlation_id, e.default_action FROM events e
             WHERE e.type = ?1 AND e.deadline IS NOT NULL
               AND e.state != 'expired' AND e.correlation_id IS NOT NULL
               AND e.deadline < strftime('%Y-%m-%dT%H:%M:%fZ','now')
               AND NOT EXISTS (SELECT 1 FROM events a
                               WHERE a.type = ?2 AND a.correlation_id = e.correlation_id)",
        )?;
        let r = stmt
            .query_map([&mb.human, &mb.agent], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (ask_id, corr, default_action) in rows {
        let default: Value = default_action
            .as_deref()
            .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.to_string())))
            .unwrap_or(Value::Null);
        events::emit(
            root,
            conn,
            EmitOpts {
                payload: Some(json!({ "answer": default, "assumed": true })),
                correlation: Some(corr.clone()),
                cause: Some(ask_id),
                ..EmitOpts::new(&mb.agent)
            },
        )?;
        conn.execute(
            "UPDATE events SET state='expired', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            [ask_id],
        )?;
        trace::write(
            root,
            "obs/harness/ledger/expire",
            &trace::Ids {
                event_id: Some(ask_id),
                correlation_id: Some(corr),
                ..Default::default()
            },
            json!({ "assumed_default": default }),
        );
    }
    Ok(())
}

fn reap(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let mut i = 0;
    while i < running.len() {
        match running[i].child.try_wait() {
            Ok(Some(status)) => {
                let r = running.swap_remove(i);
                let code = status.code().unwrap_or(-1);
                finish_dispatch(root, conn, &r, code)?;
            }
            Ok(None) => i += 1,
            Err(e) => {
                eprintln!("[daemon] wait error: {e}");
                i += 1;
            }
        }
    }
    Ok(())
}

fn finish_dispatch(root: &Root, conn: &Connection, r: &Running, code: i32) -> Result<()> {
    let stdout = read_clipped(&r.out_path);
    let stderr = read_clipped(&r.err_path);
    let mut dstate = match code {
        0 => "done",
        75 => "suspended", // EX_TEMPFAIL: checkpointed itself; resume via correlation_id
        _ => "failed",
    };
    let mut resume_correlation: Option<String> = None;
    if dstate == "suspended" {
        // The suspend contract: before exiting 75 the handler emitted an ask
        // (in/human/<owner>). Match it by the emitting dispatch
        // (ELANUS_DISPATCH_ID), so two handlers of the same event can each
        // park on their own ask without cross-wiring; fall back to cause for
        // emitters that lost env.
        //
        // Crucially we match only an *unanswered* ask. On a resume the same
        // dispatch_id is reused, so the dispatch's prior (already-answered)
        // ask is still its newest by id; without this guard a handler that
        // re-suspends WITHOUT emitting a fresh ask would re-park on that old
        // correlation, whose answer already exists — resume_suspended would
        // then respawn it every tick forever. Excluding answered asks turns
        // that into the honest "nothing to wake it -> failed" below.
        let mb = profile::mailboxes(root);
        const UNANSWERED: &str = "AND NOT EXISTS (SELECT 1 FROM events ans
                             WHERE ans.type = ?3
                               AND ans.correlation_id = events.correlation_id)";
        resume_correlation = conn
            .query_row(
                &format!(
                    "SELECT correlation_id FROM events
                     WHERE emitted_by_dispatch = ?1 AND type = ?2
                       AND correlation_id IS NOT NULL {UNANSWERED}
                     ORDER BY id DESC LIMIT 1"
                ),
                params![r.dispatch_id, mb.human, mb.agent],
                |row| row.get(0),
            )
            .optional()?;
        if resume_correlation.is_none() {
            resume_correlation = conn
                .query_row(
                    &format!(
                        "SELECT correlation_id FROM events
                         WHERE cause_id = ?1 AND type = ?2 AND correlation_id IS NOT NULL
                           AND emitted_by_dispatch IS NULL {UNANSWERED}
                         ORDER BY id DESC LIMIT 1"
                    ),
                    params![r.event_id, mb.human, mb.agent],
                    |row| row.get(0),
                )
                .optional()?;
        }
        if resume_correlation.is_none() {
            // Suspended with no open ask to wake it: a failure, loudly —
            // never a silent hot-loop on an already-answered correlation.
            dstate = "failed";
        }
    }
    conn.execute(
        "UPDATE dispatches SET state=?1, exit_code=?2, resume_correlation=?3,
         finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?4",
        params![dstate, code, resume_correlation, r.dispatch_id],
    )?;
    trace::write(
        root,
        "obs/harness/dispatch/exit",
        &trace::Ids {
            event_id: Some(r.event_id),
            correlation_id: r.correlation.clone(),
            ..Default::default()
        },
        json!({
            "handler": handler_name(&r.out_path, conn, r.dispatch_id),
            "dispatch_id": r.dispatch_id,
            "exit_code": code,
            "state": dstate,
            "stdout": stdout,
            "stderr": stderr,
        }),
    );
    recompute_event_state(conn, r.event_id)?;
    Ok(())
}

fn handler_name(_out: &PathBuf, conn: &Connection, dispatch_id: i64) -> String {
    conn.query_row(
        "SELECT handler FROM dispatches WHERE id=?1",
        [dispatch_id],
        |r| r.get(0),
    )
    .unwrap_or_else(|_| "?".into())
}

fn recompute_event_state(conn: &Connection, event_id: i64) -> Result<()> {
    // 'orphaned' rows (pre-restart casualties) are excluded: a replay that
    // succeeds must be able to reach 'done'.
    let (n_running, n_suspended, n_failed): (i64, i64, i64) = conn.query_row(
        "SELECT
           SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),
           SUM(CASE WHEN state='suspended' THEN 1 ELSE 0 END),
           SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END)
         FROM dispatches WHERE event_id=?1 AND state != 'orphaned'",
        [event_id],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    let state = if n_running > 0 {
        "running"
    } else if n_suspended > 0 {
        "waiting_on_human"
    } else if n_failed > 0 {
        "failed"
    } else {
        "done"
    };
    // 'expired' is terminal and owned by the expiry sweep: an ask can carry
    // dispatches of its own (e.g. notify), and a slow one finishing after the
    // deadline must not clobber the expiry verdict with 'done'.
    if state == "running" || state == "waiting_on_human" {
        conn.execute(
            "UPDATE events SET state=?1 WHERE id=?2 AND state != 'expired'",
            params![state, event_id],
        )?;
    } else {
        conn.execute(
            "UPDATE events SET state=?1, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?2 AND state != 'expired'",
            params![state, event_id],
        )?;
    }
    Ok(())
}

/// A suspended handler whose resume correlation now has an answer (mail to
/// the agent, in/agent/<noun>) gets re-invoked with the original event plus
/// the answer. Only that causality chain parked; everything else kept flowing.
fn resume_suspended(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let mb = profile::mailboxes(root);
    let rows: Vec<(i64, i64, String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.event_id, d.handler, d.resume_correlation,
                    (SELECT a.id FROM events a
                     WHERE a.type=?1 AND a.correlation_id = d.resume_correlation
                     ORDER BY a.id LIMIT 1) AS answer_id
             FROM dispatches d
             WHERE d.state='suspended'
               AND EXISTS (SELECT 1 FROM events a
                           WHERE a.type=?1 AND a.correlation_id = d.resume_correlation)",
        )?;
        let r = stmt
            .query_map([&mb.agent], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (dispatch_id, event_id, handler, corr, answer_id) in rows {
        let mut envelope = events::envelope(conn, event_id)?;
        envelope["resume"] = events::envelope(conn, answer_id)?;
        conn.execute(
            "UPDATE dispatches SET state='running', exit_code=NULL, finished_at=NULL WHERE id=?1",
            [dispatch_id],
        )?;
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [event_id])?;
        spawn_handler(
            root,
            conn,
            running,
            event_id,
            &envelope,
            PathBuf::from(&handler),
            Some(dispatch_id),
            Some(corr),
        )?;
    }
    Ok(())
}

/// Drain worker completion reports and settle each delivery event's state on the
/// dispatcher's connection (workers never touch it). A finished resume moves its
/// event `running -> done`; a tool that errored or exited non-zero moves it to
/// `failed` (the message was delivered and acted on, even if the turn failed — it
/// is not re-driven). Either way the event leaves `running`, so it is not replayed
/// on the next restart. The in-flight count drops; a worker with nothing left is
/// retired so an idle session holds no thread.
fn settle_code_deliveries(root: &Root, conn: &Connection, code: &mut CodeDrivers) -> Result<()> {
    while let Ok(done) = code.done_rx.try_recv() {
        code.claimed.remove(&done.event_id);
        let failed = !matches!(done.success, Some(true));
        let state = if failed { "failed" } else { "done" };
        conn.execute(
            "UPDATE events SET state=?1, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?2 AND state='running'",
            params![state, done.event_id],
        )?;
        // A small completion obs so a waiter can thread the result by the
        // delivery's correlation_id (M4's planner reads this). Kernel-emitted, so
        // the announce sweep delivers it; it carries no read authority.
        let _ = events::emit(
            root,
            conn,
            EmitOpts {
                payload: Some(json!({
                    "session": done.session,
                    "failed": failed,
                    "detail": trace::clip(&done.detail, 2000),
                })),
                correlation: done.correlation.clone(),
                cause: Some(done.event_id),
                ..EmitOpts::new("obs/agent/code/delivery/complete")
            },
        );
        // Retire a now-idle worker (inflight back to 0): drop the sender so the
        // thread's recv() ends and it joins out. A later delivery to the same
        // session simply spawns a fresh worker.
        if let Some(w) = code.workers.get_mut(&done.session) {
            w.inflight = w.inflight.saturating_sub(1);
            if w.inflight == 0 {
                code.workers.remove(&done.session);
            }
        }
    }
    Ok(())
}

/// Claim pending coding-session deliveries and hand each to its session's worker
/// thread. A `pending` event whose topic `recognize_delivery` matches an existing
/// `code-*` record is a drive: we mark it `running` (durability — replays on
/// restart), pull the message, and enqueue it on the per-session worker (spawned
/// on first sight). Everything else is left for `dispatch_pending`. Per-session
/// serialization is the single worker thread; the `claimed` set stops a re-claim
/// within this process before the worker has reported back.
fn drive_code_deliveries(root: &Root, conn: &Connection, code: &mut CodeDrivers) -> Result<()> {
    // Only `in/agent/*` pending events can be a delivery — let the SQL prefilter
    // do the obvious narrowing so a busy daemon doesn't scan every pending event.
    let pending: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id, payload FROM events
             WHERE state='pending' AND type LIKE 'in/agent/%'
             ORDER BY priority DESC, id ASC LIMIT 100",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, etype, corr, payload) in pending {
        if code.claimed.contains(&id) {
            continue; // already handed to a worker this process; row not yet settled
        }
        let Some((session, _noun)) = crate::codeagent::recognize_delivery(root, &etype) else {
            continue; // not addressed to a known coding session — leave for dispatch_pending
        };
        let pv: Value = payload
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let Some(message) = crate::codeagent::delivery_message(&pv) else {
            // A delivery with no prompt/text is not drivable: settle it done
            // (it WAS delivered, there is just nothing to resume on) rather than
            // leave it pending forever or hand the worker an empty turn.
            conn.execute(
                "UPDATE events SET state='done', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id=?1 AND state='pending'",
                [id],
            )?;
            trace::write(
                root,
                "obs/agent/code/delivery/empty",
                &trace::Ids { event_id: Some(id), correlation_id: corr.clone(), ..Default::default() },
                json!({ "session": session, "reason": "no prompt/text in payload" }),
            );
            continue;
        };
        // Claim it durably BEFORE hand-off: a restart mid-resume re-pends a
        // `running` event and replays it (at-least-once). The in-memory guard
        // stops a same-process re-claim while the worker drains it.
        conn.execute(
            "UPDATE events SET state='running' WHERE id=?1 AND state='pending'",
            [id],
        )?;
        code.claimed.insert(id);
        trace::write(
            root,
            "obs/agent/code/delivery/accepted",
            &trace::Ids { event_id: Some(id), correlation_id: corr.clone(), ..Default::default() },
            json!({ "session": session, "type": etype }),
        );
        enqueue_code_job(
            root,
            code,
            &session,
            CodeJob { event_id: id, correlation: corr, message },
        );
    }
    Ok(())
}

/// Enqueue a job on the session's worker, spawning the worker thread on first
/// sight. The worker owns the FIFO queue: it drains jobs ONE AT A TIME (a session
/// runs a single resume at a time — the native tool isn't concurrent-safe), and
/// reports each outcome back on the shared completion channel. If the send fails
/// (a worker that died), the claim is dropped so the event re-pends and replays.
fn enqueue_code_job(root: &Root, code: &mut CodeDrivers, session: &str, job: CodeJob) {
    let event_id = job.event_id;
    // Spawn-on-first-sight: a session with no live worker gets one. The worker
    // captures its own Root clone and the done sender; it opens its own db inside
    // resume_capture, never touching the dispatcher's connection.
    if !code.workers.contains_key(session) {
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        let root = root.clone();
        let done_tx = code.done_tx.clone();
        let sess = session.to_string();
        let spawned = std::thread::Builder::new()
            .name(format!("code-driver-{sess}"))
            .spawn(move || code_worker(root, sess, rx, done_tx));
        match spawned {
            Ok(_) => {
                code.workers.insert(session.to_string(), CodeWorker { tx, inflight: 0 });
            }
            Err(e) => {
                eprintln!("[daemon] code worker spawn for {session} failed: {e}");
                // Couldn't spawn — report a synthetic failure so the event settles
                // (it stays `running` otherwise until a restart replays it).
                let _ = code.done_tx.send(CodeDone {
                    session: session.to_string(),
                    event_id,
                    correlation: job.correlation,
                    success: None,
                    detail: format!("worker spawn failed: {e}"),
                });
                return;
            }
        }
    }
    if let Some(w) = code.workers.get_mut(session) {
        let corr = job.correlation.clone();
        if w.tx.send(job).is_ok() {
            w.inflight += 1;
        } else {
            // The worker is gone; drop the claim so the event re-pends next tick.
            eprintln!("[daemon] code worker for {session} is gone; re-queueing event {event_id}");
            code.workers.remove(session);
            let _ = code.done_tx.send(CodeDone {
                session: session.to_string(),
                event_id,
                correlation: corr,
                success: None,
                detail: "worker channel closed".into(),
            });
        }
    }
}

/// A per-session worker thread: drain the queue FIFO, running one resume at a time
/// and reporting each outcome. Ends when its sender drops (the tick retires an idle
/// worker) — a clean, crash-only lifecycle. Never panics out: a resume error is
/// reported as `success=None`, not propagated.
fn code_worker(root: Root, session: String, rx: Receiver<CodeJob>, done_tx: Sender<CodeDone>) {
    while let Ok(job) = rx.recv() {
        let (success, detail) = match crate::codeagent::resume_capture(&root, &session, &job.message) {
            Ok(outcome) => (
                Some(outcome.success),
                format!("exit_code={:?}", outcome.exit_code),
            ),
            Err(e) => (None, format!("{e:#}")),
        };
        // If the receiver is gone (daemon shutting down), the event stays
        // `running` and replays on the next start — at-least-once holds.
        let _ = done_tx.send(CodeDone {
            session: session.clone(),
            event_id: job.event_id,
            correlation: job.correlation,
            success,
            detail,
        });
    }
}

fn dispatch_pending(root: &Root, conn: &Connection, running: &mut Vec<Running>) -> Result<()> {
    let pending: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id FROM events WHERE state='pending'
             ORDER BY priority DESC, id ASC LIMIT 100",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, etype, corr) in pending {
        let handlers = packages::matching_exec_handlers(root, conn, &etype)?;
        if handlers.is_empty() {
            // No consumers: the event just lives in the log.
            conn.execute(
                "UPDATE events SET state='done', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [id],
            )?;
            continue;
        }
        if is_throttled(conn, &etype, running)? {
            continue; // stays pending; revisited next tick
        }
        let envelope = events::envelope(conn, id)?;
        // Hook plane, pre_dispatch: may veto the event before any handler
        // runs (allow/deny only — the envelope is ledger-backed, no rewrite).
        let hook_ids = trace::Ids {
            event_id: Some(id),
            correlation_id: corr.clone(),
            ..Default::default()
        };
        let gate = hooks::run_chain(
            root,
            conn,
            "pre_dispatch",
            &etype,
            json!({ "point": "pre_dispatch", "event": envelope }),
            &hook_ids,
        )?;
        // Resident hooks run after the exec chain (same order as the tool
        // call points; the rationale lives in src/exec.rs). pre_dispatch is
        // allow/deny only — the envelope is ledger-backed, rewrites are
        // ignored downstream. The kv gate makes this free when nothing is
        // registered; with the daemon consulting its own broker the round
        // trip is loopback into the ntex thread, never a self-deadlock.
        let gate = if gate.allow {
            crate::resident::consult(root, conn, "pre_dispatch", &etype, gate.subject, &hook_ids)
        } else {
            gate
        };
        if !gate.allow {
            // The deny itself is already on the recorder (obs/harness/hook/...).
            conn.execute(
                "UPDATE events SET state='denied', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [id],
            )?;
            continue;
        }
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [id])?;
        for (_pkg, h) in handlers {
            spawn_handler(root, conn, running, id, &envelope, h, None, corr.clone())?;
        }
    }
    Ok(())
}

fn is_throttled(conn: &Connection, etype: &str, running: &[Running]) -> Result<bool> {
    let rows: Vec<(String, Option<i64>, Option<i64>, i64)> = {
        let mut stmt = conn
            .prepare("SELECT event_type, max_concurrent, rate_per_min, coalesce FROM throttles")?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let mut blocked = false;
    for (pat, max_concurrent, rate_per_min, coalesce) in rows {
        if !crate::topic::matches(&pat, etype) {
            continue;
        }
        // The algedonic exemption: never queued behind, never batched.
        if coalesce == 0 {
            return Ok(false);
        }
        if let Some(maxc) = max_concurrent {
            let n = running
                .iter()
                .filter(|r| crate::topic::matches(&pat, &r.etype))
                .count() as i64;
            if n >= maxc {
                blocked = true;
            }
        }
        if let Some(rate) = rate_per_min {
            let types: Vec<String> = {
                let mut stmt = conn.prepare(
                    "SELECT e.type FROM dispatches d JOIN events e ON e.id = d.event_id
                     WHERE d.started_at > strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds')",
                )?;
                let r = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                r
            };
            if types
                .iter()
                .filter(|t| crate::topic::matches(&pat, t))
                .count() as i64
                >= rate
            {
                blocked = true;
            }
        }
    }
    Ok(blocked)
}

#[allow(clippy::too_many_arguments)]
fn spawn_handler(
    root: &Root,
    conn: &Connection,
    running: &mut Vec<Running>,
    event_id: i64,
    envelope: &Value,
    handler: PathBuf,
    reuse_dispatch: Option<i64>,
    correlation: Option<String>,
) -> Result<()> {
    let dispatch_id = match reuse_dispatch {
        Some(d) => d,
        None => {
            conn.execute(
                "INSERT INTO dispatches(event_id, handler) VALUES (?1, ?2)",
                params![event_id, handler.display().to_string()],
            )?;
            conn.last_insert_rowid()
        }
    };
    let out_path = root.run_dir().join(format!("d{dispatch_id}.out"));
    let err_path = root.run_dir().join(format!("d{dispatch_id}.err"));
    let out_f = std::fs::File::create(&out_path)?;
    let err_f = std::fs::File::create(&err_path)?;

    // Handlers call back into `elanus`; make sure this binary wins on PATH.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let path_env = format!(
        "{}:{}",
        exe_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let etype = envelope["type"].as_str().unwrap_or("?").to_string();
    let is_resume = envelope
        .get("resume")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    let mut cmd = Command::new(&handler);
    cmd.current_dir(&root.dir)
        .stdin(Stdio::piped())
        .stdout(out_f)
        .stderr(err_f)
        .env_dual("EVENT_ID", event_id.to_string())
        .env_dual("DISPATCH_ID", dispatch_id.to_string())
        .env_dual("DB", root.db())
        .env_dual("TRACE", root.trace_file())
        .env_dual("ROOT", &root.dir)
        .env_dual("PROFILE", root.profile_dir("default"))
        .env("PATH", path_env);
    if let Some(c) = envelope["cause_id"].as_i64() {
        cmd.env_dual("CAUSE_ID", c.to_string());
    }
    if let Some(c) = &correlation {
        cmd.env_dual("CORRELATION_ID", c);
    }
    if is_resume {
        cmd.env_dual("RESUME", "1");
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write as _;
                // EPIPE from an instantly-exiting handler is fine.
                let _ = stdin.write_all(envelope.to_string().as_bytes());
            }
            trace::write(
                root,
                "obs/harness/dispatch/spawn",
                &trace::Ids {
                    event_id: Some(event_id),
                    cause_id: envelope["cause_id"].as_i64(),
                    correlation_id: correlation.clone(),
                    ..Default::default()
                },
                json!({
                    "handler": handler.display().to_string(),
                    "dispatch_id": dispatch_id,
                    "type": etype,
                    "resume": is_resume,
                }),
            );
            running.push(Running {
                child,
                dispatch_id,
                event_id,
                etype,
                correlation,
                out_path,
                err_path,
            });
        }
        Err(e) => {
            conn.execute(
                "UPDATE dispatches SET state='failed', exit_code=-2,
                 finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
                [dispatch_id],
            )?;
            trace::write(
                root,
                "obs/harness/dispatch/exit",
                &trace::Ids {
                    event_id: Some(event_id),
                    ..Default::default()
                },
                json!({ "handler": handler.display().to_string(), "spawn_error": e.to_string(), "state": "failed" }),
            );
            recompute_event_state(conn, event_id)?;
        }
    }
    Ok(())
}

fn read_clipped(p: &PathBuf) -> String {
    std::fs::read_to_string(p)
        .map(|s| trace::clip(&s, 4096))
        .unwrap_or_default()
}

/// Profile [throttle.*] sections merge into the throttle table at daemon start.
fn merge_profile_throttles(root: &Root, conn: &Connection) {
    let Ok(entries) = std::fs::read_dir(root.profiles()) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().to_string();
        if let Ok((prof, _)) = profile::load(root, &name) {
            for (pat, t) in &prof.throttle {
                if let Err(err) = packages::upsert_throttle(conn, pat, t) {
                    eprintln!("[daemon] throttle merge {pat}: {err:#}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn tmp_root() -> Root {
        let dir = std::env::temp_dir().join(format!(
            "elanus-dispatch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    /// drive_code_deliveries CLAIMS a recognized coding-session delivery (marks it
    /// `running`, enqueues it on a worker, records it `claimed`) and LEAVES an
    /// unrecognized in/agent/* event `pending` for the normal dispatch path. We
    /// keep the worker from running a real tool by intercepting the queue: the
    /// session's worker is pre-seeded with a sink channel before the drive, so the
    /// job lands in our channel instead of spawning `codex`. This proves the
    /// recognition + durable-claim half without burning a model turn.
    #[test]
    fn drive_claims_recognized_and_skips_unrecognized() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        // A recorded codex session — its mailbox is drivable.
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-aaaa0001".into(),
                native_session: "thread-1".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
            },
        )
        .unwrap();
        // Two pending events: one to the recorded session (drivable), one to an
        // unknown code-* conv (must be ignored, left for dispatch_pending).
        let drivable = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "hello there" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-aaaa0001")
            },
        )
        .unwrap();
        let unknown = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "nobody home" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-ffff9999")
            },
        )
        .unwrap();

        let mut code = CodeDrivers::default();
        // Pre-seed the worker so the enqueued job goes to OUR channel, never a
        // spawned tool: the drive sees a live worker and sends to it.
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-aaaa0001".into(), CodeWorker { tx, inflight: 0 });

        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        // The drivable event is now `running` and claimed; a job was enqueued.
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [drivable], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "running", "recognized delivery is claimed (running)");
        assert!(code.claimed.contains(&drivable));
        let job = rx.try_recv().expect("a job was enqueued for the session");
        assert_eq!(job.event_id, drivable);
        assert_eq!(job.message, "hello there");

        // The unknown-conv event is untouched — left pending for dispatch_pending
        // (which will mark it done as a no-consumer event), never resumed.
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [unknown], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "pending", "unrecognized in/agent event is left for dispatch");

        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// A second delivery to the SAME session, already claimed, is NOT re-enqueued
    /// while the first is in flight: the `claimed` guard plus the single worker
    /// thread serialize them. Here we keep the first job in flight (unread in our
    /// sink) and confirm a re-drive of the same row does not double-send.
    #[test]
    fn same_session_deliveries_do_not_double_run() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-bbbb0002".into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
            },
        )
        .unwrap();
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "one" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-bbbb0002")
            },
        )
        .unwrap();
        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-bbbb0002".into(), CodeWorker { tx, inflight: 0 });

        // First drive claims + enqueues exactly once.
        drive_code_deliveries(&root, &conn, &mut code).unwrap();
        // Second drive in the same process: the row is now `running` (not pending)
        // AND it is in `claimed`, so it is not selected and not re-enqueued.
        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        assert_eq!(rx.try_recv().map(|j| j.event_id).ok(), Some(ev));
        assert!(rx.try_recv().is_err(), "the same delivery is never enqueued twice");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// The worker model serializes per session by construction: a single thread
    /// draining a FIFO queue runs jobs ONE AT A TIME, in order — never two
    /// overlapping for the same session. This proves the structural guarantee
    /// `code_worker` relies on (one thread + one channel = strict serialization)
    /// without invoking a real tool.
    #[test]
    fn one_worker_thread_serializes_its_queue_fifo() {
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let order: Arc<std::sync::Mutex<Vec<usize>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (c, m, o) = (concurrent.clone(), max_seen.clone(), order.clone());
        let h = std::thread::spawn(move || {
            // Same shape as code_worker: drain FIFO, one job at a time.
            while let Ok(n) = rx.recv() {
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5)); // a "resume turn"
                o.lock().unwrap().push(n);
                c.fetch_sub(1, Ordering::SeqCst);
            }
        });
        // Two rapid deliveries to the same session.
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);
        h.join().unwrap();
        // Never overlapped, and ran in FIFO order.
        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "two same-session resumes never overlap");
        assert_eq!(*order.lock().unwrap(), vec![1, 2], "FIFO order preserved");
    }

    /// settle_code_deliveries moves a claimed `running` delivery to `done` on
    /// success / `failed` on a non-zero-or-errored resume, drops it from `claimed`,
    /// and retires the now-idle worker. (We feed a CodeDone directly — the worker
    /// side is exercised by the FIFO test above.)
    #[test]
    fn settle_marks_done_or_failed_and_retires_worker() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "x" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-cccc0003")
            },
        )
        .unwrap();
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [ev]).unwrap();
        let mut code = CodeDrivers::default();
        code.claimed.insert(ev);
        let (tx, _rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-cccc0003".into(), CodeWorker { tx, inflight: 1 });
        code.done_tx
            .send(CodeDone {
                session: "code-cccc0003".into(),
                event_id: ev,
                correlation: None,
                success: Some(true),
                detail: "exit_code=Some(0)".into(),
            })
            .unwrap();

        settle_code_deliveries(&root, &conn, &mut code).unwrap();

        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "done", "a successful resume settles the delivery done");
        assert!(!code.claimed.contains(&ev), "settled event leaves the claimed set");
        assert!(!code.workers.contains_key("code-cccc0003"), "idle worker is retired");

        // A failed resume settles `failed`.
        let ev2 = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "y" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-dddd0004")
            },
        )
        .unwrap();
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [ev2]).unwrap();
        code.claimed.insert(ev2);
        code.done_tx
            .send(CodeDone {
                session: "code-dddd0004".into(),
                event_id: ev2,
                correlation: None,
                success: None,
                detail: "boom".into(),
            })
            .unwrap();
        settle_code_deliveries(&root, &conn, &mut code).unwrap();
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev2], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "failed", "an errored resume settles the delivery failed");
        let _ = std::fs::remove_dir_all(&root.dir);
    }
}

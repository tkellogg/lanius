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
    /// Where to route the worker's completion (M4-A): the requester captured from
    /// the inbound delivery (explicit `reply_to`, else the broker-verified
    /// `sender`). None = a plain worker resume with no one waiting (the M2-B
    /// behavior, unchanged).
    requester: Option<crate::codeagent::DeliveryRequester>,
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
    /// Where to route the completion (M4-A): carried through from the job so the
    /// settle step can deliver to the requester's mailbox. None = no routing.
    requester: Option<crate::codeagent::DeliveryRequester>,
    /// The worker's VERBATIM final message for the routed completion (M4-A
    /// follow-on) — its actual last answer, NOT a generated summary. None when the
    /// worker produced no final text, OR when the resume primitive itself errored
    /// (no stream to harvest); `route_completion` then falls back to a minimal
    /// factual line built from `detail`.
    final_text: Option<String>,
    /// The on-disk paths the worker reported writing this turn (deduped, possibly
    /// empty), carried verbatim onto the routed completion.
    file_changes: Vec<String>,
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
    // M5: also release the room membership + advisory claims of any coding session
    // whose owning process is dead (a SIGKILL'd launcher never ran its clean
    // release). A dead session's claims must not linger in its roommates' per-turn
    // injections forever (the lease-released membership of docs/topics.md
    // decided-5). Crash-only, same liveness sweep as the credential reaper.
    for (room, sess) in crate::codesession::reap_dead_members(root) {
        eprintln!("[daemon] released claims of dead session {sess} in room {room}");
    }
    // Recover any planner wake lost to a crash in the settle->route gap (M4-A
    // reliability residual): a driven worker delivery may have settled `done` (or
    // been re-pended and deduped) while its routed completion was never emitted, so
    // the planner would never be woken. Re-derive the route from durable state and
    // re-emit it. Crash-only, idempotent (the routed key + a no-route-exists check).
    if let Err(e) = reconcile_lost_routes(root, &conn) {
        eprintln!("[daemon] reconciling lost completion routes: {e:#}");
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
        // Keep the coding-session projection (obs/trace -> sqlite) fresh each
        // tick; best-effort so a projection error never stalls the daemon.
        let _ = crate::code_projection::project_trace(root);
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
    tick_schedules(root, conn)?;
    expire_deadlines(root, conn)?;
    announce_ledger_events(root, conn)?;
    reap(root, conn, running)?;
    settle_code_deliveries(root, conn, code)?;
    reap_dead_spawn_edges(root, conn)?;
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
/// emit that did NOT arrive over the bus: CLI `lanius emit`, cron, the
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
        // receipts via `lanius emit`) keep their obs/harness/ledger/emit echo
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
            .env_dual("PACKAGE", &pkg.name)
            .env_dual("SCRATCH", &scratch)
            .env_dual("BUS_ADDR", &addr)
            .env_dual("BUS_TOKEN", &token)
            .env(
                "LANIUS_SESSION_EXPIRY_S",
                proc_.session_expiry_s.to_string(),
            )
            .env(
                "LANIUS_HTTP_PORT",
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

/// One-shot schedules (docs/handoffs/timers.md): fire each due row once as a
/// plain kernel emit, then mark it fired. A sibling of tick_crons on the same
/// tick — not a rival clock. Durable + idempotent: the sched:<id> key dedupes
/// the same firing across a restart or a crash between emit and the fired
/// flip, and the fired flag skips already-delivered rows on the fast path.
/// Authorization was decided at schedule time (the tool / the CLI); the fire
/// has no reliable actor to re-authorize, so it does not re-check here.
pub(crate) fn tick_schedules(root: &Root, conn: &Connection) -> Result<()> {
    let now = trace::now_iso();
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, emit_type, payload FROM scheduled_events
             WHERE fired = 0 AND fire_at <= ?1",
        )?;
        let r = stmt
            .query_map([&now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for (id, emit_type, payload) in rows {
        events::emit(
            root,
            conn,
            EmitOpts {
                payload: payload
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok()),
                // Dedupes this firing across daemon restarts / mid-fire crash.
                idempotency: Some(format!("sched:{id}")),
                ..EmitOpts::new(&emit_type)
            },
        )?;
        conn.execute(
            "UPDATE scheduled_events SET fired = 1 WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(())
}

/// The scan `expire_deadlines` runs every tick. Its `WHERE` leads with
/// `e.type = ?1 AND e.deadline IS NOT NULL AND … deadline < now`, which rides
/// `idx_events_type_deadline (type, deadline)` (storage-hardening M3) instead of
/// full-scanning `events` (9.3ms @ 200k rows → low µs); the correlated
/// `NOT EXISTS` rides `idx_events_correlation`. Kept as a const so the
/// EXPLAIN-plan regression test (`expire_deadlines_uses_type_deadline_index`)
/// guards the EXACT production query against drift that would reintroduce the
/// scan.
const EXPIRE_DEADLINES_SELECT: &str =
    "SELECT e.id, e.correlation_id, e.default_action FROM events e
             WHERE e.type = ?1 AND e.deadline IS NOT NULL
               AND e.state != 'expired' AND e.correlation_id IS NOT NULL
               AND e.deadline < strftime('%Y-%m-%dT%H:%M:%fZ','now')
               AND NOT EXISTS (SELECT 1 FROM events a
                               WHERE a.type = ?2 AND a.correlation_id = e.correlation_id)";

/// Defaults are the big unblock: an expired ask executes its default and logs
/// the assumption as an ordinary answer event (mail to the agent) —
/// auditable, vetoable.
fn expire_deadlines(root: &Root, conn: &Connection) -> Result<()> {
    let mb = profile::mailboxes(root);
    let rows: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare(EXPIRE_DEADLINES_SELECT)?;
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
        // (LANIUS_DISPATCH_ID), so two handlers of the same event can each
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
        // delivery's correlation_id (a dashboard / monitor reads this). Kernel-
        // emitted, so the announce sweep delivers it; it carries no read authority.
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
        // Route the completion to the requester's mailbox (M4-A — the loop
        // closing). When the original delivery named a requester (an explicit
        // reply_to, else the broker-verified sender), publish the completion to
        // that mailbox carrying the SAME correlation_id, so a planner is resumed to
        // react. If the requester is itself a coding session, the EXISTING M2-B
        // machinery (`drive_code_deliveries`) picks this pending in/agent event up
        // next tick and resumes the planner — exactly like any other delivery; that
        // is the headless loop. (The shared helper builds the payload + emits, and
        // is reused by the boot reconciliation that recovers a route lost to a crash
        // in this settle->route gap.)
        if let Some(req) = &done.requester {
            route_completion(
                root,
                conn,
                done.event_id,
                &done.session,
                &req.reply_to,
                failed,
                done.final_text.as_deref(),
                &done.file_changes,
                &done.detail,
                done.correlation.as_deref(),
            );
        }
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

/// Whether a worker delivery (event id `cause`) already has its completion routed
/// to `reply_to` — the idempotency guard shared by settle and the boot
/// reconciliation. A routed completion is the unique `(cause_id, type)` pair, so
/// its presence means the route already happened (this run or a prior one).
fn route_already_emitted(conn: &Connection, cause: i64, reply_to: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE cause_id = ?1 AND type = ?2",
        params![cause, reply_to],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Route a worker's completion to the requester's mailbox (M4-A — the loop
/// closing), shared by `settle_code_deliveries` (the live path) and
/// `reconcile_lost_routes` (the boot recovery of a route lost to a crash in the
/// settle->route gap). Idempotent: it no-ops if a completion was already routed for
/// this worker delivery (the `(cause, reply_to)` guard) and carries a stable
/// `code-complete:<worker-event-id>` idempotency key, so a planner is never woken
/// twice for one completion even if both callers run. The routed event is a
/// kernel-minted ledger delivery (sender=kernel): when the requester is a coding
/// session, `drive_code_deliveries` picks it up next tick and resumes the planner.
#[allow(clippy::too_many_arguments)]
fn route_completion(
    root: &Root,
    conn: &Connection,
    worker_event_id: i64,
    session: &str,
    reply_to: &str,
    failed: bool,
    // The worker's VERBATIM final message (None when it produced none, or when the
    // resume errored before any stream). NEVER a generated summary.
    final_text: Option<&str>,
    // The on-disk paths the worker reported writing this turn (possibly empty).
    file_changes: &[String],
    // A terse diagnostic (exit_code / error / "recovered after restart") used ONLY
    // for the minimal factual fallback line when there is no final_text.
    detail: &str,
    correlation: Option<&str>,
) {
    if !crate::topic::valid_name(reply_to) {
        eprintln!(
            "[daemon] completion of {session} has an unroutable reply_to {reply_to:?}; dropping the route"
        );
        return;
    }
    // Idempotency: never route the same completion twice (settle + reconcile, or
    // two boots). If a route already exists for this worker delivery, stop.
    match route_already_emitted(conn, worker_event_id, reply_to) {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            eprintln!("[daemon] checking for an existing route of {session}: {e:#}");
            return;
        }
    }
    let noun = crate::codesession::read_record(root, session)
        .ok()
        .flatten()
        .map(|r| r.agent_noun)
        .unwrap_or_default();
    let obs_pointer = if noun.is_empty() {
        Value::Null
    } else {
        json!(format!(
            "obs/agent/{}/{}/#",
            crate::topic::encode_segment(&noun),
            crate::topic::encode_segment(session),
        ))
    };
    // The worker's VERBATIM answer is the meat of the completion. We never
    // summarize: when the worker produced final text we carry it as-is (already
    // capped/marked upstream); only when it produced NONE (a silent turn, or a
    // resume that errored before any stream) do we fall back to a minimal factual
    // line built from the terse diagnostic — never a fabricated description of what
    // the worker "did".
    let pointer = obs_pointer.as_str().unwrap_or("its obs subtree");
    let answer = match final_text {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => format!("(worker {session} completed with no final message; {detail})"),
    };
    // The files the worker changed on disk, as the tool itself reported them.
    let files_line = if file_changes.is_empty() {
        "Files changed: none reported.".to_string()
    } else {
        format!(
            "Files changed:\n{}",
            file_changes
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    // The prompt reads like the worker's actual answer, then the files it touched,
    // then the pointer to the full conversation — clean and honest, no summary.
    let status_word = if failed { "FAILED" } else { "completed" };
    let prompt = format!(
        "Worker session {session} {status_word} the work you dispatched.\n\n\
         {answer}\n\n\
         {files_line}\n\n\
         (full conversation at {pointer})"
    );
    let routed = events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(json!({
                "prompt": prompt,
                "worker": session,
                "failed": failed,
                // The worker's verbatim final answer + the paths it wrote — the
                // legible result a parent reads directly (no summary).
                "final_text": answer,
                "file_changes": file_changes,
                "worker_obs": obs_pointer,
                // A stable idempotency key on the routed delivery: a replayed
                // completion (or a settle + reconcile overlap) dedupes when it
                // drives the planner.
                "idempotency_key": format!("code-complete:{worker_event_id}"),
            })),
            // Thread the planner's resume by the SAME correlation the requester
            // used, so the round trip is one conversation.
            correlation: correlation.map(str::to_string),
            cause: Some(worker_event_id),
            ..EmitOpts::new(reply_to)
        },
    );
    match routed {
        Ok(rid) => trace::write(
            root,
            "obs/agent/code/delivery/routed",
            &trace::Ids {
                event_id: Some(rid),
                correlation_id: correlation.map(str::to_string),
                ..Default::default()
            },
            json!({ "worker": session, "reply_to": reply_to, "failed": failed }),
        ),
        Err(e) => eprintln!("[daemon] routing completion of {session} to {reply_to} failed: {e:#}"),
    }
}

/// Reap detached-spawn workers that died without reporting (cross-harness-death
/// M2). `lanius code spawn` fires a DETACHED, unparented worker: nothing can
/// `wait()` on it, so if its wrapper is SIGKILL'd (or crashes) before its own
/// `emit_completion_delivery`, the spawner would hang forever — the driven path's
/// `reconcile_lost_routes` only covers `code_delivery_keys`. Each spawn records a
/// durable `code_spawn_edges` row (spawner, correlation, wrapper pid); this sweep,
/// beside `settle_code_deliveries` in the tick, finds unsettled edges whose wrapper
/// pid is dead and synthesizes the SAME `{failed:true}` completion-mail the worker
/// would have sent, then settles the edge.
///
/// Exactly-once: the settle is CLAIMED atomically (`claim_spawn_edge_on` —
/// `UPDATE ... WHERE settled_at IS NULL`) before mailing. A worker finishing in the
/// same tick claims the edge from its own process; whichever write commits first
/// wins, and the loser's claim returns non-`Claimed` and mails nothing. (The reaper
/// only ever sees a DEAD pid, so it never races a still-running worker — a live
/// worker's own completion path owns the claim.) A mail failure releases the claim
/// so the next tick retries rather than losing the completion.
fn reap_dead_spawn_edges(root: &Root, conn: &Connection) -> Result<()> {
    for edge in crate::codesession::dead_unsettled_spawn_edges(conn) {
        match crate::codesession::claim_spawn_edge_on(conn, &edge.worker_session) {
            Ok(crate::codesession::SettleClaim::Claimed) => {}
            // The worker's own completion won the claim (or the edge vanished) —
            // nothing to synthesize.
            _ => continue,
        }
        match crate::codeagent::emit_reaper_failure_delivery(
            root,
            conn,
            &edge.worker_session,
            &edge.spawner,
            edge.correlation.as_deref(),
        ) {
            Ok(rid) => trace::write(
                root,
                "obs/agent/code/spawn/reaped",
                &trace::Ids {
                    event_id: Some(rid),
                    correlation_id: edge.correlation.clone(),
                    ..Default::default()
                },
                json!({
                    "worker": edge.worker_session,
                    "spawner": edge.spawner,
                    "pid": edge.worker_pid,
                }),
            ),
            Err(e) => {
                eprintln!(
                    "[daemon] reaping dead spawn worker {} failed: {e:#}",
                    edge.worker_session
                );
                // Release the claim so the next tick retries the completion.
                crate::codesession::unclaim_spawn_edge(root, &edge.worker_session);
            }
        }
    }
    Ok(())
}

/// Recover any planner wake lost to a crash in the settle->route gap (M4-A
/// reliability residual). The settle UPDATE (worker delivery -> done) and the
/// routed completion emit are separate autocommit transactions; a crash between
/// them settles the worker delivery but never emits the route, and the boot sweep
/// only re-pends `running` events — it never revisits `done` — so the planner's
/// wake would be lost forever.
///
/// This boot sweep re-derives the route entirely from DURABLE state: every
/// `code_delivery_keys` row marks a delivery that was actually DRIVEN (the key is
/// recorded only when a delivery is claimed for a worker, never for an
/// empty/no-prompt or no-consumer settle). For each, we read the original delivery
/// event's persisted `sender`/`payload`/`correlation`, re-derive the requester the
/// same way the live path did, and — if a requester resolves and no completion was
/// ever routed (`route_already_emitted`) — emit the route now. Idempotent and
/// crash-only, like every other boot reconciliation (orphaned dispatches, stale
/// leases, orphaned credentials). A delivery with no requester (a plain owner
/// resume) resolves to None and is skipped; an already-routed one is skipped by the
/// guard, so a clean boot does nothing.
///
/// We cannot reconstruct the worker's live result text after a crash (it was in
/// memory), but the routed completion's job is to WAKE the planner to read the
/// recorded state — the worker's full transcript is durable on the bus — so a
/// recovered route carries an honest "completed (recovered after a restart)"
/// result line and the obs pointer; the planner reads actual state regardless.
fn reconcile_lost_routes(root: &Root, conn: &Connection) -> Result<()> {
    // Every driven delivery's worker event id (the row's event_id), oldest first.
    let driven: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT event_id, session FROM code_delivery_keys
             WHERE event_id IS NOT NULL ORDER BY event_id ASC",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let mut recovered = 0u32;
    for (worker_event_id, session) in driven {
        // The original delivery event's persisted provenance + payload. If the row
        // is gone (shouldn't happen pre-release), skip it.
        let row: Option<(String, Option<String>, Option<String>, String)> = conn
            .query_row(
                "SELECT type, sender, payload, correlation_id FROM events WHERE id = ?1",
                [worker_event_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ))
                },
            )
            .optional()?;
        let Some((_etype, sender, payload, correlation)) = row else {
            continue;
        };
        let pv: Value = payload
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let corr_opt = (!correlation.is_empty()).then_some(correlation.as_str());
        let Some(req) =
            crate::codeagent::delivery_requester(root, &pv, sender.as_deref(), corr_opt)
        else {
            continue; // a plain owner/kernel resume with no one waiting — nothing to route
        };
        // Already routed (this is a clean boot, or a prior reconcile did it)? Skip.
        if route_already_emitted(conn, worker_event_id, &req.reply_to).unwrap_or(true) {
            continue;
        }
        // The wake was lost — re-emit the route now. The worker's live final text
        // and file changes were in memory and are gone; we pass None/empty and an
        // honest "recovered" detail, so the fallback line is factual (the worker's
        // full transcript is still durable on the bus under the obs pointer the
        // route carries). NOT a fabricated summary.
        route_completion(
            root,
            conn,
            worker_event_id,
            &session,
            &req.reply_to,
            false,
            None,
            &[],
            "route recovered after a restart; read the recorded transcript",
            corr_opt,
        );
        recovered += 1;
    }
    if recovered > 0 {
        eprintln!("[daemon] recovered {recovered} lost completion route(s) at boot");
    }
    Ok(())
}

/// How stale a pending coding-session delivery may be before it is HELD rather
/// than auto-driven (docs/handoffs/bus-resilience.md M3). A normal driven
/// round-trip (spawn → worker → completion mail → planner resume) settles in
/// seconds-to-minutes, and an idle session resumed the same day should still get
/// its mail; a full day is well past both, so a delivery older than this is
/// "from a previous session" — the exact surprise report (b) describes.
const STALE_DELIVERY_HORIZON: chrono::Duration = chrono::Duration::hours(24);

/// Should this pending delivery be HELD for confirmation instead of driven?
/// Returns `Some(reason)` when the mail is stale — older than the horizon, or
/// addressed BEFORE the target session record existed (a prior incarnation of a
/// reused durable id) — else `None` (drive it normally). Timestamps that fail to
/// parse are treated as NOT stale (fail-open toward delivery: a legible-but-odd
/// timestamp must never silently swallow real mail).
fn stale_delivery_reason(
    conn: &Connection,
    delivery_created_at: &str,
    session: &str,
) -> Result<Option<&'static str>> {
    let Ok(delivered) = chrono::DateTime::parse_from_rfc3339(delivery_created_at) else {
        return Ok(None);
    };
    let now = chrono::Utc::now();
    if now.signed_duration_since(delivered.with_timezone(&chrono::Utc)) > STALE_DELIVERY_HORIZON {
        return Ok(Some("older than the 24h delivery horizon"));
    }
    // Predating the target session's creation: mail addressed before this record
    // existed belongs to a prior incarnation of a reused id, not to this session.
    let session_created: Option<String> = conn
        .query_row(
            "SELECT created_at FROM code_sessions WHERE elanus_session=?1",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sc) = session_created {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&sc) {
            if delivered < created {
                return Ok(Some("addressed before this session was created"));
            }
        }
    }
    Ok(None)
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
    // A small row struct (vs a 5-tuple) keeps the delivery's sender/payload named.
    struct PendingDelivery {
        id: i64,
        etype: String,
        corr: Option<String>,
        payload: Option<String>,
        sender: Option<String>,
        created_at: String,
    }
    let pending: Vec<PendingDelivery> = {
        let mut stmt = conn.prepare(
            "SELECT id, type, correlation_id, payload, sender, created_at FROM events
             WHERE state='pending' AND type LIKE 'in/agent/%'
             ORDER BY priority DESC, id ASC LIMIT 100",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok(PendingDelivery {
                    id: r.get(0)?,
                    etype: r.get(1)?,
                    corr: r.get(2)?,
                    payload: r.get(3)?,
                    sender: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    for PendingDelivery {
        id,
        etype,
        corr,
        payload,
        sender,
        created_at,
    } in pending
    {
        if code.claimed.contains(&id) {
            continue; // already handed to a worker this process; row not yet settled
        }
        let Some((session, _noun)) = crate::codeagent::recognize_delivery(root, &etype) else {
            continue; // not addressed to a known coding session — leave for dispatch_pending
        };
        // Delivery scoping (docs/handoffs/bus-resilience.md M3, report (b)). The
        // ledger scan has no time bound: an old `pending` in/agent/* row fires the
        // instant its target session id resolves again — a `--resume` (or a boot
        // re-pend) of a reused durable session id drives DAYS-old mail into what
        // the human experiences as a fresh session. `recognize_delivery` already
        // pins the address to the exact session id; the missing guard is age. Mail
        // older than the horizon, or addressed BEFORE this session record existed
        // (a prior incarnation of a reused id), is HELD — surfaced in the session's
        // inbox for confirmation, never silently driven. A held row leaves the
        // pending scan (state='held') so it neither re-fires nor drips; the human
        // re-delivers it if they still want it.
        if let Some(reason) = stale_delivery_reason(conn, &created_at, &session)? {
            conn.execute(
                "UPDATE events SET state='held', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id=?1 AND state='pending'",
                [id],
            )?;
            trace::write(
                root,
                "obs/agent/code/delivery/held",
                &trace::Ids {
                    event_id: Some(id),
                    correlation_id: corr.clone(),
                    ..Default::default()
                },
                json!({ "session": session, "reason": reason, "created_at": created_at }),
            );
            continue;
        }
        let pv: Value = payload
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        // Idempotency (M4-A): a delivery whose key was already processed FOR THIS
        // SESSION is the at-least-once duplicate (a crash mid-resume re-pends the
        // same row). Skip the resume and settle it `done` — a clean no-op — so the
        // replay does NOT drive a second turn. The key is namespaced by the target
        // session (docs/security.md) so an attacker's pre-claimed key for one
        // session cannot suppress a victim's delivery to a different one. Checked
        // BEFORE the durable claim so a replayed row (re-pended, same id) is
        // recognized even across a restart.
        let key = crate::codeagent::idempotency_key(&pv, id);
        if crate::codesession::delivery_key_seen(root, &key, &session) {
            conn.execute(
                "UPDATE events SET state='done', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id=?1 AND state='pending'",
                [id],
            )?;
            trace::write(
                root,
                "obs/agent/code/delivery/duplicate",
                &trace::Ids {
                    event_id: Some(id),
                    correlation_id: corr.clone(),
                    ..Default::default()
                },
                json!({ "session": session, "idempotency_key": key, "reason": "already processed" }),
            );
            continue;
        }
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
                &trace::Ids {
                    event_id: Some(id),
                    correlation_id: corr.clone(),
                    ..Default::default()
                },
                json!({ "session": session, "reason": "no prompt/text in payload" }),
            );
            continue;
        };
        // Record the idempotency key DURABLY as part of claiming (M4-A). This is
        // the durable guard the at-least-once replay is checked against above; the
        // race-free INSERT also means two concurrent claims of the same key cannot
        // both proceed. If the key was just taken by another claimant, this row is
        // the loser of the race — settle it as a no-op and move on.
        match crate::codesession::claim_delivery_key(root, &key, &session, id) {
            Ok(true) => {} // first claimant — proceed to drive
            Ok(false) => {
                conn.execute(
                    "UPDATE events SET state='done', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE id=?1 AND state='pending'",
                    [id],
                )?;
                trace::write(
                    root,
                    "obs/agent/code/delivery/duplicate",
                    &trace::Ids {
                        event_id: Some(id),
                        correlation_id: corr.clone(),
                        ..Default::default()
                    },
                    json!({ "session": session, "idempotency_key": key, "reason": "key claimed concurrently" }),
                );
                continue;
            }
            Err(e) => {
                // Couldn't record the key — don't drive (we'd risk a double-act on
                // replay). Leave the row pending; the next tick retries.
                eprintln!("[daemon] recording delivery key for event {id} failed: {e:#}");
                continue;
            }
        }
        // Who to route the completion back to (M4-A): the explicit reply_to, else
        // the broker-verified sender. None for a plain owner/kernel delivery (the
        // M2-B behavior, unchanged — worker resumes, no routing).
        let requester =
            crate::codeagent::delivery_requester(root, &pv, sender.as_deref(), corr.as_deref());
        // Claim it durably BEFORE hand-off: a restart mid-resume re-pends a
        // `running` event and replays it (at-least-once). The in-memory guard
        // stops a same-process re-claim while the worker drains it. (The durable
        // idempotency key above is what makes that replay a no-op, not a re-drive.)
        conn.execute(
            "UPDATE events SET state='running' WHERE id=?1 AND state='pending'",
            [id],
        )?;
        code.claimed.insert(id);
        trace::write(
            root,
            "obs/agent/code/delivery/accepted",
            &trace::Ids {
                event_id: Some(id),
                correlation_id: corr.clone(),
                ..Default::default()
            },
            json!({
                "session": session,
                "type": etype,
                "idempotency_key": key,
                "reply_to": requester.as_ref().map(|r| r.reply_to.clone()),
            }),
        );
        enqueue_code_job(
            root,
            code,
            &session,
            CodeJob {
                event_id: id,
                correlation: corr,
                message,
                requester,
            },
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
                code.workers
                    .insert(session.to_string(), CodeWorker { tx, inflight: 0 });
            }
            Err(e) => {
                eprintln!("[daemon] code worker spawn for {session} failed: {e}");
                // Couldn't spawn — report a synthetic failure so the event settles
                // (it stays `running` otherwise until a restart replays it). The
                // failure still routes to the requester so a planner's loop is not
                // left hanging on a worker that never started (M4-A: a worker
                // failure reaches the planner).
                let detail = format!("worker spawn failed: {e}");
                let _ = code.done_tx.send(CodeDone {
                    session: session.to_string(),
                    event_id,
                    correlation: job.correlation,
                    success: None,
                    detail,
                    requester: job.requester,
                    // No stream ran; route_completion builds an honest line from detail.
                    final_text: None,
                    file_changes: Vec::new(),
                });
                return;
            }
        }
    }
    if let Some(w) = code.workers.get_mut(session) {
        let corr = job.correlation.clone();
        let requester = job.requester.clone();
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
                requester,
                final_text: None,
                file_changes: Vec::new(),
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
        // The routed completion carries the worker's VERBATIM final text + the
        // files it wrote (M4-A follow-on), harvested by resume_capture from the
        // tool's own stream — NOT a generated summary. `detail` stays a terse
        // diagnostic (exit_code / error) for the delivery/complete obs only.
        let (success, detail, final_text, file_changes) =
            match crate::codeagent::resume_capture(&root, &session, &job.message) {
                Ok(outcome) => (
                    Some(outcome.success),
                    format!("exit_code={:?}", outcome.exit_code),
                    outcome.final_text,
                    outcome.file_changes,
                ),
                Err(e) => {
                    // The resume primitive itself errored (missing record / spawn /
                    // credential): there was no stream to harvest. No final_text —
                    // route_completion builds an honest factual line from `detail`.
                    (None, format!("{e:#}"), None, Vec::new())
                }
            };
        // If the receiver is gone (daemon shutting down), the event stays
        // `running` and replays on the next start — at-least-once holds.
        let _ = done_tx.send(CodeDone {
            session: session.clone(),
            event_id: job.event_id,
            correlation: job.correlation,
            success,
            detail,
            requester: job.requester,
            final_text,
            file_changes,
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

    // Handlers call back into `lanius`; make sure this binary wins on PATH.
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
            "lanius-dispatch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    // storage-hardening M3 acceptance: the exact `expire_deadlines` SELECT must
    // ride `idx_events_type_deadline` (a SEARCH), never full-scan `events` (SCAN
    // e). Prepares the production query const and asserts the plan — guarding
    // against future query drift or an accidentally-dropped index reintroducing
    // the O(ledger-age) scan the research probe measured (9.3ms @ 200k rows).
    #[test]
    fn expire_deadlines_uses_type_deadline_index() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {EXPIRE_DEADLINES_SELECT}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(["human", "agent"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let joined = plan.join("\n");
        assert!(
            joined.contains("idx_events_type_deadline"),
            "expire_deadlines must ride idx_events_type_deadline; plan was:\n{joined}"
        );
        // The outer scan of `events e` must be a SEARCH via the index, not a SCAN.
        assert!(
            !joined.contains("SCAN e "),
            "expire_deadlines must not full-scan events e; plan was:\n{joined}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
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
                room: None,
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
            .query_row("SELECT state FROM events WHERE id=?1", [drivable], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(st, "running", "recognized delivery is claimed (running)");
        assert!(code.claimed.contains(&drivable));
        let job = rx.try_recv().expect("a job was enqueued for the session");
        assert_eq!(job.event_id, drivable);
        assert_eq!(job.message, "hello there");

        // The unknown-conv event is untouched — left pending for dispatch_pending
        // (which will mark it done as a no-consumer event), never resumed.
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [unknown], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            st, "pending",
            "unrecognized in/agent event is left for dispatch"
        );

        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// docs/handoffs/bus-resilience.md M3, report (b): the ledger scan had NO
    /// time bound, so an old `pending` in/agent/* row fired the instant its target
    /// session id resolved again — a resume (or boot re-pend) of a reused durable
    /// id drove DAYS-old mail into a fresh-feeling session. The fix HOLDS a
    /// stale delivery (older than the 24h horizon, or predating the session
    /// record) instead of driving it, while a fresh delivery to the same session
    /// still drives normally. This test FAILS on the old code (the stale row would
    /// be claimed `running` and enqueued) and PASSES on the new.
    #[test]
    fn drive_holds_stale_delivery_and_drives_fresh() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-aaaa0001".into(),
                native_session: "thread-1".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();

        // Two deliveries to the SAME (recorded, drivable) session: one addressed
        // days ago (the stale "previous session" prompt), one just now.
        let stale = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "run the OLD task" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-aaaa0001")
            },
        )
        .unwrap();
        // Backdate it well past the 24h horizon.
        let old_ts = (chrono::Utc::now() - chrono::Duration::hours(48))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        conn.execute(
            "UPDATE events SET created_at=?1 WHERE id=?2",
            params![old_ts, stale],
        )
        .unwrap();
        let fresh = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "the task I just typed" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-aaaa0001")
            },
        )
        .unwrap();

        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-aaaa0001".into(), CodeWorker { tx, inflight: 0 });

        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        // The stale row is HELD, not driven: surfaced for confirmation, never a
        // prompt the user didn't just type.
        let stale_state: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [stale], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stale_state, "held", "stale mail is held, not driven");
        assert!(
            !code.claimed.contains(&stale),
            "stale mail is never claimed for a resume"
        );

        // The fresh row drives normally.
        let fresh_state: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [fresh], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fresh_state, "running", "fresh mail is claimed and driven");
        assert!(code.claimed.contains(&fresh));

        // Exactly the fresh delivery reached the worker — the stale one did not.
        let job = rx.try_recv().expect("the fresh delivery was enqueued");
        assert_eq!(job.event_id, fresh);
        assert_eq!(job.message, "the task I just typed");
        assert!(
            rx.try_recv().is_err(),
            "the stale delivery was NOT enqueued"
        );

        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// The "predates the session record" arm (docs/handoffs/bus-resilience.md M3):
    /// mail addressed BEFORE this session id's record existed belongs to a prior
    /// incarnation of a reused id, so it is held even when it is younger than the
    /// 24h horizon.
    #[test]
    fn drive_holds_delivery_predating_session_creation() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-bbbb0002".into(),
                native_session: "thread-2".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        // Move the session record's creation to now+10min so the just-emitted
        // delivery (now) predates it — the reused-id signature, without tripping
        // the age horizon.
        let future = (chrono::Utc::now() + chrono::Duration::minutes(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        conn.execute(
            "UPDATE code_sessions SET created_at=?1 WHERE elanus_session='code-bbbb0002'",
            params![future],
        )
        .unwrap();

        let mail = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "stale for a rebound id" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-bbbb0002")
            },
        )
        .unwrap();

        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-bbbb0002".into(), CodeWorker { tx, inflight: 0 });
        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        let state: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [mail], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "held", "mail predating the record is held");
        assert!(rx.try_recv().is_err(), "nothing enqueued");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M3/M5 (docs/handoffs/timers.md): a one-shot schedule → fire → wake on the
    /// ledger. A due `scheduled_events` row (as `lanius schedule` / the tool
    /// insert) fires exactly one `in/agent/main` event carrying {prompt,session};
    /// that event is a plain pending delivery — drive_code_deliveries leaves it
    /// for the chat exec handler (wonky bit 4: `main` is not a coding session);
    /// a future row waits; the fired flag + `sched:<id>` idempotency make a
    /// restart (reopen) + re-sweep a no-op. The wake-turn (LLM) is out of scope
    /// here — this is the ledger half of the sanctioned split.
    #[test]
    fn one_shot_schedule_fires_wakes_and_is_idempotent() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let main_mb = crate::topic::agent_mailbox("main");

        // Two rows exactly as the CLI/tool insert them: one due (past), one future.
        let payload = json!({ "prompt": "post here", "session": "conv-7" }).to_string();
        conn.execute(
            "INSERT INTO scheduled_events(fire_at, emit_type, payload, created_by, fired)
             VALUES ('2000-01-01T00:00:00.000Z', ?1, ?2, 'cli', 0)",
            params![main_mb, payload],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scheduled_events(fire_at, emit_type, payload, created_by, fired)
             VALUES ('2999-01-01T00:00:00.000Z', ?1, '{}', 'cli', 0)",
            params![main_mb],
        )
        .unwrap();

        tick_schedules(&root, &conn).unwrap();

        // Exactly one in/agent/main event, carrying the wake payload.
        let (n, pl): (i64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MAX(payload) FROM events WHERE type = ?1",
                [&main_mb],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "the due schedule fires exactly one in/agent/main");
        let pl: Value = serde_json::from_str(&pl.unwrap()).unwrap();
        assert_eq!(pl["prompt"], "post here");
        assert_eq!(
            pl["session"], "conv-7",
            "the wake carries the turn's session"
        );
        // The future row is untouched.
        let unfired: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scheduled_events WHERE fired = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unfired, 1, "the future schedule has not fired");

        // Wonky bit 4: the fired event is NOT a coding-session delivery, so
        // drive_code_deliveries leaves it pending for the chat exec handler.
        let mut code = CodeDrivers::default();
        drive_code_deliveries(&root, &conn, &mut code).unwrap();
        let st: String = conn
            .query_row(
                "SELECT state FROM events WHERE type = ?1",
                [&main_mb],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            st, "pending",
            "a self-wake falls through to dispatch, not code drive"
        );
        assert!(
            code.claimed.is_empty(),
            "no coding session claims a self-wake"
        );

        // Restart durability + idempotency: reopen the db and re-sweep — the
        // fired row does not re-fire (fired flag; sched:<id> key backstops it).
        drop(conn);
        let conn2 = db::open(&root).unwrap();
        db::init_schema(&conn2).unwrap();
        tick_schedules(&root, &conn2).unwrap();
        let n2: i64 = conn2
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type = ?1",
                [&main_mb],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 1, "a fired schedule never re-fires across a restart");

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
                room: None,
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
        assert!(
            rx.try_recv().is_err(),
            "the same delivery is never enqueued twice"
        );
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
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "two same-session resumes never overlap"
        );
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
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [ev])
            .unwrap();
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
                requester: None,
                final_text: Some("ok".into()),
                file_changes: Vec::new(),
            })
            .unwrap();

        settle_code_deliveries(&root, &conn, &mut code).unwrap();

        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "done", "a successful resume settles the delivery done");
        assert!(
            !code.claimed.contains(&ev),
            "settled event leaves the claimed set"
        );
        assert!(
            !code.workers.contains_key("code-cccc0003"),
            "idle worker is retired"
        );

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
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [ev2])
            .unwrap();
        code.claimed.insert(ev2);
        code.done_tx
            .send(CodeDone {
                session: "code-dddd0004".into(),
                event_id: ev2,
                correlation: None,
                success: None,
                detail: "boom".into(),
                requester: None,
                final_text: None,
                file_changes: Vec::new(),
            })
            .unwrap();
        settle_code_deliveries(&root, &conn, &mut code).unwrap();
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev2], |r| r.get(0))
            .unwrap();
        assert_eq!(
            st, "failed",
            "an errored resume settles the delivery failed"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A — requester capture: a delivery whose broker-verified `sender` is a
    /// recorded coding session (a planner) captures that planner's own mailbox as
    /// the job's requester, so the completion can later route back and resume it.
    #[test]
    fn drive_captures_requester_from_sender() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        // Worker W and planner P, both recorded coding sessions.
        for (s, t) in [("code-worker01", "codex"), ("code-plannr01", "claude")] {
            crate::codesession::upsert_record(
                &root,
                &crate::codesession::SessionRecord {
                    elanus_session: s.into(),
                    native_session: "n".into(),
                    tool: t.into(),
                    agent_noun: if t == "codex" { "codex" } else { "claude-code" }.into(),
                    workdir: root.dir.display().to_string(),
                    room: None,
                },
            )
            .unwrap();
        }
        // A delivery to W, stamped (as the broker would) sender = the planner P.
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "do the work" })),
                sender: Some("code-plannr01".into()),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-worker01")
            },
        )
        .unwrap();
        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-worker01".into(), CodeWorker { tx, inflight: 0 });

        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        let job = rx.try_recv().expect("a job was enqueued");
        assert_eq!(job.event_id, ev);
        // The requester is the planner's OWN mailbox — routing the completion there
        // resumes the planner (the loop closing).
        let req = job
            .requester
            .expect("the planner sender is captured as requester");
        assert_eq!(req.reply_to, "in/agent/claude-code/code-plannr01");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A — idempotency: a replayed delivery with the same key (the at-least-once
    /// duplicate a mid-resume crash re-pends) is recognized and settled as a clean
    /// no-op — no second resume is driven. We drive once (recording the key), then
    /// re-pend the same row (as the boot sweep does) and drive again; the second
    /// drive must NOT enqueue a second job.
    #[test]
    fn drive_dedupes_replayed_delivery_no_second_resume() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-worker02".into(),
                native_session: "n".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "once only" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-worker02")
            },
        )
        .unwrap();
        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-worker02".into(), CodeWorker { tx, inflight: 0 });

        // First drive: claims + enqueues exactly once, and records the key.
        drive_code_deliveries(&root, &conn, &mut code).unwrap();
        assert_eq!(rx.try_recv().map(|j| j.event_id).ok(), Some(ev));
        assert!(crate::codesession::delivery_key_seen(
            &root,
            &format!("event:{ev}"),
            "code-worker02"
        ));

        // Simulate the at-least-once replay across a restart: the row re-pends
        // (boot's running->pending sweep) AND a fresh process's in-flight guard is
        // empty (the `claimed` set does not survive a restart).
        conn.execute(
            "UPDATE events SET state='pending', finished_at=NULL WHERE id=?1",
            [ev],
        )
        .unwrap();
        let mut code2 = CodeDrivers::default();
        let (tx2, rx2) = std::sync::mpsc::channel::<CodeJob>();
        code2.workers.insert(
            "code-worker02".into(),
            CodeWorker {
                tx: tx2,
                inflight: 0,
            },
        );

        drive_code_deliveries(&root, &conn, &mut code2).unwrap();

        // The replay is a recognized no-op: NO second job, and the row is settled
        // `done` (not re-driven, not left pending forever).
        assert!(
            rx2.try_recv().is_err(),
            "the duplicate must not drive a second resume"
        );
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev], |r| r.get(0))
            .unwrap();
        assert_eq!(
            st, "done",
            "the replayed duplicate is settled as a clean no-op"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A cross-victim suppression (security): an attacker who pre-claims an
    /// explicit idempotency key K (for their own session A) must NOT silently
    /// suppress a different victim's delivery to a DIFFERENT session B that reuses
    /// K. With a global key space the victim's delivery would be falsely deduped and
    /// never driven; namespacing the key by target session keeps them independent,
    /// so B is driven (claimed + enqueued). This drives through the REAL
    /// `drive_code_deliveries` path, the exact abuse probe.
    #[test]
    fn cross_victim_key_does_not_suppress_a_different_session_delivery() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        // The victim's session B is recorded (drivable).
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-victimbb".into(),
                native_session: "n".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        // The attacker pre-claims key K, but for a DIFFERENT session A (their own).
        // This is what a global key space would let leak into B's namespace.
        assert!(crate::codesession::claim_delivery_key(&root, "K", "code-attackeraa", 1).unwrap());

        // The victim delivery to session B reuses the same explicit key K.
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "victim work", "idempotency_key": "K" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-victimbb")
            },
        )
        .unwrap();
        let mut code = CodeDrivers::default();
        let (tx, rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-victimbb".into(), CodeWorker { tx, inflight: 0 });

        drive_code_deliveries(&root, &conn, &mut code).unwrap();

        // B is DRIVEN, not suppressed: the event is claimed `running` and a job was
        // enqueued for it. (A global key space would have settled it `done` as a
        // bogus duplicate with no job.)
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev], |r| r.get(0))
            .unwrap();
        assert_eq!(
            st, "running",
            "the victim delivery to B must be driven, not suppressed"
        );
        let job = rx.try_recv().expect("the victim delivery must drive a job");
        assert_eq!(job.event_id, ev);
        assert_eq!(job.message, "victim work");
        // And the key is now recorded under B's namespace (independent of A's).
        assert!(crate::codesession::delivery_key_seen(
            &root,
            "K",
            "code-victimbb"
        ));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A reliability residual (Fix 3): a crash in the settle->route gap settles
    /// the worker delivery `done` but never emits the route, and the boot sweep only
    /// re-pends `running` events — so the planner's wake would be lost forever. The
    /// boot `reconcile_lost_routes` recovers it from durable state. This reproduces
    /// the exact crash: a driven worker delivery (key recorded, settled done) whose
    /// requester resolves but with NO routed event — reconcile must emit the route.
    #[test]
    fn reconcile_recovers_a_route_lost_in_the_settle_route_gap() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        // Worker W (recorded) and planner P (recorded), both coding sessions.
        for (s, t, n) in [
            ("code-wrkr0001", "codex", "codex"),
            ("code-plnr0001", "claude", "claude-code"),
        ] {
            crate::codesession::upsert_record(
                &root,
                &crate::codesession::SessionRecord {
                    elanus_session: s.into(),
                    native_session: "n".into(),
                    tool: t.into(),
                    agent_noun: n.into(),
                    workdir: root.dir.display().to_string(),
                    room: None,
                },
            )
            .unwrap();
        }
        // A delivery to W, stamped by the broker as sender = the planner P, threaded
        // by a correlation. This is the durable state a crash leaves behind.
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "do the work" })),
                sender: Some("code-plnr0001".into()),
                correlation: Some("loop-x".into()),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-wrkr0001")
            },
        )
        .unwrap();
        // The crash state: the delivery was DRIVEN (key recorded) and settled `done`
        // by settle, but the route was NEVER emitted (crash in the gap).
        assert!(crate::codesession::claim_delivery_key(
            &root,
            &format!("event:{ev}"),
            "code-wrkr0001",
            ev
        )
        .unwrap());
        conn.execute("UPDATE events SET state='done' WHERE id=?1", [ev])
            .unwrap();
        // Precondition: no completion routed to P yet.
        assert!(!route_already_emitted(&conn, ev, "in/agent/claude-code/code-plnr0001").unwrap());

        // Boot reconciliation recovers the lost wake.
        reconcile_lost_routes(&root, &conn).unwrap();

        // A completion is now routed to P's mailbox, pending, same correlation — the
        // planner will be woken (drive picks it up).
        let (st, corr, payload): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT state, correlation_id, payload FROM events
                 WHERE type='in/agent/claude-code/code-plnr0001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("the lost route is recovered to the planner's mailbox");
        assert_eq!(
            st, "pending",
            "the recovered route is a pending delivery the planner drives"
        );
        assert_eq!(
            corr.as_deref(),
            Some("loop-x"),
            "same correlation threads the loop"
        );
        let pv: Value = serde_json::from_str(&payload.unwrap()).unwrap();
        assert!(pv["prompt"].as_str().unwrap().contains("code-wrkr0001"));
        assert!(pv["idempotency_key"]
            .as_str()
            .unwrap()
            .starts_with("code-complete:"));

        // Idempotent: a second boot does NOT route a duplicate (the guard sees the
        // existing route).
        reconcile_lost_routes(&root, &conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type='in/agent/claude-code/code-plnr0001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "a second boot must not route a duplicate wake");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// reconcile_lost_routes does NOT route a wake for a driven delivery that had no
    /// requester (a plain owner resume) — only deliveries that named a planner are
    /// recovered, so a normal worker resume is unaffected.
    #[test]
    fn reconcile_skips_deliveries_with_no_requester() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-wrkr0002".into(),
                native_session: "n".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        // An owner delivery (no reply_to, sender owner) — no planner waiting.
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "x" })),
                sender: Some("owner".into()),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-wrkr0002")
            },
        )
        .unwrap();
        assert!(crate::codesession::claim_delivery_key(
            &root,
            &format!("event:{ev}"),
            "code-wrkr0002",
            ev
        )
        .unwrap());
        conn.execute("UPDATE events SET state='done' WHERE id=?1", [ev])
            .unwrap();

        reconcile_lost_routes(&root, &conn).unwrap();

        // Nothing was routed (no in/agent/* event other than the original worker
        // delivery exists with a cause pointing at it).
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE cause_id = ?1",
                [ev],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "an owner delivery with no requester routes nothing");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A — completion routing: a settled delivery whose job named a requester
    /// publishes a completion to that requester's mailbox carrying the SAME
    /// correlation, so a planner is resumed. We feed a CodeDone with a requester and
    /// assert the routed in/agent event lands as a pending delivery the planner's
    /// drive would pick up — that is the loop closing.
    #[test]
    fn settle_routes_completion_to_requester_mailbox() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        // The worker session must be recorded so the routed completion can name its
        // obs subtree.
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-worker03".into(),
                native_session: "n".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        let ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "x" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-worker03")
            },
        )
        .unwrap();
        conn.execute("UPDATE events SET state='running' WHERE id=?1", [ev])
            .unwrap();
        let mut code = CodeDrivers::default();
        code.claimed.insert(ev);
        let (tx, _rx) = std::sync::mpsc::channel::<CodeJob>();
        code.workers
            .insert("code-worker03".into(), CodeWorker { tx, inflight: 1 });
        code.done_tx
            .send(CodeDone {
                session: "code-worker03".into(),
                event_id: ev,
                correlation: Some("loop-corr-1".into()),
                success: Some(true),
                detail: "exit_code=Some(0)".into(),
                requester: Some(crate::codeagent::DeliveryRequester {
                    reply_to: "in/agent/claude-code/code-plannr03".into(),
                }),
                // The worker's VERBATIM answer + the file it wrote — carried as-is
                // to the routed completion (no summary).
                final_text: Some("ALPHA is the answer".into()),
                file_changes: vec!["src/answer.rs".into()],
            })
            .unwrap();

        settle_code_deliveries(&root, &conn, &mut code).unwrap();

        // The original delivery settled done.
        let st: String = conn
            .query_row("SELECT state FROM events WHERE id=?1", [ev], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "done");
        // A completion was routed to the planner's mailbox, pending, threaded by the
        // SAME correlation — exactly what drive_code_deliveries resumes the planner
        // on (the loop closing).
        let (routed_state, routed_corr, routed_payload): (String, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT state, correlation_id, payload FROM events
                 WHERE type='in/agent/claude-code/code-plannr03'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("a completion was routed to the planner's mailbox");
        assert_eq!(
            routed_state, "pending",
            "the routed completion is a pending delivery"
        );
        assert_eq!(
            routed_corr.as_deref(),
            Some("loop-corr-1"),
            "same correlation threads the loop"
        );
        let pv: Value = serde_json::from_str(&routed_payload.unwrap()).unwrap();
        // The payload carries a prompt (resumes a coding-session planner), the
        // success flag, and an idempotency key so a replayed completion dedupes.
        assert!(pv["prompt"].as_str().unwrap().contains("code-worker03"));
        assert_eq!(pv["failed"], false);
        assert!(pv["idempotency_key"]
            .as_str()
            .unwrap()
            .starts_with("code-complete:"));
        // M4-A follow-on: the completion carries the worker's VERBATIM final text,
        // the file paths it changed, and the obs pointer — NOT a generated summary.
        assert_eq!(pv["final_text"], "ALPHA is the answer");
        assert!(pv["prompt"]
            .as_str()
            .unwrap()
            .contains("ALPHA is the answer"));
        assert_eq!(pv["file_changes"][0], "src/answer.rs");
        assert!(pv["prompt"].as_str().unwrap().contains("src/answer.rs"));
        assert!(
            pv["worker_obs"].as_str().unwrap().contains("code-worker03"),
            "the completion keeps the obs pointer to the worker's full conversation"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A follow-on — the no-final-text fallback: a worker that produced no final
    /// message routes a MINIMAL FACTUAL line (built from the diagnostic detail), NOT
    /// a fabricated summary of what it "did". The obs pointer + empty file_changes
    /// still ride along.
    #[test]
    fn route_completion_falls_back_factually_when_no_final_text() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        crate::codesession::upsert_record(
            &root,
            &crate::codesession::SessionRecord {
                elanus_session: "code-silent01".into(),
                native_session: "n".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
        // A worker event id to cause the route off of.
        let worker_ev = events::emit(
            &root,
            &conn,
            EmitOpts {
                payload: Some(json!({ "prompt": "x" })),
                pre_announced: true,
                ..EmitOpts::new("in/agent/codex/code-silent01")
            },
        )
        .unwrap();
        route_completion(
            &root,
            &conn,
            worker_ev,
            "code-silent01",
            "in/agent/claude-code/code-plannr04",
            false,
            None, // the worker produced NO final text
            &[],  // and changed no files
            "exit_code=Some(0)",
            Some("corr-silent"),
        );
        let payload: String = conn
            .query_row(
                "SELECT payload FROM events WHERE type='in/agent/claude-code/code-plannr04'",
                [],
                |r| r.get(0),
            )
            .expect("a completion was routed even with no final text");
        let pv: Value = serde_json::from_str(&payload).unwrap();
        let ft = pv["final_text"].as_str().unwrap();
        // A minimal factual fallback — names the worker + the diagnostic; it does NOT
        // claim to describe what the worker accomplished.
        assert!(
            ft.contains("no final message"),
            "factual fallback, not a summary: {ft}"
        );
        assert!(
            ft.contains("exit_code=Some(0)"),
            "the fallback carries the honest diagnostic"
        );
        assert!(pv["file_changes"].as_array().unwrap().is_empty());
        assert!(pv["worker_obs"].as_str().unwrap().contains("code-silent01"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// Record a recorded coding session (so `mailbox_for_actor` can address it).
    fn record_code_session(root: &Root, sess: &str, noun: &str) {
        crate::codesession::upsert_record(
            root,
            &crate::codesession::SessionRecord {
                elanus_session: sess.into(),
                native_session: "n".into(),
                tool: if noun == "codex" { "codex" } else { "claude" }.into(),
                agent_noun: noun.into(),
                workdir: root.dir.display().to_string(),
                room: None,
            },
        )
        .unwrap();
    }

    /// cross-harness-death M2 — the reaper's core: a detached spawn edge whose
    /// wrapper pid is dead and which never reported synthesizes a `{failed:true}`
    /// completion-mail to the spawner and settles the edge. A DEAD pid is required —
    /// a very high pid is guaranteed `ESRCH` (no such process).
    #[test]
    fn reaper_mails_failure_for_dead_unreported_spawn_worker() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        record_code_session(&root, "code-plannr09", "claude-code");
        // A dead wrapper pid; the worker never mailed (unsettled edge).
        crate::codesession::record_spawn_edge(
            &root,
            "code-wrkdead1",
            "code-plannr09",
            Some("corr-dead"),
            i32::MAX,
        )
        .unwrap();

        reap_dead_spawn_edges(&root, &conn).unwrap();

        // Exactly one failure-mail to the planner's mailbox, structured failed:true.
        let (payload, corr): (String, String) = conn
            .query_row(
                "SELECT payload, COALESCE(correlation_id,'') FROM events
                   WHERE type='in/agent/claude-code/code-plannr09'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the reaper mailed the spawner");
        let pv: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(pv["failed"], json!(true), "structured failed:true");
        assert_eq!(pv["worker"], json!("code-wrkdead1"));
        assert!(
            pv["exit_code"].is_null(),
            "a killed worker has no exit code"
        );
        assert_eq!(
            corr, "corr-dead",
            "the completion threads on the spawn corr"
        );
        assert!(pv["prompt"]
            .as_str()
            .unwrap()
            .contains("terminated without reporting"));
        // The edge is settled.
        let settled: Option<String> = conn
            .query_row(
                "SELECT settled_at FROM code_spawn_edges WHERE worker_session='code-wrkdead1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(settled.is_some(), "the edge is settled after the reap");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// A LIVE worker (its wrapper pid still alive) is never reaped — the reaper only
    /// ever acts on a dead pid, so a running worker's own completion path owns the
    /// claim.
    #[test]
    fn reaper_leaves_a_live_spawn_worker_alone() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        record_code_session(&root, "code-plannr10", "claude-code");
        crate::codesession::record_spawn_edge(
            &root,
            "code-wrklive1",
            "code-plannr10",
            Some("corr-live"),
            std::process::id() as i32, // this test process — very much alive
        )
        .unwrap();

        reap_dead_spawn_edges(&root, &conn).unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type='in/agent/claude-code/code-plannr10'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "a live worker is not reaped");
        let settled: Option<String> = conn
            .query_row(
                "SELECT settled_at FROM code_spawn_edges WHERE worker_session='code-wrklive1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(settled.is_none(), "a live worker's edge stays unsettled");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// Exactly-once under the slow-worker/reaper race (cross-harness-death M2
    /// acceptance): if the WORKER'S own completion already claimed+settled the edge,
    /// the reaper mails nothing — no double-mail. And running the reaper twice over a
    /// dead worker mails exactly once (the second pass finds a settled edge).
    #[test]
    fn reaper_is_idempotent_and_never_double_mails() {
        let root = tmp_root();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        record_code_session(&root, "code-plannr11", "claude-code");

        // (a) worker won the claim first → reaper is a no-op even for a dead pid.
        crate::codesession::record_spawn_edge(
            &root,
            "code-wrkwon01",
            "code-plannr11",
            Some("corr-won"),
            i32::MAX,
        )
        .unwrap();
        assert_eq!(
            crate::codesession::claim_spawn_edge(&root, "code-wrkwon01"),
            crate::codesession::SettleClaim::Claimed,
            "the worker wins the claim"
        );
        reap_dead_spawn_edges(&root, &conn).unwrap();

        // (b) a genuinely dead+unreported worker → reaper mails; a SECOND reap pass
        // finds the edge settled and mails nothing more.
        crate::codesession::record_spawn_edge(
            &root,
            "code-wrkdead2",
            "code-plannr11",
            Some("corr-dead2"),
            i32::MAX,
        )
        .unwrap();
        reap_dead_spawn_edges(&root, &conn).unwrap();
        reap_dead_spawn_edges(&root, &conn).unwrap();

        // The planner's mailbox holds EXACTLY ONE completion — for wrkdead2 only
        // (wrkwon01 was claimed by the worker, so the reaper stayed silent).
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE type='in/agent/claude-code/code-plannr11'")
            .unwrap();
        let workers: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .map(|p| {
                serde_json::from_str::<Value>(&p).unwrap()["worker"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            workers,
            vec!["code-wrkdead2".to_string()],
            "exactly one completion, and only for the unreported worker"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }
}

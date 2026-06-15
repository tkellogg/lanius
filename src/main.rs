mod broker;
mod bus;
mod buscli;
mod context;
mod db;
mod dispatcher;
mod dotenv;
mod events;
mod exec;
mod hooks;
mod human;
mod initcmd;
mod kit;
mod manifest;
mod mcp;
mod models;
mod packages;
mod paths;
mod profile;
mod profilecli;
mod recorder;
mod render;
mod resident;
mod sandbox;
mod secrets;
mod topic;
mod trace;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "elanus", version, about = "elanus: a minimal event-driven agent harness")]
struct Cli {
    /// Harness root (default: $HARNESS_ROOT, or walk up from cwd to find harness.db)
    #[arg(short = 'C', long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a harness root (db, trace log, default profile, stock skills)
    Init {
        dir: Option<PathBuf>,
        /// Kit(s) to install: packages linked (or --copy vendored) + granted,
        /// profiles copied if missing, README printed. A value containing '/'
        /// is a path; a bare name resolves against <root>/kits (seeded with
        /// the stock kits), ~/.elanus/kits, $ELANUS_KIT_PATH, then the repo
        /// kits/ (dev builds). Repeatable.
        #[arg(long)]
        kit: Vec<String>,
        /// Vendor kit packages into the root's packages/ instead of linking
        /// the kit's dir onto the package path.
        #[arg(long)]
        copy: bool,
    },
    /// Print the effective context-pipeline chain for a profile
    /// (docs/context.md): the built-in seed, then every package stage in
    /// deterministic order (order, package, stage)
    Stages {
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Kits: starter packs of packages + profiles (add / list / show)
    Kit {
        #[command(subcommand)]
        cmd: KitCmd,
    },
    /// Ask the configured provider for its model list (GET /v1/models)
    Models {
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Profiles: agent identities (list / get / set / new)
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Run the dispatcher: poll events, fork handlers, record exits
    Daemon {
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Emit an event — the universal entry point
    Emit {
        r#type: String,
        #[arg(long)]
        payload: Option<String>,
        #[arg(long, default_value_t = 0)]
        priority: i64,
        #[arg(long)]
        correlation: Option<String>,
        /// ISO8601; for asks (in/human/<owner>): when the default fires
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long)]
        default_action: Option<String>,
        #[arg(long)]
        idempotency: Option<String>,
        #[arg(long)]
        cause: Option<i64>,
    },
    /// Append a line to the flight recorder (for handlers in any language)
    Trace {
        kind: String,
        #[arg(long)]
        payload: Option<String>,
    },
    /// Run an agent turn; chat is exec with a session ID
    Exec {
        /// Prompt text, or '-' to read stdin
        prompt: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "default")]
        profile: String,
        /// Resume a suspended session with the human's answer
        #[arg(long)]
        resume: Option<String>,
    },
    /// Backend for exec-as-handler; reads the event envelope on stdin
    #[command(hide = true)]
    HandleExec,
    /// Print the assembled context for a profile (inspectable with | less)
    Render {
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long, default_value = "render-preview")]
        session: String,
    },
    /// List packages: what's discovered, what's requested, what's granted
    Packages {
        /// Machine-readable: one JSON object per package, including each
        /// pending/approved grant row (the UI's pending-review queue)
        #[arg(long)]
        json: bool,
    },
    /// Approve a package's requested capabilities (prints each one)
    Approve {
        name: String,
        /// Identity trail for the ledger's decided_by (e.g. "ui")
        #[arg(long, default_value = "cli")]
        by: String,
    },
    /// Revoke a package's approved capabilities
    Revoke {
        name: String,
        #[arg(long, default_value = "cli")]
        by: String,
        /// Force-revoke a protected (stdlib) package the product depends on
        #[arg(long)]
        force: bool,
    },
    /// What's blocked on you?
    Inbox,
    /// Answer an ask by event id
    Answer { ask_id: i64, text: String },
    /// Sugar over emit: an ask (in/human/<owner>) with correlation + deadline + default
    Ask {
        question: String,
        /// Comma-separated options
        #[arg(long)]
        options: Option<String>,
        #[arg(long)]
        deadline_minutes: Option<i64>,
        #[arg(long)]
        default: Option<String>,
    },
    /// Recent events (debug view)
    Events {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// The live bus: publish/subscribe via the daemon's MQTT listener
    Bus {
        #[command(subcommand)]
        cmd: BusCmd,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// All profiles, one JSON object per line
    List,
    /// One profile: parsed summary + raw TOML, as JSON
    Get { name: String },
    /// Set dotted keys, comments preserved, validated before writing:
    /// elanus profile set default agent=kestrel model.max_turns=12
    Set {
        name: String,
        /// key=value pairs; values parse as TOML when they can
        pairs: Vec<String>,
    },
    /// Scaffold a profile (agent noun defaults to the name; blocks seeded
    /// from the default profile)
    New {
        name: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Check a candidate profile.toml would load (exits non-zero with the reason
    /// if not) — the web UI's raw editor validates before it saves.
    Validate {
        /// path to the candidate profile.toml file
        path: String,
    },
}

#[derive(Subcommand)]
enum KitCmd {
    /// Install a kit into this root: packages linked onto the package path
    /// (or --copy vendored), profiles copied if missing, packages granted
    /// with provenance kit:<name>, README printed
    Add {
        /// Kit name (resolved via <root>/kits, ~/.elanus/kits,
        /// $ELANUS_KIT_PATH, <repo>/kits) or a path
        kit: String,
        /// Vendor packages into the root's packages/ instead of linking
        #[arg(long)]
        copy: bool,
        /// STAGE only: files land and requests register, but every grant
        /// stays pending — commit with `elanus approve <package>` (the
        /// web UI / agent-staging path)
        #[arg(long)]
        pending: bool,
    },
    /// Kits installable right now, in resolution order (first hit wins)
    List {
        /// One JSON object per kit
        #[arg(long)]
        json: bool,
    },
    /// Print a kit's README without installing it
    Show { kit: String },
    /// Remove a linked kit's packages dir from the package path (grants
    /// stay in the ledger, inert; revoke per package to retire them)
    Unlink { kit: String },
}

#[derive(Subcommand)]
enum BusCmd {
    /// Publish once; QoS 1 (default) waits for the broker to accept
    Pub {
        topic: String,
        payload: Option<String>,
        #[arg(long, default_value_t = 1)]
        qos: u8,
        /// Retain: late subscribers get the last value (empty payload clears)
        #[arg(long)]
        retain: bool,
        /// Envelope correlation (flow id) — rides the el-correlation user
        /// property; the broker materializes it on in/# and signal/# topics
        #[arg(long)]
        correlation: Option<String>,
    },
    /// Subscribe and print one JSON line per message
    Sub {
        filter: String,
        /// Exit successfully after this many messages
        #[arg(long)]
        count: Option<u64>,
        /// Give up after this many seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Register as a resident blocking hook (filter must live under
        /// obs/harness/hookreq/<point>/...; needs an approved blocking grant
        /// and the actor token environment). Each request prints its JSON on
        /// stdout; one stdin line answers it: allow | deny[:reason] | a JSON
        /// object (rewritten subject).
        #[arg(long)]
        blocking: bool,
        /// Chain position (lower runs earlier)
        #[arg(long, default_value_t = 50)]
        order: u32,
        /// Broker-side wait per invocation before on-timeout applies
        #[arg(long, default_value_t = 500)]
        timeout_ms: u64,
        /// allow|deny when this hook doesn't answer in time (fail-open vs
        /// fail-closed is the registrant's security declaration)
        #[arg(long, default_value = "deny")]
        on_timeout: String,
        /// Informational user property (the filter is authoritative)
        #[arg(long)]
        phase: Option<String>,
        /// Informational user property (the filter is authoritative)
        #[arg(long)]
        point: Option<String>,
    },
}

fn main() {
    // Die quietly on EPIPE like a normal Unix tool (`elanus inbox | grep -q`).
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    // Secrets fallback: cwd .env first (dev convenience), then the root's
    // .env once resolved. Real environment always wins over both.
    dotenv::load(std::path::Path::new(".env"));
    match cli.cmd {
        Cmd::Init { ref dir, ref kit, copy } => {
            // Same resolution order as every other command: explicit arg >
            // HARNESS_ROOT > ~/.elanus/root. Init once targeted cwd while
            // the env var pointed elsewhere, littering template roots into
            // repos and test directories.
            let dir = match dir.clone().or_else(|| std::env::var("HARNESS_ROOT").ok().map(PathBuf::from)) {
                Some(d) => d,
                None => paths::default_root()?,
            };
            return initcmd::init(dir, kit.clone(), copy);
        }
        _ => {}
    }
    let root = paths::resolve(cli.root)?;
    dotenv::load(&root.dir.join(".env"));
    match cli.cmd {
        Cmd::Init { .. } => unreachable!(),
        Cmd::Daemon { interval_ms } => dispatcher::run(&root, interval_ms)?,
        Cmd::Emit {
            r#type,
            payload,
            priority,
            correlation,
            deadline,
            default_action,
            idempotency,
            cause,
        } => {
            let conn = open(&root)?;
            let id = events::emit(
                &root,
                &conn,
                events::EmitOpts {
                    payload: parse_json_opt(payload.as_deref())?,
                    priority,
                    correlation,
                    deadline,
                    default_action: parse_json_opt(default_action.as_deref())?,
                    idempotency,
                    cause,
                    ..events::EmitOpts::new(&r#type)
                },
            )?;
            println!("{id}");
        }
        Cmd::Trace { kind, payload } => {
            let ids = trace::Ids::from_env();
            trace::write(
                &root,
                &kind,
                &ids,
                parse_json_opt(payload.as_deref())?.unwrap_or(Value::Null),
            );
        }
        Cmd::Exec { prompt, session, profile, resume } => {
            let result = exec::run(&root, exec::ExecOpts { session, profile, prompt, resume, event: None });
            if let Ok(conn) = open(&root) {
                exec::release_own_leases(&conn);
            }
            result?;
        }
        Cmd::HandleExec => exec::handle_exec(&root)?,
        Cmd::Render { profile, session } => {
            let conn = open(&root)?;
            println!("{}", render::render(&root, &conn, &profile, &session)?);
        }
        Cmd::Packages { json } => {
            let conn = open(&root)?;
            packages::sync(&root, &conn)?;
            for p in packages::discover(&root)? {
                if json {
                    let hash = p.manifest.as_ref().map(|lm| lm.hash.clone()).unwrap_or_default();
                    let grants: Vec<Value> = if hash.is_empty() {
                        vec![]
                    } else {
                        let mut stmt = conn.prepare(
                            "SELECT kind, value, state, decided_by FROM grants
                             WHERE package=?1 AND manifest_hash=?2 ORDER BY kind, value",
                        )?;
                        let rows = stmt
                            .query_map(rusqlite::params![p.name, hash], |r| {
                                Ok(serde_json::json!({
                                    "kind": r.get::<_, String>(0)?,
                                    "value": r.get::<_, String>(1)?,
                                    "state": r.get::<_, String>(2)?,
                                    "decided_by": r.get::<_, Option<String>>(3)?,
                                }))
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    };
                    println!(
                        "{}",
                        serde_json::json!({
                            "name": p.name,
                            "dir": p.dir,
                            "mode": p.manifest.as_ref()
                                .and_then(|lm| lm.manifest.process.as_ref().map(|pr| pr.mode.clone())),
                            "skill": p.meta.as_ref().map(|m| serde_json::json!({
                                "name": m.name, "description": m.description })),
                            "grants": grants,
                        })
                    );
                    continue;
                }
                let (mode, hash) = match &p.manifest {
                    Some(lm) => (
                        lm.manifest
                            .process
                            .as_ref()
                            .map(|pr| pr.mode.clone())
                            .unwrap_or_else(|| "-".into()),
                        lm.hash.clone(),
                    ),
                    None => ("-".into(), String::new()),
                };
                let counts: (i64, i64) = if hash.is_empty() {
                    (0, 0)
                } else {
                    conn.query_row(
                        "SELECT
                           SUM(CASE WHEN state='requested' THEN 1 ELSE 0 END),
                           SUM(CASE WHEN state='approved' THEN 1 ELSE 0 END)
                         FROM grants WHERE package=?1 AND manifest_hash=?2",
                        rusqlite::params![p.name, hash],
                        |r| {
                            Ok((
                                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                            ))
                        },
                    )?
                };
                let kind = match (&p.manifest, &p.meta) {
                    (Some(_), Some(_)) => "actor+skill",
                    (Some(_), None) => "actor",
                    (None, Some(_)) => "skill",
                    (None, None) => "empty",
                };
                let desc = p.meta.as_ref().map(|m| m.description.clone()).unwrap_or_default();
                println!(
                    "{:<12} {:<12} mode={:<7} pending={:<3} granted={:<3} {}",
                    p.name, kind, mode, counts.0, counts.1, desc
                );
            }
        }
        Cmd::Stages { profile: pname } => {
            let conn = open(&root)?;
            let (prof, _) = profile::load(&root, &pname)?;
            println!("seed (built-in, once per run): blocks -> providers -> skills-inventory");
            let chain = context::chain(&root, &conn, &prof)?;
            if chain.is_empty() {
                println!("chain: (no package stages declared)");
            } else {
                println!("chain (per LLM call, order/package/stage):");
                for s in &chain {
                    println!(
                        "  {:>5}  {}/{}  mode={}  {}  [{}]",
                        s.order,
                        s.package,
                        s.name,
                        s.mode,
                        if s.approved { "approved" } else { "REQUESTED (inert until approved)" },
                        s.script.display()
                    );
                }
            }
        }
        Cmd::Kit { cmd } => match cmd {
            KitCmd::Add { kit: kref, copy, pending } => {
                let dir = kit::resolve(&root, &kref)?;
                let conn = open(&root)?;
                let mode = if copy { kit::Mode::Copy } else { kit::Mode::Link };
                let readme = kit::install(&root, &conn, &dir, mode, !pending)?;
                println!("installed kit from {}", dir.display());
                if let Some(r) = readme {
                    println!();
                    println!("{}", r.trim_end());
                }
            }
            KitCmd::List { json } => {
                for (name, dir, hook) in kit::list(&root)? {
                    if json {
                        println!("{}", serde_json::json!({ "name": name, "dir": dir, "hook": hook }));
                    } else {
                        println!("{name:<16} {hook}  [{}]", dir.display());
                    }
                }
            }
            KitCmd::Show { kit: kref } => {
                print!("{}", kit::show(&root, &kref)?);
            }
            KitCmd::Unlink { kit: kref } => {
                let dir = kit::resolve(&root, &kref)?;
                kit::unlink(&root, &dir)?;
            }
        },
        Cmd::Models { profile: pname, json } => models::list(&root, &pname, json)?,
        Cmd::Profile { cmd } => match cmd {
            ProfileCmd::List => profilecli::list(&root)?,
            ProfileCmd::Get { name } => profilecli::get(&root, &name)?,
            ProfileCmd::Set { name, pairs } => profilecli::set(&root, &name, &pairs)?,
            ProfileCmd::New { name, agent, model } => {
                profilecli::new(&root, &name, agent.as_deref(), model.as_deref())?
            }
            ProfileCmd::Validate { path } => profilecli::validate(&path)?,
        },
        Cmd::Approve { name, by } => {
            let conn = open(&root)?;
            packages::decide(&root, &conn, &name, true, &by)?;
        }
        Cmd::Revoke { name, by, force } => {
            // Stdlib packages are protected: the product depends on them, so
            // revoking one is a deliberate act, not a casual one (docs/config.md).
            if !force && kit::protected_packages(&root).contains(&name) {
                anyhow::bail!(
                    "⚠ {name} is a protected stdlib package — the product depends on it \
                     (docs/config.md). Revoking it breaks things (e.g. the web UI's \
                     transcripts). Re-run with --force if you really mean it."
                );
            }
            let conn = open(&root)?;
            packages::decide(&root, &conn, &name, false, &by)?;
        }
        Cmd::Inbox => {
            let conn = open(&root)?;
            human::inbox(&root, &conn)?;
        }
        Cmd::Answer { ask_id, text } => {
            let conn = open(&root)?;
            human::answer(&root, &conn, ask_id, &text)?;
        }
        Cmd::Ask { question, options, deadline_minutes, default } => {
            let conn = open(&root)?;
            human::ask(&root, &conn, &question, options.as_deref(), deadline_minutes, default.as_deref())?;
        }
        Cmd::Bus { cmd } => match cmd {
            BusCmd::Pub { topic, payload, qos, retain, correlation } => {
                buscli::publish(&root, &topic, payload.as_deref(), qos, retain, correlation.as_deref())?;
            }
            BusCmd::Sub { filter, count, timeout, blocking, order, timeout_ms, on_timeout, phase, point } => {
                let b = blocking.then_some(buscli::BlockingOpts {
                    order,
                    timeout_ms,
                    on_timeout,
                    phase,
                    point,
                });
                buscli::subscribe(&root, &filter, count, timeout, b)?;
            }
        },
        Cmd::Events { limit } => {
            let conn = open(&root)?;
            let mut stmt = conn.prepare(
                "SELECT id, type, state, cause_id, correlation_id, substr(COALESCE(payload,''),1,60), created_at
                 FROM events ORDER BY id DESC LIMIT ?1",
            )?;
            let rows: Vec<(i64, String, String, Option<i64>, Option<String>, String, String)> = stmt
                .query_map([limit], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, t, state, cause, corr, payload, created) in rows.into_iter().rev() {
                let cause = cause.map(|c| format!("<-{c}")).unwrap_or_default();
                let corr = corr
                    .map(|c| format!(" corr={}", c.chars().take(8).collect::<String>()))
                    .unwrap_or_default();
                println!("#{id:<5} {created} {t:<20} {state:<16} {cause}{corr} {payload}");
            }
        }
    }
    Ok(())
}

fn open(root: &paths::Root) -> Result<rusqlite::Connection> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    Ok(conn)
}

fn parse_json_opt(s: Option<&str>) -> Result<Option<Value>> {
    match s {
        None => Ok(None),
        Some(s) => Ok(Some(serde_json::from_str(s).map_err(|e| {
            anyhow::anyhow!("invalid JSON {s:?}: {e}")
        })?)),
    }
}

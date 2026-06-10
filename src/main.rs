mod broker;
mod bus;
mod buscli;
mod db;
mod dispatcher;
mod dotenv;
mod events;
mod exec;
mod hooks;
mod human;
mod initcmd;
mod manifest;
mod paths;
mod profile;
mod recorder;
mod render;
mod sandbox;
mod skills;
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
    Init { dir: Option<PathBuf> },
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
        /// ISO8601; for human/ask: when the default fires
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
    /// Materialize a skill package's registrations into handlers.d/ + tables
    Enable { name: String },
    /// Remove a skill package's registrations
    Disable { name: String },
    /// List skill packages and their wiring
    Skills,
    /// What's blocked on you?
    Inbox,
    /// Answer an ask by event id
    Answer { ask_id: i64, text: String },
    /// Sugar over emit: human/ask with correlation + deadline + default
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
        Cmd::Init { dir } => {
            return initcmd::init(dir.unwrap_or(std::env::current_dir()?));
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
                    etype: r#type,
                    payload: parse_json_opt(payload.as_deref())?,
                    priority,
                    correlation,
                    deadline,
                    default_action: parse_json_opt(default_action.as_deref())?,
                    idempotency,
                    cause,
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
            exec::run(&root, exec::ExecOpts { session, profile, prompt, resume })?;
        }
        Cmd::HandleExec => exec::handle_exec(&root)?,
        Cmd::Render { profile, session } => {
            let conn = open(&root)?;
            println!("{}", render::render(&root, &conn, &profile, &session)?);
        }
        Cmd::Enable { name } => {
            let conn = open(&root)?;
            skills::enable(&root, &conn, &name)?;
        }
        Cmd::Disable { name } => {
            let conn = open(&root)?;
            skills::disable(&root, &conn, &name)?;
        }
        Cmd::Skills => {
            let conn = open(&root)?;
            for s in skills::list(&root)? {
                let crons: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM crons WHERE skill=?1",
                    [&s.name],
                    |r| r.get(0),
                )?;
                // A package is enabled if anything of it is wired: handlers or crons.
                let enabled = skills::is_enabled(&root, &s.name) || crons > 0;
                let kind = match (&s.manifest, &s.meta) {
                    (Some(_), Some(_)) => "handlers+skill",
                    (Some(_), None) => "handlers",
                    (None, Some(_)) => "skill",
                    (None, None) => "empty",
                };
                let desc = s.meta.as_ref().map(|m| m.description.clone()).unwrap_or_default();
                println!(
                    "{:<12} {:<16} enabled={:<5} crons={} {}",
                    s.name, kind, enabled, crons, desc
                );
            }
        }
        Cmd::Inbox => {
            let conn = open(&root)?;
            human::inbox(&conn)?;
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
            BusCmd::Pub { topic, payload, qos, retain } => {
                buscli::publish(&root, &topic, payload.as_deref(), qos, retain)?;
            }
            BusCmd::Sub { filter, count, timeout } => {
                buscli::subscribe(&root, &filter, count, timeout)?;
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

use anyhow::Result;
use clap::{Parser, Subcommand};
use lanius::{
    agentcli, blockcli, buscli, code_projection, codeagent, configcli, context, db, dev, discover,
    dispatcher, dotenv, envcompat, estimatecli, events, exec, human, initcmd, kbcli, kit, mailcli,
    manifest, models, packages, paths, profile, profilecli, providercli, render, secrets, trace,
    web,
};
use serde_json::Value;
use std::path::PathBuf;

/// Read all of stdin as a UTF-8 string (used by `kb write` when `--content` is
/// omitted, so a harness can pipe knowledge in).
fn read_stdin() -> anyhow::Result<String> {
    use std::io::Read as _;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

fn leading_comment_summary(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        let Some(comment) = trimmed.strip_prefix('#') else {
            break;
        };
        lines.push(comment.trim().to_string());
    }
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn package_manifest_description(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("lanius.toml"))
        .or_else(|_| std::fs::read_to_string(dir.join("elanus.toml")))
        .map(|raw| leading_comment_summary(&raw))
        .unwrap_or_default()
}

#[derive(Parser)]
#[command(
    name = "lanius",
    version,
    about = "lanius: a minimal event-driven agent harness"
)]
struct Cli {
    /// Lanius root (default: $LANIUS_ROOT, else ~/.lanius/root)
    #[arg(short = 'C', long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a lanius root (db, trace log, default profile, stock skills)
    Init {
        dir: Option<PathBuf>,
        /// Kit(s) to install: packages linked (or --copy vendored) + granted,
        /// profiles copied if missing, README printed. A value containing '/'
        /// is a path; a bare name resolves against <root>/kits (seeded with
        /// the stock kits), ~/.lanius/kits, $LANIUS_KIT_PATH, then the repo
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
    /// Context program inspection (render the current context document)
    Context {
        #[command(subcommand)]
        cmd: ContextCmd,
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
    /// Launch native/profile agents: catalog (discover), run (blocking), spawn (durable, async)
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Package configuration (docs/config.md): set / get / list. A `set` commits
    /// the change on the config repo's `live` branch and records who accepted it.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Model providers (docs/handoffs/model-providers.md): a named, encrypted
    /// credential any LLM consumer can be pointed at. add / list / get / test / rm.
    /// The secret is encrypted at rest and never printed.
    Provider {
        #[command(subcommand)]
        cmd: ProviderCmd,
    },
    /// Memory blocks (docs/handoffs/memory-blocks.md): named, durable, editable
    /// chunks of prompt. set / get / list / append / rm — owner-scoped, upserted
    /// into context_blocks, seeded into the system context by priority.
    Block {
        #[command(subcommand)]
        cmd: BlockCmd,
    },
    /// Knowledge bases (docs/handoffs/kb-core.md): a `kb/` subfolder any package
    /// carries and declares with a `[kb]` marker. list names the enabled ones;
    /// write commits a file into a KB's `kb/` tree with the hardened git path.
    Kb {
        #[command(subcommand)]
        cmd: KbCmd,
    },
    /// Capability discovery (docs/handoffs/kb-discovery.md): search the instance's
    /// package UNIVERSE — not just the agent's visible set — for a capability the
    /// caller lacks. "You don't have the discord package enabled, but it exists and
    /// matches your query." Reports what enabling it would add (kb/, skills, tools,
    /// stages) and the enable path. Privileged (universe read); grants nothing.
    Discover {
        /// The query — plain words, e.g. "discord api".
        query: Vec<String>,
        /// The profile whose visible set is the "already have it" baseline.
        #[arg(long, default_value = "default")]
        profile: String,
        /// Emit the machine-stable JSON report (the find_capability tool's input).
        #[arg(long)]
        json: bool,
    },
    /// Work estimation (docs/handoffs/work-estimation.md): an agent records a
    /// multi-dimensional estimate (dollars/turns/tokens/wall-clock) right after it
    /// plans (set), the package counts actuals from that boundary and reports the
    /// variance (actual), and a retro appends the miss to a durable learned block
    /// (retro). No kernel data model — state lives in memory blocks + obs events.
    Estimate {
        #[command(subcommand)]
        cmd: EstimateCmd,
    },
    /// Run the dispatcher: poll events, fork handlers, record exits
    Daemon {
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Run the local dev stack: daemon + web relay + Vite UI, supervised
    Dev {
        /// Dispatcher poll interval for the daemon child.
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
        /// Port for the web relay backend.
        #[arg(long, default_value_t = 7180)]
        web_port: u16,
        /// Port for the Vite dev server.
        #[arg(long, default_value_t = 5173)]
        vite_port: u16,
        /// If the requested web/vite ports are busy, walk up to the next free pair
        /// instead of failing to bind. The banner prints the resolved ports.
        #[arg(long)]
        shift_ports: bool,
    },
    /// Run the packaged stack: the daemon (this binary) + the web server (also
    /// this binary, `lanius web`), supervised. The prod counterpart of `dev` —
    /// no cargo, no `--watch`, no Vite, no Node: the SPA is embedded in the binary
    /// (src/web.rs `include_dir!`), so an installed `cargo install lanius` works
    /// off any host with no checkout.
    Serve {
        /// Dispatcher poll interval for the daemon child.
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
        /// Port for the web server (serves the embedded SPA).
        #[arg(long, default_value_t = 7180)]
        web_port: u16,
        /// Ignored — the SPA is embedded in the binary (nothing to npm-build at
        /// serve time). Kept for flag compatibility; use `lanius dev` for the
        /// Vite hot-reload loop.
        #[arg(long)]
        rebuild: bool,
        /// Additional hostname allowed by the web UI's Host/Origin guard.
        /// Repeat for multiple reverse-proxy or private-network names.
        #[arg(long = "trusted-host")]
        trusted_hosts: Vec<String>,
    },
    /// Serve the web dashboard in-process: the embedded SPA + the SSE bus relay +
    /// the JSON API (the Rust port of ui/web/server.mjs — no Node at runtime). Run
    /// standalone beside the daemon, or supervised by `serve`/`dev`.
    Web {
        /// Port for the web server (serves the embedded SPA).
        #[arg(long, default_value_t = 7180)]
        port: u16,
        /// Agent the dashboard targets by default.
        #[arg(long, default_value = "main")]
        agent: String,
        /// Additional hostname allowed by the Host/Origin guard. Loopback names
        /// remain trusted by default. Repeat for multiple names.
        #[arg(long = "trusted-host")]
        trusted_hosts: Vec<String>,
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
    /// Schedule a one-shot self-wake for an agent (docs/handoffs/timers.md):
    /// at the given time the harness delivers in/agent/<agent> a message and
    /// the agent runs a turn. A trusted operator gesture — unlike the self-only
    /// `schedule_event` tool, this may target any named agent's mailbox. Give
    /// exactly one of --in / --at.
    Schedule {
        /// The agent noun to wake (its mailbox is in/agent/<agent>).
        #[arg(long, default_value = "main")]
        agent: String,
        /// Wake this many seconds from now.
        #[arg(long)]
        r#in: Option<f64>,
        /// Absolute rfc3339 time to wake at.
        #[arg(long)]
        at: Option<String>,
        /// What to do on wake (the prompt the woken turn receives).
        #[arg(long)]
        message: String,
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
        /// `check` runs the deterministic dependency-validity report (each
        /// problem paired with its exact fix command); omit to list packages.
        #[arg(value_name = "ACTION")]
        action: Option<String>,
        /// Machine-readable: one JSON object per package, including each
        /// pending/approved grant row (the UI's pending-review queue). With
        /// `check`, the stable validity-report JSON the helper/UI relays.
        #[arg(long)]
        json: bool,
        /// Resolve packages through this profile's effective elanus_path.
        #[arg(long, default_value = "default")]
        profile: String,
        /// Run the dependency-validity check (same as `packages check`).
        #[arg(long)]
        check: bool,
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
    /// Launch and observe an external coding agent.
    ///
    /// `lanius code <tool> [args...]` launches the real coding agent in this
    /// directory, observed on the bus (`tool` selects the adapter — `claude` or
    /// `codex`; everything after it is passed through unchanged). Reserved first
    /// words: `hook` is the internal hook bridge the generated hooks invoke
    /// (`lanius code hook <Event>`). (To re-attach to a session interactively, just
    /// relaunch its tool with the tool's own resume flag passed through, e.g.
    /// `lanius code claude --resume <native_session>`; there is no `resume` verb.)
    /// `deliver <worker-session> "<message>"` (run from inside a session) dispatches
    /// work to a worker and records the running session as the requester (M4-B);
    /// `send "<message>" [--corr <id>]` (run from inside a session) sends a
    /// non-blocking message to the human owner's chat as the running session;
    /// `spawn <tool> "<task>"` (run from inside a session) starts a worker
    /// detached and delivers its completion back to the spawner's mailbox;
    /// `inbox` (run from inside a session) reads ITS OWN inbox (M3, own-inbox-only by
    /// construction); `note <session> "<text>"` leaves a per-session memory note (M3).
    #[command(disable_help_flag = true)]
    Code {
        /// Arguments passed straight through to the tool, or an `lanius code` verb.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// All profiles, one JSON object per line
    List,
    /// One profile: parsed summary + raw TOML, as JSON
    Get { name: String },
    /// Set dotted keys, comments preserved, validated before writing:
    /// lanius profile set default agent=kestrel model.max_turns=12
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
    /// Replace a profile.toml from a validated candidate file, then commit it
    /// on the config repo's live branch.
    Put {
        name: String,
        /// path to the candidate profile.toml file
        path: String,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Set one key for a package: lanius config set watcher accounts '["a","b"]'
    /// (creates the package's config if absent; value parses as TOML when it can)
    Set {
        /// package name
        pkg: String,
        /// dotted key path (e.g. accounts, or limits.max)
        key: String,
        /// the value (TOML when it parses — arrays, ints, bools — else a string).
        /// Quote to force a string for tokens that look like TOML, e.g. an
        /// account named "inf" or a date-shaped id: 'config set w h "2026-06-15"'
        value: String,
    },
    /// Print one value
    Get { pkg: String, key: String },
    /// List a package's config (raw TOML), or every package that has config
    List { pkg: Option<String> },
    /// Pending agent proposals (docs/config.md): one JSON line each
    Proposals,
    /// Show a proposal's diff vs live config
    Show { id: String },
    /// Accept a proposal: merge it into live config
    Accept { id: String },
    /// Decline a proposal: drop it without applying
    Decline { id: String },
}

/// Native/profile agents — launch discovery, blocking run, durable spawn.
///
/// `catalog` inventories what you can launch (profiles + their packages, coding
/// tools, providers); `run` executes a turn in the foreground; `spawn` queues a
/// turn for the daemon (async, durable). `spawn` needs the profile to be
/// spawn-ready — an approved exec package must subscribe to its mailbox — while
/// `run` works for any profile. Both carry launch-time overrides: `--with-package`
/// widens the run's visible packages (approved packages only — visibility, not
/// authority) and `--provider` pins the model provider for the run. Native agents
/// launch peers with the `launch_agent` tool; coding workers use `lanius code`.
#[derive(Subcommand)]
enum AgentCmd {
    /// Inventory launchable things: native profiles + their packages, coding tools, providers (--json for a machine-readable pick)
    Catalog {
        /// Emit the full inventory as JSON (fields: coding_tools, profiles[], providers[]).
        #[arg(long)]
        json: bool,
    },
    /// Run a native/profile agent turn in the foreground (blocking; any profile)
    Run {
        /// Which native profile to run (its identity, model, and package path).
        #[arg(long, default_value = "default")]
        profile: String,
        /// Optional session id; generated by exec when omitted.
        #[arg(long)]
        session: Option<String>,
        /// Widen this run's visible packages: an approved package name (repeatable). Already-visible packages are a no-op; an un-granted package is refused. Visibility only — bus authority stays gated by grants.
        #[arg(long = "with-package")]
        with_packages: Vec<String>,
        /// Override the profile's model provider for this run (a name from `lanius provider list`).
        #[arg(long)]
        provider: Option<String>,
        /// Prompt text, or '-' to read stdin through exec.
        prompt: String,
    },
    /// Queue a native/profile agent turn for the daemon to run (async, durable). Requires an approved exec handler on the profile's mailbox; emits {event, correlation, session, mailbox}.
    Spawn {
        /// Which native profile to spawn (must be spawn-ready — see `catalog`).
        #[arg(long, default_value = "default")]
        profile: String,
        /// Optional session id; generated when omitted.
        #[arg(long)]
        session: Option<String>,
        /// Event priority for daemon scheduling.
        #[arg(long, default_value_t = 0)]
        priority: i64,
        /// Widen this run's visible packages: an approved package name (repeatable). Un-granted packages are refused. Visibility only, for this run — the profile.toml is untouched.
        #[arg(long = "with-package")]
        with_packages: Vec<String>,
        /// Override the profile's model provider for this run (a name from `lanius provider list`).
        #[arg(long)]
        provider: Option<String>,
        /// Prompt text.
        prompt: String,
    },
}

#[derive(Subcommand)]
enum ProviderCmd {
    /// Define a provider. An api-key provider needs --base-url and a key
    /// (--key, --key-env <VAR>, or piped on stdin); a native-login provider
    /// (--native) carries no secret. Repeatable --header Name=Value.
    Add {
        name: String,
        /// Make a native-login provider ("use the tool's own login; inject nothing").
        #[arg(long)]
        native: bool,
        /// Optional harness pin for a native-login provider (claude|codex|opencode).
        #[arg(long)]
        tool: Option<String>,
        /// Wire/adapter for an api-key provider: anthropic (default) | openai.
        #[arg(long)]
        wire: Option<String>,
        /// Base URL for an api-key provider (e.g. https://api.deepseek.com/anthropic).
        #[arg(long)]
        base_url: Option<String>,
        /// The literal API key (convenience — visible in the process table; prefer --key-env/stdin).
        #[arg(long)]
        key: Option<String>,
        /// Read the API key from this environment variable (keeps it off the command line).
        #[arg(long)]
        key_env: Option<String>,
        /// Extra header Name=Value (repeatable; values are encrypted at rest).
        #[arg(long = "header")]
        headers: Vec<String>,
    },
    /// List providers (metadata only; secret never shown)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one provider's metadata (secret redacted)
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Probe a provider's /models endpoint for reachability (decrypts transiently)
    Test {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a provider
    Rm { name: String },
}

/// Block-addressing flags shared by every `lanius block` verb (clap flattens
/// them into each subcommand).
#[derive(clap::Args)]
struct BlockArgs {
    /// The profile whose agent owns the block (also the render context).
    #[arg(long, default_value = "default")]
    profile: String,
    /// The session a session/run-scoped block binds to.
    #[arg(long, default_value = "render-preview")]
    session: String,
    /// global | agent | session | run (run is stage-only).
    #[arg(long, default_value = "agent")]
    scope: String,
    /// system | before_messages | after_messages | user | scratch. Prefer
    /// `system` (the default): it rides the cached system prefix, cheap until
    /// edited. `user` folds into the user turn each activation — edits land
    /// immediately but the block is re-sent in full every turn; pick it only
    /// for a block that changes nearly every turn. The other three placements
    /// are stored but not yet rendered.
    #[arg(long, default_value = "system")]
    placement: String,
    /// Render order relative to the profile's static blocks (negative = before).
    #[arg(long, allow_hyphen_values = true)]
    priority: Option<i32>,
    /// Override the owner identity: a SELF-ATTESTED label, not an authenticated
    /// identity (this local-trusted CLI has no broker session to verify it).
    /// Defaults to the profile's agent noun. A mismatched value only writes a
    /// different owner row — it cannot read or overwrite another owner's blocks.
    #[arg(long)]
    owner: Option<String>,
    /// Decided-by attribution: who drove this write (e.g. `ui` for a human edit
    /// through the web inspector). Recorded in `context_build_log` so a UI write is
    /// attributable — mirrors the `--by ui` trail every `/api/admin` mutation stamps.
    #[arg(long)]
    by: Option<String>,
    /// Free-JSON meta for the block: a KB pointer block carries
    /// {"kb":"<pkg>","path":"kb/role-verifier.md","lines":"12-28","sha":"<sha>"}
    /// (kb-core.md M3). Must be a JSON object.
    #[arg(long)]
    meta: Option<String>,
}

impl BlockArgs {
    fn opts(&self) -> blockcli::BlockOpts {
        blockcli::BlockOpts {
            profile: self.profile.clone(),
            session: self.session.clone(),
            scope: self.scope.clone(),
            placement: self.placement.clone(),
            priority: self.priority,
            owner: self.owner.clone(),
            by: self.by.clone(),
            meta: self.meta.clone(),
        }
    }
}

#[derive(Subcommand)]
enum BlockCmd {
    /// Set (upsert) a block: lanius block set identity "I am Lily."
    Set {
        name: String,
        content: String,
        #[command(flatten)]
        args: BlockArgs,
    },
    /// Append to a block (creating it if absent; a newline joins prior content)
    Append {
        name: String,
        content: String,
        #[command(flatten)]
        args: BlockArgs,
    },
    /// Print one block's content
    Get {
        name: String,
        #[command(flatten)]
        args: BlockArgs,
    },
    /// The system-placement blocks visible to a profile, one JSON line each
    List {
        #[command(flatten)]
        args: BlockArgs,
    },
    /// Remove a block
    Rm {
        name: String,
        #[command(flatten)]
        args: BlockArgs,
    },
}

#[derive(Subcommand)]
enum KbCmd {
    /// List the enabled knowledge bases (packages carrying a [kb] marker)
    List {
        /// The profile whose visible package set is enumerated.
        #[arg(long, default_value = "default")]
        profile: String,
        /// Emit one JSON line per KB instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Search the knowledge base index: lanius kb search <query>
    Search {
        /// The query — plain words, e.g. "who verifies".
        query: Vec<String>,
        /// Max hits to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Emit one JSON line per hit instead of the human list.
        #[arg(long)]
        json: bool,
    },
    /// Parse a KB entry's frontmatter + links (deterministic, no LLM): lanius kb parse <pkg> <path>
    Parse {
        /// The package that owns the KB (must be on the path).
        pkg: String,
        /// The path INSIDE the package's kb/ tree (e.g. role-verifier.md).
        path: String,
        /// Emit the parsed entry as one JSON object instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Write a file into a KB's kb/ tree and commit it: lanius kb write <pkg> <path>
    Write {
        /// The package that owns the KB (must be on the path).
        pkg: String,
        /// The path INSIDE the package's kb/ tree (e.g. role-verifier.md or notes/x.md).
        path: String,
        /// The content to write inline; omit to read the content from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// The groundskeeper sweep (no LLM): validate pointer blocks, find orphans, flag staleness
    Check {
        /// The profile whose visible KBs are swept.
        #[arg(long, default_value = "default")]
        profile: String,
        /// Emit the report as JSON instead of the human summary.
        #[arg(long)]
        json: bool,
        /// Mail the report to the owner (in/human/owner) when there are findings.
        #[arg(long)]
        mail: bool,
    },
    /// Apply a unified diff into a KB's kb/ tree and commit it (the ratifier's apply path)
    ApplyDiff {
        /// The package that owns the KB (must be on the path).
        pkg: String,
        /// The unified diff inline; omit to read it from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Run the diff-pipeline dispatch (setup-gated): spawn the compactor if set up
    Groundskeep {
        /// The profile whose visible KBs the compactor sweeps.
        #[arg(long, default_value = "default")]
        profile: String,
    },
}

/// Shared addressing for the estimate verbs (the agent/session a block belongs to).
#[derive(clap::Args)]
struct EstimateArgs {
    /// The profile whose agent owns the estimate blocks (also the render context).
    #[arg(long, default_value = "default")]
    profile: String,
    /// The coding session the estimate counts against.
    #[arg(long, default_value = "render-preview")]
    session: String,
    /// Override the owner identity (the agent noun). Defaults to the profile's
    /// agent noun — a self-attested label, like `lanius block --owner`.
    #[arg(long)]
    owner: Option<String>,
    /// Override the pricing.toml path (model id -> $/token). Defaults to the
    /// estimation package's shipped copy under the root's package path.
    #[arg(long)]
    pricing: Option<PathBuf>,
}

impl EstimateArgs {
    fn opts(&self) -> estimatecli::EstimateOpts {
        estimatecli::EstimateOpts {
            profile: self.profile.clone(),
            session: self.session.clone(),
            owner: self.owner.clone(),
            pricing: self.pricing.clone(),
        }
    }
}

#[derive(Subcommand)]
enum EstimateCmd {
    /// E1 — record the multi-dimensional estimate (latest wins). Writes the
    /// `estimate` block + emits obs/estimate/<session> (the count-from boundary).
    Set {
        /// Headline dollars (the cross-model normalizer).
        #[arg(long)]
        dollars: Option<f64>,
        /// Estimated agent turns.
        #[arg(long)]
        turns: Option<i64>,
        /// Estimated total tokens.
        #[arg(long)]
        tokens: Option<i64>,
        /// Estimated wall-clock, in milliseconds.
        #[arg(long = "wall-clock", alias = "wall-clock-ms")]
        wall_clock_ms: Option<i64>,
        #[command(flatten)]
        args: EstimateArgs,
    },
    /// E2 — compute actuals from the obs projection (from the boundary onward),
    /// price tokens via pricing.toml, write the `estimate-vs-actual` block, print
    /// the report. A session with no estimate is skipped.
    Actual {
        /// Emit the report as JSON (the `Report` struct) instead of the
        /// block-content text. Backs the web /api/estimate/{session} route.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        args: EstimateArgs,
    },
    /// E3 — append the dated miss to the durable `estimation` block (agent scope),
    /// so the next estimate reads the prior misses. Skips when no estimate exists.
    Retro {
        #[command(flatten)]
        args: EstimateArgs,
    },
}

#[derive(Subcommand)]
enum ContextCmd {
    /// Render the transformed context document without calling the provider.
    /// `--event` accepts an event id, a full event JSON envelope, an already
    /// normalized context event, or a payload JSON object.
    Render {
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long, default_value = "render-preview")]
        session: String,
        #[arg(long)]
        event: Option<String>,
    },
}

#[derive(Subcommand)]
enum KitCmd {
    /// Install a kit into this root: packages linked onto the package path
    /// (or --copy vendored), profiles copied if missing, packages granted
    /// with provenance kit:<name>, README printed
    Add {
        /// Kit name (resolved via <root>/kits, ~/.lanius/kits,
        /// $LANIUS_KIT_PATH, <repo>/kits) or a path
        kit: String,
        /// Vendor packages into the root's packages/ instead of linking
        #[arg(long)]
        copy: bool,
        /// STAGE only: files land and requests register, but every grant
        /// stays pending — commit with `lanius approve <package>` (the
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
    Unlink {
        kit: String,
        /// Required to unlink a protected stdlib kit
        #[arg(long)]
        force: bool,
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
    // Die quietly on EPIPE like a normal Unix tool (`lanius inbox | grep -q`).
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
        Cmd::Init {
            ref dir,
            ref kit,
            copy,
        } => {
            // Same resolution order as every other command: explicit arg >
            // $LANIUS_ROOT (or legacy $HARNESS_ROOT) > ~/.lanius/root. Init once
            // targeted cwd while the env var pointed elsewhere, littering
            // template roots into repos and test directories.
            let dir = match dir
                .clone()
                .or_else(|| envcompat::read("ROOT").map(PathBuf::from))
            {
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
        Cmd::Dev {
            interval_ms,
            web_port,
            vite_port,
            shift_ports,
        } => {
            // dev resolves its OWN isolated, repo-local root (target/lanius-dev) —
            // it deliberately ignores the global root so it can never run against
            // ~/.lanius/root and collide with `serve`/coding sessions. See dev::run.
            dev::run(interval_ms, web_port, vite_port, shift_ports)?
        }
        Cmd::Serve {
            interval_ms,
            web_port,
            rebuild,
            trusted_hosts,
        } => dev::serve(&root, interval_ms, web_port, rebuild, &trusted_hosts)?,
        Cmd::Web {
            port,
            agent,
            trusted_hosts,
        } => {
            // server.mjs parity: LANIUS_WEB_PORT overrides the default when no
            // explicit --port is on the command line. clap can't tell a default
            // from an explicit equal value, so honor the env var only when --port
            // is left at the default; supervisors (serve/dev) pass --port anyway.
            let port = if port == 7180 {
                std::env::var("LANIUS_WEB_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(port)
            } else {
                port
            };
            web::serve_web(&root, port, &agent, &trusted_hosts)?
        }
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
        Cmd::Schedule {
            agent,
            r#in,
            at,
            message,
        } => {
            let fire_at = match (r#in, at) {
                (Some(_), Some(_)) => {
                    anyhow::bail!("schedule: give exactly one of --in or --at, not both")
                }
                (None, None) => anyhow::bail!("schedule: give one of --in or --at"),
                (Some(secs), None) => (chrono::Utc::now()
                    + chrono::Duration::milliseconds((secs * 1000.0) as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                (None, Some(at)) => chrono::DateTime::parse_from_rfc3339(&at)?
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            };
            let conn = open(&root)?;
            // Trusted operator surface ("CLI is the API"): may target any named
            // agent's mailbox, reusing M2's row shape. The wake carries no
            // conversation session — a CLI-initiated reminder isn't threaded to
            // a chat, so the send that closes the loop lands on the mailbox.
            let emit_type = lanius::topic::agent_mailbox(&agent);
            let payload = serde_json::json!({ "prompt": message, "session": Value::Null });
            conn.execute(
                "INSERT INTO scheduled_events(fire_at, emit_type, payload, created_by, fired)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![fire_at, emit_type, payload.to_string(), "cli"],
            )?;
            println!(
                "scheduled {} to wake at {fire_at}",
                lanius::topic::agent_mailbox(&agent)
            );
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
        Cmd::Exec {
            prompt,
            session,
            profile,
            resume,
        } => {
            let result = exec::run(
                &root,
                exec::ExecOpts {
                    session,
                    profile,
                    prompt,
                    resume,
                    event: None,
                    with_packages: Vec::new(),
                    provider: None,
                    model: None,
                    budget: None,
                },
            );
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
        Cmd::Context { cmd } => match cmd {
            ContextCmd::Render {
                profile,
                session,
                event,
            } => {
                let out = exec::render_context(
                    &root,
                    exec::ContextRenderOpts {
                        profile,
                        session,
                        event,
                    },
                )?;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        },
        Cmd::Packages {
            action,
            json,
            profile,
            check,
        } => {
            let conn = open(&root)?;
            packages::sync(&root, &conn)?;
            // `elanus packages check` (or `--check`): the dependency-validity
            // report (docs/handoffs/package-dependencies.md M3). Non-zero exit on
            // failure so a script/agent can branch on it.
            if check || action.as_deref() == Some("check") {
                let report = packages::validate(&root, &conn, &profile)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report.to_json())?);
                } else {
                    println!("{}", report.human());
                }
                if !report.is_ok() {
                    std::process::exit(1);
                }
                return Ok(());
            }
            if let Some(a) = &action {
                anyhow::bail!("unknown packages action {a:?} (did you mean `check`?)");
            }
            for p in packages::discover_for_profile(&root, &profile)? {
                if json {
                    let hash = p
                        .manifest
                        .as_ref()
                        .map(|lm| lm.hash.clone())
                        .unwrap_or_default();
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
                            "manifest": p.manifest.as_ref().map(|lm| serde_json::json!({
                                "description": package_manifest_description(&p.dir),
                                "request": {
                                    "subscribe": lm.manifest.request.subscribe,
                                    "publish": lm.manifest.request.publish,
                                    "blocking": lm.manifest.request.blocking,
                                    "fs_write": lm.manifest.request.fs_write,
                                },
                                "process": lm.manifest.process.as_ref().map(|pr| serde_json::json!({
                                    "mode": pr.mode,
                                    "run": pr.run,
                                    "http": pr.http,
                                })),
                                "hooks": lm.manifest.hook.len(),
                                "cron": lm.manifest.cron.len(),
                                "providers": lm.manifest.provider.len(),
                                "stages": lm.manifest.stage.iter().map(|s| serde_json::json!({
                                    "name": s.name,
                                    "mode": s.mode,
                                    "order": s.order,
                                    "config": s.config.iter().map(|c| serde_json::json!({
                                        "key": c.key,
                                        "type": c.kind,
                                        "default": c.default.as_ref().map(manifest::toml_to_json),
                                        "label": c.label,
                                        "help": c.help,
                                        "agent_tunable": c.agent_tunable,
                                        "options": c.options,
                                    })).collect::<Vec<_>>(),
                                })).collect::<Vec<_>>(),
                                "mcp": lm.manifest.mcp.iter().map(|s| serde_json::json!({
                                    "name": s.name,
                                    "transport": s.transport,
                                })).collect::<Vec<_>>(),
                                "config": {
                                    "agent_tunable": lm.manifest.config.agent_tunable,
                                },
                                "requires": {
                                    "packages": lm.manifest.requires.packages,
                                },
                                // The coding-harness adapter names this package
                                // declares (`[[harness]]`). Presentation-only
                                // metadata for the configure UI's "applies to this
                                // harness" line (docs/handoffs/package-truth.md,
                                // wonky bit 1): a package that only provides a
                                // coding-harness adapter is not loaded by a native
                                // agent. Computed from the manifest, never a
                                // hard-coded name list.
                                "harness": lm.manifest.harness.iter()
                                    .map(|h| h.name.clone())
                                    .collect::<Vec<_>>(),
                            })),
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
                let desc = p
                    .meta
                    .as_ref()
                    .map(|m| m.description.clone())
                    .unwrap_or_default();
                println!(
                    "{:<12} {:<12} mode={:<7} pending={:<3} granted={:<3} {}",
                    p.name, kind, mode, counts.0, counts.1, desc
                );
            }
        }
        Cmd::Stages { profile: pname } => {
            let conn = open(&root)?;
            let (prof, _) = profile::load(&root, &pname)?;
            println!(
                "context program: {} (max_total_ms={} policy)",
                prof.context.program, prof.context.max_total_ms
            );
            println!("seed (built-in, once per run): blocks -> providers -> skills-inventory");
            let chain = context::chain(&root, &conn, &pname, &prof)?;
            if chain.is_empty() {
                println!("chain: (no package stages declared)");
            } else {
                println!("chain (per LLM call, order/package/stage):");
                for s in &chain {
                    println!(
                        "  {:>5}  {}/{}  mode={}  timeout_ms={}  {}  [{}]",
                        s.order,
                        s.package,
                        s.name,
                        s.mode,
                        s.timeout_ms,
                        if s.approved {
                            "approved"
                        } else {
                            "REQUESTED (inert until approved)"
                        },
                        s.script.display()
                    );
                }
            }
        }
        Cmd::Kit { cmd } => match cmd {
            KitCmd::Add {
                kit: kref,
                copy,
                pending,
            } => {
                let dir = kit::resolve(&root, &kref)?;
                let conn = open(&root)?;
                let mode = if copy {
                    kit::Mode::Copy
                } else {
                    kit::Mode::Link
                };
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
                        println!(
                            "{}",
                            serde_json::json!({ "name": name, "dir": dir, "hook": hook })
                        );
                    } else {
                        println!("{name:<16} {hook}  [{}]", dir.display());
                    }
                }
            }
            KitCmd::Show { kit: kref } => {
                print!("{}", kit::show(&root, &kref)?);
            }
            KitCmd::Unlink { kit: kref, force } => {
                let dir = kit::resolve(&root, &kref)?;
                kit::guard_unlink_protected(&dir, &kref, force)?;
                kit::unlink(&root, &dir)?;
            }
        },
        Cmd::Models {
            profile: pname,
            json,
        } => models::list(&root, &pname, json)?,
        Cmd::Profile { cmd } => match cmd {
            ProfileCmd::List => profilecli::list(&root)?,
            ProfileCmd::Get { name } => profilecli::get(&root, &name)?,
            ProfileCmd::Set { name, pairs } => {
                let sha = profilecli::set(&root, &name, &pairs)?;
                emit_agent_config_changed(&root, &name, sha.as_deref())?;
            }
            ProfileCmd::New { name, agent, model } => {
                let sha = profilecli::new(&root, &name, agent.as_deref(), model.as_deref())?;
                emit_agent_config_changed(&root, &name, sha.as_deref())?;
            }
            ProfileCmd::Validate { path } => profilecli::validate(&path)?,
            ProfileCmd::Put { name, path } => {
                let sha = profilecli::put(&root, &name, &path)?;
                emit_agent_config_changed(&root, &name, sha.as_deref())?;
            }
        },
        Cmd::Agent { cmd } => match cmd {
            AgentCmd::Catalog { json } => {
                agentcli::catalog(&root, agentcli::CatalogOpts { json })?;
            }
            AgentCmd::Run {
                profile,
                prompt,
                session,
                with_packages,
                provider,
            } => {
                agentcli::run(
                    &root,
                    agentcli::RunOpts {
                        profile,
                        prompt,
                        session,
                        with_packages,
                        provider,
                    },
                )?;
            }
            AgentCmd::Spawn {
                profile,
                prompt,
                session,
                priority,
                with_packages,
                provider,
            } => {
                agentcli::spawn(
                    &root,
                    agentcli::SpawnOpts {
                        profile,
                        prompt,
                        session,
                        priority,
                        with_packages,
                        provider,
                    },
                )?;
            }
        },
        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Set { pkg, key, value } => {
                let conn = open(&root)?;
                // The accepter is the current identity (the owner), not the
                // literal "cli" used for grant decisions — config.md wants the
                // ledger's decided_by to be a real identity.
                let by = secrets::owner_name(&root);
                configcli::set(&root, &conn, &pkg, &key, &value, &by)?;
            }
            ConfigCmd::Get { pkg, key } => configcli::get(&root, &pkg, &key)?,
            ConfigCmd::List { pkg } => configcli::list(&root, pkg.as_deref())?,
            ConfigCmd::Proposals => {
                let conn = open(&root)?;
                configcli::proposals(&root, &conn)?;
            }
            ConfigCmd::Show { id } => configcli::show(&root, &id)?,
            ConfigCmd::Accept { id } => {
                let conn = open(&root)?;
                let by = secrets::owner_name(&root);
                configcli::accept(&root, &conn, &id, &by)?;
            }
            ConfigCmd::Decline { id } => {
                let conn = open(&root)?;
                let by = secrets::owner_name(&root);
                configcli::decline(&root, &conn, &id, &by)?;
            }
        },
        Cmd::Provider { cmd } => match cmd {
            ProviderCmd::Add {
                name,
                native,
                tool,
                wire,
                base_url,
                key,
                key_env,
                headers,
            } => {
                let conn = open(&root)?;
                providercli::add(
                    &root,
                    &conn,
                    providercli::AddArgs {
                        name,
                        native,
                        tool,
                        wire,
                        base_url,
                        key,
                        key_env,
                        headers,
                    },
                )?;
            }
            ProviderCmd::List { json } => {
                let conn = open(&root)?;
                providercli::list(&conn, json)?;
            }
            ProviderCmd::Get { name, json } => {
                let conn = open(&root)?;
                providercli::get(&conn, &name, json)?;
            }
            ProviderCmd::Test { name, json } => {
                let conn = open(&root)?;
                providercli::test(&root, &conn, &name, json)?;
            }
            ProviderCmd::Rm { name } => {
                let conn = open(&root)?;
                providercli::rm(&conn, &name)?;
            }
        },
        Cmd::Block { cmd } => match cmd {
            BlockCmd::Set {
                name,
                content,
                args,
            } => blockcli::set(&root, &name, &content, &args.opts())?,
            BlockCmd::Append {
                name,
                content,
                args,
            } => blockcli::append(&root, &name, &content, &args.opts())?,
            BlockCmd::Get { name, args } => blockcli::get(&root, &name, &args.opts())?,
            BlockCmd::List { args } => blockcli::list(&root, &args.opts())?,
            BlockCmd::Rm { name, args } => blockcli::rm(&root, &name, &args.opts())?,
        },
        Cmd::Kb { cmd } => match cmd {
            KbCmd::List { profile, json } => kbcli::list(&root, &profile, json)?,
            KbCmd::Search { query, limit, json } => {
                kbcli::search(&root, &query.join(" "), limit, json)?
            }
            KbCmd::Parse { pkg, path, json } => kbcli::parse(&root, &pkg, &path, json)?,
            KbCmd::Write { pkg, path, content } => {
                let content = match content {
                    Some(c) => c,
                    None => read_stdin()?,
                };
                kbcli::write(&root, &pkg, &path, &content)?;
            }
            KbCmd::Check {
                profile,
                json,
                mail,
            } => kbcli::check(&root, &profile, json, mail)?,
            KbCmd::ApplyDiff { pkg, content } => {
                let content = match content {
                    Some(c) => c,
                    None => read_stdin()?,
                };
                kbcli::apply_diff(&root, &pkg, &content)?;
            }
            KbCmd::Groundskeep { profile } => kbcli::groundskeep(&root, &profile)?,
        },
        Cmd::Discover {
            query,
            profile,
            json,
        } => {
            let report = discover::scan(&root, &profile, &query.join(" "))?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else if report.matches.is_empty() {
                println!(
                    "no available-but-disabled capability matches {:?} (everything matching is already on your path)",
                    report.query
                );
            } else {
                for m in &report.matches {
                    println!(
                        "{} (not enabled) — matches: {}",
                        m.package,
                        m.matched.join(", ")
                    );
                    let mut adds = Vec::new();
                    if !m.adds.kb.is_empty() {
                        adds.push(format!("kb ({})", m.adds.kb.join(", ")));
                    }
                    if !m.adds.skills.is_empty() {
                        adds.push(format!("skill {}", m.adds.skills.join(", ")));
                    }
                    if !m.adds.tools.is_empty() {
                        adds.push(format!("tool {}", m.adds.tools.join(", ")));
                    }
                    if !m.adds.stages.is_empty() {
                        adds.push(format!("stage {}", m.adds.stages.join(", ")));
                    }
                    if !m.adds.mcp.is_empty() {
                        adds.push(format!("mcp {}", m.adds.mcp.join(", ")));
                    }
                    if !m.adds.harnesses.is_empty() {
                        adds.push(format!("harness {}", m.adds.harnesses.join(", ")));
                    }
                    if !adds.is_empty() {
                        println!("  enabling adds: {}", adds.join("; "));
                    }
                    println!("  enable: {}", m.enable);
                }
            }
        }
        Cmd::Estimate { cmd } => match cmd {
            EstimateCmd::Set {
                dollars,
                turns,
                tokens,
                wall_clock_ms,
                args,
            } => estimatecli::set(&root, dollars, turns, tokens, wall_clock_ms, &args.opts())?,
            EstimateCmd::Actual { json, args } => {
                if json {
                    estimatecli::actual_json(&root, &args.opts())?
                } else {
                    estimatecli::actual(&root, &args.opts())?
                }
            }
            EstimateCmd::Retro { args } => estimatecli::retro_cmd(&root, &args.opts())?,
        },
        Cmd::Approve { name, by } => {
            let conn = open(&root)?;
            packages::decide(&root, &conn, &name, true, &by)?;
            // Non-refusing dependency nudge (docs/handoffs/package-dependencies.md
            // M4): the approve already happened; if this package declares deps
            // that are not yet installed/approved, print the M3-shaped fix line so
            // "remember to also approve phonebook" is no longer a remembered step.
            for line in packages::unmet_dep_nudges(&root, &conn, &name)? {
                println!("{line}");
            }
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
        Cmd::Ask {
            question,
            options,
            deadline_minutes,
            default,
        } => {
            let conn = open(&root)?;
            human::ask(
                &root,
                &conn,
                &question,
                options.as_deref(),
                deadline_minutes,
                default.as_deref(),
            )?;
        }
        Cmd::Bus { cmd } => match cmd {
            BusCmd::Pub {
                topic,
                payload,
                qos,
                retain,
                correlation,
            } => {
                buscli::publish(
                    &root,
                    &topic,
                    payload.as_deref(),
                    qos,
                    retain,
                    correlation.as_deref(),
                )?;
            }
            BusCmd::Sub {
                filter,
                count,
                timeout,
                blocking,
                order,
                timeout_ms,
                on_timeout,
                phase,
                point,
            } => {
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
        Cmd::Code { args } => {
            // M2 (model-providers): pull the lanius-level `--provider <name>` that
            // sits BEFORE the tool token (`lanius code --provider deepseek claude …`)
            // so it never collides with forwarded tool args. Absent ⇒ argv unchanged
            // (byte-identical to today). Consumed by launch/spawn; ignored by the
            // other code verbs.
            let (provider, args) = codeagent::take_provider_flag(&args)?;
            let provider = provider.as_deref();
            let tool = args.first().map(String::as_str).unwrap_or("");
            let rest = args.get(1..).unwrap_or(&[]);
            // Reserved first words: `hook` is the internal hook bridge
            // (`lanius code hook <Event>`); `resume` continues a recorded session
            // (`lanius code resume <elanus_session> "<message>"`). Any other first
            // word is a coding-tool adapter to launch.
            match tool {
                "" | "help" | "-h" | "--help" => {
                    codeagent::print_help();
                }
                "list" => {
                    codeagent::print_tools();
                }
                "hook" => {
                    let event = rest.first().map(String::as_str).unwrap_or("");
                    codeagent::hook(&root, event)?;
                }
                "resume" => {
                    // `resume` is NOT an lanius verb. Re-attaching interactively is
                    // a normal managed launch with the tool's own resume flag passed
                    // through; the daemon's async resume is the in-process
                    // `resume_capture` primitive, never a human command. Redirect a
                    // muscle-memory `lanius code resume <id>` to the real form rather
                    // than silently treating "resume" as a tool name.
                    let session = rest.first().map(String::as_str).unwrap_or("");
                    let hint = codeagent::session_resume_hint(&root, session);
                    anyhow::bail!(
                        "`lanius code resume` is not a command. To re-attach to a \
                         session, launch its tool with the tool's own resume flag, \
                         e.g. `lanius code claude --resume <native_session>` (run in \
                         the session's workdir).{hint}\n\
                         Find the exact command with `lanius code session <id>`."
                    );
                }
                "deliver" => {
                    // A planner dispatches work to a worker (M4-B). Run from inside
                    // a coding session; records the running session as the requester
                    // so the worker's completion routes back (M4-A).
                    let worker = rest.first().map(String::as_str).unwrap_or("");
                    if worker.is_empty() {
                        anyhow::bail!("usage: lanius code deliver <worker-session> \"<message>\"");
                    }
                    let message = rest.get(1..).unwrap_or(&[]).join(" ");
                    codeagent::deliver(&root, worker, &message)?;
                }
                "send" => {
                    // A coding session sends a non-blocking message to the human
                    // owner's chat. Identity comes from LANIUS_CODE_SESSION, never
                    // an argument, so a session can only speak as itself.
                    let mut corr: Option<String> = None;
                    let mut words: Vec<String> = Vec::new();
                    let mut i = 0;
                    while i < rest.len() {
                        match rest[i].as_str() {
                            "--corr" => {
                                i += 1;
                                let value = rest.get(i).map(String::as_str).unwrap_or("").trim();
                                if value.is_empty() {
                                    anyhow::bail!(
                                        "usage: lanius code send \"<message>\" [--corr <id>]"
                                    );
                                }
                                corr = Some(value.to_string());
                            }
                            other => words.push(other.to_string()),
                        }
                        i += 1;
                    }
                    let message = words.join(" ");
                    if message.trim().is_empty() {
                        anyhow::bail!("usage: lanius code send \"<message>\" [--corr <id>]");
                    }
                    codeagent::send(&root, &message, corr.as_deref())?;
                }
                "spawn" => {
                    // A planner creates a new worker in the background. The child
                    // wrapper gets a pre-generated worker session id and a reply
                    // route to this session's mailbox, then this command returns
                    // immediately so the planner can end its turn.
                    let worker_tool = rest.first().map(String::as_str).unwrap_or("");
                    if worker_tool.is_empty() {
                        anyhow::bail!("usage: lanius code spawn <tool> \"<task>\"");
                    }
                    let prompt = rest.get(1..).unwrap_or(&[]).join(" ");
                    codeagent::spawn(&root, worker_tool, &prompt, provider)?;
                }
                "inbox" => {
                    // A session pulls its OWN inbox (M3). Identity comes from the
                    // env the launcher set (LANIUS_CODE_SESSION/AGENT) — never an
                    // arg — so it can only ever read its own mailbox. Flags: --all
                    // (full inbox, non-destructive), --json (machine-readable).
                    codeagent::inbox_cmd(&root, rest)?;
                }
                "mail" => {
                    // The human-facing projection of agent-to-agent message
                    // traffic (agent-comms-ui M1). `lanius code mail [--json]
                    // [--limit N]`. A pure ledger read over `in/agent/%` events,
                    // threaded by correlation, failure-mail flagged. Backs the web
                    // /api/comms/mail route.
                    mailcli::mail_cmd(&root, rest)?;
                }
                "blocks" => {
                    // The human-facing memory-block inspector projection
                    // (agent-comms-ui M4, read-only). `lanius code blocks
                    // --session <code-id> [--json]`. Durable blocks (context_blocks,
                    // keyed by the session's agent noun) + recomputed ephemeral
                    // inbox/channel blocks (never stored). Backs /api/blocks.
                    mailcli::blocks_cmd(&root, rest)?;
                }
                "rooms" => {
                    // The human-facing projection of coordination rooms
                    // (agent-comms-ui M3). `lanius code rooms [--json] [--recent
                    // N]`. Roster (liveness honest), claims, and recent channel
                    // traffic per room. Backs the web /api/comms/rooms route.
                    mailcli::rooms_cmd(&root, rest)?;
                }
                "note" => {
                    // Leave a per-session memory note (M3), surfaced by the per-turn
                    // injection. `lanius code note <session> "<text>"`; empty text
                    // clears it.
                    let session = rest.first().map(String::as_str).unwrap_or("");
                    if session.is_empty() {
                        anyhow::bail!(
                            "usage: lanius code note <session> \"<text>\"  (empty text clears the note)"
                        );
                    }
                    let text = rest.get(1..).unwrap_or(&[]).join(" ");
                    codeagent::note_cmd(&root, session, &text)?;
                }
                "claim" => {
                    // Announce an advisory edit claim (M5). Run from inside a
                    // session; identity + room are env/record-derived, so it can
                    // only claim as itself in its own room. `lanius code claim <path>`.
                    let path = rest.first().map(String::as_str).unwrap_or("");
                    if path.is_empty() {
                        anyhow::bail!("usage: lanius code claim <path>");
                    }
                    codeagent::claim_cmd(&root, path)?;
                }
                "unclaim" => {
                    // Release this session's advisory claim on a path (M5).
                    // `lanius code unclaim <path>`.
                    let path = rest.first().map(String::as_str).unwrap_or("");
                    if path.is_empty() {
                        anyhow::bail!("usage: lanius code unclaim <path>");
                    }
                    codeagent::unclaim_cmd(&root, path)?;
                }
                "claims" => {
                    // Show this session's room coordination view (own + peer
                    // claims), M5. `lanius code claims [--json]`.
                    codeagent::claims_cmd(&root, rest)?;
                }
                "whose" => {
                    // SI4 (sibling-intent): change attribution. `lanius code whose
                    // <path>` or `lanius code whose --dirty [--json]` — map a path
                    // (or the whole `git status` set) to its owning session, that
                    // session's tool, last-active, and current task. Backed by the
                    // `code_claims` projection (codesession::whose_path).
                    codeagent::whose_cmd(&root, rest)?;
                }
                "ask" => {
                    // sibling-resolution: a blocking deliver-and-wait. `lanius code
                    // ask <session> "<question>" [--timeout SECS] [--priority N]` —
                    // send a scoped question to a live sibling and block briefly for
                    // the correlated reply (or "no answer — treat as theirs"). Thin
                    // over the shipped deliver/inbox/correlation rails.
                    codeagent::ask_cmd(&root, rest)?;
                }
                "sitrep" => {
                    // Situational-awareness M4: ONE view accounting for every
                    // coding session AND every loose worktree/branch — intent,
                    // liveness, workdir/branch, and derived outcome (active |
                    // merged | abandoned | wip-stranded). Replaces git archaeology.
                    // `lanius code sitrep [--json]`.
                    codeagent::sitrep_cmd(&root, rest)?;
                }
                "watch" => {
                    // Situational-awareness M5: tail a READABLE digest of a
                    // session's live obs (assistant messages + tool calls
                    // summarized). "Spy on what it's doing" in one command.
                    // `lanius code watch <session> [--count N] [--timeout SECS]`.
                    codeagent::watch_cmd(&root, rest)?;
                }
                "project" => {
                    // Observability: run the trace->sqlite projection once now
                    // (the daemon also does this each tick). Useful to refresh the
                    // projection manually or before querying on a non-daemon build.
                    let n = code_projection::project_trace(&root)?;
                    println!("projected {n} new coding-session event(s)");
                }
                "sessions" => {
                    // Observability M2: list coding sessions from the projection.
                    // `lanius code sessions [--json]`. Reads the derived sqlite
                    // projection (code_projection); empty until the daemon (this
                    // build) has projected at least once.
                    let want_json = rest.iter().any(|a| a == "--json");
                    // Grouped by default (one row per native thread; manual
                    // --resume relaunches fold into one). --raw/--ungrouped emits
                    // the per-incarnation rows (the old behaviour / debugging).
                    let want_raw = rest.iter().any(|a| a == "--raw" || a == "--ungrouped");
                    let sessions = if want_raw {
                        code_projection::list_sessions_raw(&root)?
                    } else {
                        code_projection::list_sessions(&root)?
                    };
                    if want_json {
                        println!("{}", serde_json::to_string_pretty(&sessions)?);
                    } else if sessions.is_empty() {
                        println!(
                            "(no coding sessions projected yet — is the daemon running this build?)"
                        );
                    } else {
                        for s in &sessions {
                            let dur = s
                                .duration_ms
                                .map(|m| format!("{}s", m / 1000))
                                .unwrap_or_else(|| "—".into());
                            println!(
                                "{}  {:<6}  {}/{}  {:<7}  {:>6}  in:{} out:{}",
                                s.elanus_session,
                                s.tool.as_deref().unwrap_or("?"),
                                s.model.as_deref().unwrap_or("?"),
                                s.effort.as_deref().unwrap_or("?"),
                                s.last_status.as_deref().unwrap_or("?"),
                                dur,
                                s.input_tokens,
                                s.output_tokens,
                            );
                        }
                    }
                }
                "session" => {
                    // Observability M2: one session's detail (stats, timeline,
                    // resume command, children). `lanius code session <id> [--json]`.
                    // Thread-grouping (TG1): <id> may be any incarnation's lanius
                    // id OR a native thread_key; the detail shows the UNIONED
                    // thread timeline across all incarnations by default. The
                    // richer per-incarnation web rendering (TG3 CodeSessions.tsx
                    // expandable incarnations row) is DEFERRED — the --json wire is
                    // a backward-compatible superset so the existing UI renders the
                    // grouped threads unchanged.
                    let id = rest.first().map(String::as_str).unwrap_or("");
                    if id.is_empty() {
                        anyhow::bail!("usage: lanius code session <id> [--json]");
                    }
                    let want_json = rest.iter().any(|a| a == "--json");
                    match code_projection::session_detail(&root, id)? {
                        None if want_json => println!("null"),
                        None => println!("no such coding session {id:?}"),
                        Some(detail) if want_json => {
                            println!("{}", serde_json::to_string_pretty(&detail)?)
                        }
                        Some(detail) => {
                            let s = &detail.session;
                            println!(
                                "session {}  ({}/{}, {})",
                                s.elanus_session,
                                s.tool.as_deref().unwrap_or("?"),
                                s.model.as_deref().unwrap_or("?"),
                                s.last_status.as_deref().unwrap_or("?"),
                            );
                            match &detail.resume_command {
                                Some(cmd) => println!("  resume: {cmd}"),
                                None => {
                                    println!("  resume: (no managed passthrough for this tool)")
                                }
                            }
                            if !detail.children.is_empty() {
                                println!("  children:");
                                for c in &detail.children {
                                    println!(
                                        "    {} ({})",
                                        c.elanus_session,
                                        c.tool.as_deref().unwrap_or("?")
                                    );
                                }
                            }
                            println!("  timeline ({} events):", detail.events.len());
                            for e in &detail.events {
                                println!(
                                    "    {} {}",
                                    e.ts.as_deref().unwrap_or("?"),
                                    e.kind.as_deref().unwrap_or("?")
                                );
                            }
                        }
                    }
                }
                _ => {
                    codeagent::launch(&root, tool, rest, provider)?;
                }
            }
        }
        Cmd::Events { limit } => {
            let conn = open(&root)?;
            let mut stmt = conn.prepare(
                "SELECT id, type, state, cause_id, correlation_id, substr(COALESCE(payload,''),1,60), created_at
                 FROM events ORDER BY id DESC LIMIT ?1",
            )?;
            let rows: Vec<(
                i64,
                String,
                String,
                Option<i64>,
                Option<String>,
                String,
                String,
            )> = stmt
                .query_map([limit], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
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

fn emit_agent_config_changed(root: &paths::Root, name: &str, sha: Option<&str>) -> Result<()> {
    let Some(sha) = sha else {
        return Ok(());
    };
    let conn = open(root)?;
    let by = secrets::owner_name(root);
    events::emit(
        root,
        &conn,
        events::EmitOpts {
            payload: Some(serde_json::json!({
                "agent": name,
                "commit": sha,
                "decided_by": by,
            })),
            sender: Some(by),
            ..events::EmitOpts::new("obs/config/changed")
        },
    )?;
    Ok(())
}

fn parse_json_opt(s: Option<&str>) -> Result<Option<Value>> {
    match s {
        None => Ok(None),
        Some(s) => Ok(Some(
            serde_json::from_str(s).map_err(|e| anyhow::anyhow!("invalid JSON {s:?}: {e}"))?,
        )),
    }
}

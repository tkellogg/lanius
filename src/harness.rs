use crate::codeagent::{self, Mode};
use crate::codesession::{self, InboxItem};
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const ENV_ROOT: &str = "LANIUS_ROOT";
pub const ENV_BUS_TOKEN: &str = "LANIUS_BUS_TOKEN";
pub const ENV_WORKDIR: &str = "LANIUS_CODE_WORKDIR";
pub const ENV_MODE: &str = "LANIUS_CODE_MODE";
pub const ENV_TOOL: &str = "LANIUS_CODE_TOOL";
pub const ENV_MODEL: &str = "LANIUS_CODE_MODEL";
pub const ENV_PROVIDER: &str = "LANIUS_CODE_PROVIDER";
pub const ENV_SUMMARY_FILE: &str = "LANIUS_CODE_SUMMARY_FILE";
pub const ENV_PROMPT: &str = "LANIUS_CODE_PROMPT";
/// The FULL post-lanius-flags argv the user passed (JSON array). Real harness
/// adapters pass this verbatim to their capture fn, which knows how to split the
/// harness's own flags (e.g. codex `-c …`) from the prompt — joining it into a
/// single `ENV_PROMPT` string loses that distinction.
pub const ENV_ARGS: &str = "LANIUS_CODE_ARGS";
pub const ENV_BRIEFING: &str = "LANIUS_CODE_BRIEFING";
pub const ENV_SKILLS_DIR: &str = "LANIUS_CODE_SKILLS_DIR";
/// The human owner's noun (profile `owner`): where a codex app-server driver
/// routes approval elicitations (`in/human/<owner>`, docs/handoffs/codex-app-server.md
/// M3). Absent ⇒ the default owner.
pub const ENV_OWNER: &str = "LANIUS_CODE_OWNER";
/// Per-launch gate for the codex app-server transport (docs/handoffs/codex-app-server.md
/// M4). Set to `1` by the launcher ONLY for a headless codex worker whose profile
/// (or a `--app-server` launch flag) opted in; absent ⇒ the `codex exec` fallback.
pub const ENV_CODEX_APP_SERVER: &str = "LANIUS_CODE_CODEX_APP_SERVER";
/// The app-server approval elicitation deadline (seconds) + fail-closed default,
/// threaded from the profile's `[codex]` table so the adapter need not re-read it.
pub const ENV_CODEX_AS_TIMEOUT: &str = "LANIUS_CODE_CODEX_APP_SERVER_TIMEOUT";
pub const ENV_CODEX_AS_DEFAULT: &str = "LANIUS_CODE_CODEX_APP_SERVER_DEFAULT";

/// The session context lanius hands an adapter. Built from the launch-contract env.
#[derive(Clone, Debug)]
pub struct Ctx {
    root: Root,
    session: String,
    agent_noun: String,
    bus_token: Option<String>,
    workdir: PathBuf,
    mode: Mode,
    args: Vec<String>,
    tool: String,
    model: Option<String>,
    provider: Option<String>,
    summary_file: Option<PathBuf>,
    prompt: Option<String>,
    briefing: Option<String>,
    skills_dir: Option<PathBuf>,
    owner: Option<String>,
    codex_app_server: bool,
    codex_as_timeout: Option<u64>,
    codex_as_default: Option<String>,
}

impl Ctx {
    /// Read the launch contract from env.
    pub fn from_env() -> Result<Ctx> {
        let root = Root {
            dir: env_required(ENV_ROOT)?.into(),
        };
        let session = env_required(codeagent::ENV_SESSION)?;
        let agent_noun = env_required(codeagent::ENV_AGENT)?;
        let bus_token = env_optional(ENV_BUS_TOKEN);
        let workdir = match env_optional(ENV_WORKDIR) {
            Some(workdir) => PathBuf::from(workdir),
            None => std::env::current_dir().context("resolving current directory")?,
        };
        let mode = match env_optional(ENV_MODE) {
            Some(mode) => parse_mode(&mode)?,
            None => Mode::Headless,
        };
        let args = env_optional(ENV_ARGS)
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default();
        let tool = env_optional(ENV_TOOL).unwrap_or_else(|| agent_noun.clone());
        let model = env_optional(ENV_MODEL);
        let provider = env_optional(ENV_PROVIDER);
        let summary_file = env_optional(ENV_SUMMARY_FILE).map(PathBuf::from);
        let prompt = env_optional(ENV_PROMPT);
        let briefing = env_optional(ENV_BRIEFING);
        let skills_dir = env_optional(ENV_SKILLS_DIR).map(PathBuf::from);
        let owner = env_optional(ENV_OWNER);
        let codex_app_server = env_optional(ENV_CODEX_APP_SERVER)
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let codex_as_timeout = env_optional(ENV_CODEX_AS_TIMEOUT).and_then(|v| v.parse().ok());
        let codex_as_default = env_optional(ENV_CODEX_AS_DEFAULT);

        Ok(Ctx {
            root,
            session,
            agent_noun,
            bus_token,
            workdir,
            mode,
            args,
            tool,
            model,
            provider,
            summary_file,
            prompt,
            briefing,
            skills_dir,
            owner,
            codex_app_server,
            codex_as_timeout,
            codex_as_default,
        })
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn root(&self) -> &Root {
        &self.root
    }

    pub fn agent_noun(&self) -> &str {
        &self.agent_noun
    }

    pub fn bus_token(&self) -> Option<&str> {
        self.bus_token.as_deref()
    }

    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The full post-lanius-flags argv (harness flags + prompt). Real adapters pass
    /// this to their capture fn, which splits the harness's flags from the prompt.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn summary_file(&self) -> Option<&Path> {
        self.summary_file.as_deref()
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub fn briefing(&self) -> Option<&str> {
        self.briefing.as_deref()
    }

    pub fn skills_dir(&self) -> Option<&Path> {
        self.skills_dir.as_deref()
    }

    /// The human owner's noun (where a codex app-server driver routes approval
    /// elicitations). Falls back to the default owner when the launcher did not
    /// stamp one.
    pub fn owner(&self) -> &str {
        self.owner.as_deref().unwrap_or("owner")
    }

    /// Whether this headless codex launch opted into the app-server transport
    /// (docs/handoffs/codex-app-server.md M4). Default false ⇒ `codex exec`.
    pub fn codex_app_server(&self) -> bool {
        self.codex_app_server
    }

    /// The app-server approval elicitation deadline (seconds); `None` ⇒ the
    /// driver's built-in default.
    pub fn codex_as_timeout(&self) -> Option<u64> {
        self.codex_as_timeout
    }

    /// The app-server fail-closed default on a timed-out approval; `None` ⇒ the
    /// driver's built-in default (`deny`).
    pub fn codex_as_default(&self) -> Option<&str> {
        self.codex_as_default.as_deref()
    }

    /// Publish an observation: obs/agent/<noun>/<session>/<leaf>.
    pub fn emit(&self, leaf: &str, body: Value) {
        if let Some(bus_token) = self.bus_token.as_deref() {
            codeagent::publish_obs(
                &self.root,
                &self.session,
                bus_token,
                &codeagent::obs_topic(&self.agent_noun, &self.session, leaf),
                body,
            );
        }
    }

    /// Advisory edit-claim for a path this session wrote.
    pub fn claim(&self, path: &str) {
        let cwd = self.workdir.to_str();
        codeagent::auto_claim_write(&self.root, &self.session, path, cwd);
    }

    /// Persist the durable resume record.
    pub fn record(&self, native_session_id: &str) {
        let rec = codesession::SessionRecord {
            elanus_session: self.session.clone(),
            native_session: native_session_id.to_string(),
            tool: self.tool.clone(),
            agent_noun: self.agent_noun.clone(),
            workdir: self.workdir.to_string_lossy().into_owned(),
            room: None,
        };
        let _ = codesession::upsert_record(&self.root, &rec);
    }

    /// Keep last_active fresh.
    pub fn bump_active(&self) {
        let _ = codesession::bump_last_active(&self.root, &self.session);
    }

    /// Read this session's inbox.
    pub fn inbox(&self) -> Result<Vec<InboxItem>> {
        codesession::inbox_for_session(&self.root, &self.agent_noun, &self.session, false)
    }

    /// Send to another coding session.
    pub fn deliver(&self, to: &str, message: &str) -> Result<()> {
        std::env::set_var(codeagent::ENV_SESSION, &self.session);
        codeagent::deliver(&self.root, to, message)
    }

    /// Scrub lanius's provider creds from a child Command.
    pub fn scrub_provider_creds<'a>(
        &self,
        cmd: &'a mut std::process::Command,
    ) -> &'a mut std::process::Command {
        codeagent::scrub_provider_creds(cmd)
    }
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name)
        .with_context(|| format!("missing required lanius harness env var {name}"))
        .and_then(|v| {
            if v.is_empty() {
                bail!("missing required lanius harness env var {name}")
            } else {
                Ok(v)
            }
        })
}

fn env_optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn parse_mode(raw: &str) -> Result<Mode> {
    match raw {
        "tui" => Ok(Mode::Tui),
        "headless" => Ok(Mode::Headless),
        other => bail!("invalid {ENV_MODE} {other:?}: expected \"tui\" or \"headless\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, OnceLock,
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn tmp_root() -> Root {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lanius-harness-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    #[test]
    fn ctx_from_env_reads_launch_contract() {
        let _guard = env_lock().lock().unwrap();
        let vars = [
            ENV_ROOT,
            codeagent::ENV_SESSION,
            codeagent::ENV_AGENT,
            ENV_BUS_TOKEN,
            ENV_WORKDIR,
            ENV_MODE,
            ENV_TOOL,
            ENV_MODEL,
            ENV_PROVIDER,
            ENV_SUMMARY_FILE,
            ENV_PROMPT,
            ENV_BRIEFING,
            ENV_SKILLS_DIR,
        ];
        let saved: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();

        std::env::set_var(ENV_ROOT, "/tmp/lanius-root");
        std::env::set_var(codeagent::ENV_SESSION, "code-test1234");
        std::env::set_var(codeagent::ENV_AGENT, "test-agent");
        std::env::set_var(ENV_BUS_TOKEN, "bus-secret");
        std::env::set_var(ENV_WORKDIR, "/tmp/workdir");
        std::env::set_var(ENV_MODE, "headless");
        std::env::set_var(ENV_TOOL, "test-tool");
        std::env::set_var(ENV_PROMPT, "do the task");
        std::env::set_var(ENV_BRIEFING, "briefing text");
        std::env::set_var(ENV_SKILLS_DIR, "/tmp/skills");

        let ctx = Ctx::from_env().unwrap();

        assert_eq!(ctx.session(), "code-test1234");
        assert_eq!(ctx.agent_noun(), "test-agent");
        assert_eq!(ctx.tool(), "test-tool");
        assert_eq!(ctx.workdir(), Path::new("/tmp/workdir"));
        assert_eq!(ctx.mode(), Mode::Headless);
        assert_eq!(ctx.prompt(), Some("do the task"));
        assert_eq!(ctx.briefing(), Some("briefing text"));
        assert_eq!(ctx.skills_dir(), Some(Path::new("/tmp/skills")));

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn ctx_record_uses_tool_distinct_from_agent_noun() {
        let _guard = env_lock().lock().unwrap();
        let root = tmp_root();
        let workdir = root.dir.join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let root_s = root.dir.to_string_lossy().into_owned();
        let workdir_s = workdir.to_string_lossy().into_owned();
        let vars = [
            ENV_ROOT,
            codeagent::ENV_SESSION,
            codeagent::ENV_AGENT,
            ENV_BUS_TOKEN,
            ENV_WORKDIR,
            ENV_MODE,
            ENV_TOOL,
            ENV_MODEL,
            ENV_PROVIDER,
            ENV_SUMMARY_FILE,
            ENV_PROMPT,
            ENV_BRIEFING,
            ENV_SKILLS_DIR,
        ];
        let saved: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();

        std::env::set_var(ENV_ROOT, &root_s);
        std::env::set_var(codeagent::ENV_SESSION, "code-record01");
        std::env::set_var(codeagent::ENV_AGENT, "codex-adapter");
        std::env::remove_var(ENV_BUS_TOKEN);
        std::env::set_var(ENV_WORKDIR, &workdir_s);
        std::env::set_var(ENV_MODE, "headless");
        std::env::set_var(ENV_TOOL, "codex");
        std::env::remove_var(ENV_PROMPT);
        std::env::remove_var(ENV_BRIEFING);
        std::env::remove_var(ENV_SKILLS_DIR);

        let ctx = Ctx::from_env().unwrap();
        ctx.record("native-thread-1");

        let rec = codesession::read_record(&root, "code-record01")
            .unwrap()
            .unwrap();
        assert_eq!(rec.tool, "codex");
        assert_eq!(rec.agent_noun, "codex-adapter");

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let _ = std::fs::remove_dir_all(&root.dir);
    }
}

use crate::codeagent::{self, Mode};
use crate::codesession::{self, InboxItem};
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const ENV_ROOT: &str = "ELANUS_ROOT";
pub const ENV_BUS_TOKEN: &str = "ELANUS_BUS_TOKEN";
pub const ENV_WORKDIR: &str = "ELANUS_CODE_WORKDIR";
pub const ENV_MODE: &str = "ELANUS_CODE_MODE";
pub const ENV_TOOL: &str = "ELANUS_CODE_TOOL";
pub const ENV_PROMPT: &str = "ELANUS_CODE_PROMPT";
pub const ENV_BRIEFING: &str = "ELANUS_CODE_BRIEFING";
pub const ENV_SKILLS_DIR: &str = "ELANUS_CODE_SKILLS_DIR";

/// The session context elanus hands an adapter. Built from the launch-contract env.
#[derive(Clone, Debug)]
pub struct Ctx {
    root: Root,
    session: String,
    agent_noun: String,
    bus_token: Option<String>,
    workdir: PathBuf,
    mode: Mode,
    tool: String,
    prompt: Option<String>,
    briefing: Option<String>,
    skills_dir: Option<PathBuf>,
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
        let tool = env_optional(ENV_TOOL).unwrap_or_else(|| agent_noun.clone());
        let prompt = env_optional(ENV_PROMPT);
        let briefing = env_optional(ENV_BRIEFING);
        let skills_dir = env_optional(ENV_SKILLS_DIR).map(PathBuf::from);

        Ok(Ctx {
            root,
            session,
            agent_noun,
            bus_token,
            workdir,
            mode,
            tool,
            prompt,
            briefing,
            skills_dir,
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

    /// Scrub elanus's provider creds from a child Command.
    pub fn scrub_provider_creds<'a>(
        &self,
        cmd: &'a mut std::process::Command,
    ) -> &'a mut std::process::Command {
        codeagent::scrub_provider_creds(cmd)
    }
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name)
        .with_context(|| format!("missing required elanus harness env var {name}"))
        .and_then(|v| {
            if v.is_empty() {
                bail!("missing required elanus harness env var {name}")
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
            "elanus-harness-{}-{}",
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
            ENV_PROMPT,
            ENV_BRIEFING,
            ENV_SKILLS_DIR,
        ];
        let saved: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();

        std::env::set_var(ENV_ROOT, "/tmp/elanus-root");
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

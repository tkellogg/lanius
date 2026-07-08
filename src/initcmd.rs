use crate::db;
use crate::kit;
use crate::manifest::ThrottleDecl;
use crate::packages;
use crate::paths::Root;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

struct PkgFile {
    rel: &'static str,
    content: &'static str,
    exec: bool,
}

/// Packages shipped with the binary. Real source of truth is the repo's
/// packages/ dir; init materializes copies into the harness root so the root
/// is self-contained and the user can edit/fork them freely.
const PKG_FILES: &[PkgFile] = &[
    PkgFile {
        rel: "chat/lanius.toml",
        content: include_str!("../packages/chat/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "chat/scripts/run",
        content: include_str!("../packages/chat/scripts/run"),
        exec: true,
    },
    PkgFile {
        rel: "echo/lanius.toml",
        content: include_str!("../packages/echo/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "echo/scripts/echo",
        content: include_str!("../packages/echo/scripts/echo"),
        exec: true,
    },
    PkgFile {
        rel: "notify/lanius.toml",
        content: include_str!("../packages/notify/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "notify/scripts/notify",
        content: include_str!("../packages/notify/scripts/notify"),
        exec: true,
    },
    PkgFile {
        rel: "watchdog/lanius.toml",
        content: include_str!("../packages/watchdog/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "watchdog/scripts/scan",
        content: include_str!("../packages/watchdog/scripts/scan"),
        exec: true,
    },
    PkgFile {
        rel: "notes/SKILL.md",
        content: include_str!("../packages/notes/SKILL.md"),
        exec: false,
    },
    // Ships pending, NOT auto-approved below: an approved stage shapes every
    // prompt; activating it is the human's call (lanius approve recent-history).
    PkgFile {
        rel: "recent-history/lanius.toml",
        content: include_str!("../packages/recent-history/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "recent-history/scripts/main",
        content: include_str!("../packages/recent-history/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "window/lanius.toml",
        content: include_str!("../packages/window/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "window/scripts/stage",
        content: include_str!("../packages/window/scripts/stage"),
        exec: true,
    },
    // Ships pending like the other stages: an approved stage shapes every
    // prompt, so activating the platform block is the human's call
    // (lanius approve platform). docs/handoffs/platform-trust.md M3.
    PkgFile {
        rel: "platform/lanius.toml",
        content: include_str!("../packages/platform/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "platform/scripts/main",
        content: include_str!("../packages/platform/scripts/main"),
        exec: true,
    },
];

struct StockHarnessPackage {
    dir: &'static str,
    binary: &'static str,
    manifest: &'static str,
}

const STOCK_HARNESS_PACKAGES: &[StockHarnessPackage] = &[
    StockHarnessPackage {
        dir: "harness-claude",
        binary: "harness-claude",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"claude\"\n",
            "aliases = [\"cc\"]\n",
            "agent_noun = \"claude-code\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
    StockHarnessPackage {
        dir: "harness-codex",
        binary: "harness-codex",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"codex\"\n",
            "agent_noun = \"codex\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
    StockHarnessPackage {
        dir: "harness-opencode",
        binary: "harness-opencode",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"opencode\"\n",
            "agent_noun = \"opencode\"\n",
            "run = \"bin/adapter\"\n",
        ),
    },
    // The generic ACP package (docs/handoffs/acp-harness.md A4). ONE adapter bin,
    // one block per known ACP agent — the only per-agent difference is `command`
    // + `args`, stamped into LANIUS_ACP_ARGV by the launcher. Adding the next ACP
    // agent is appending a block here (a manifest-only edit — no new binary). Give
    // an agent MCP servers with an `mcp` array on its block, e.g.:
    //   [[harness.mcp]]  (or inline)  name="fs" command="mcp-fs" args=["--root","."]
    StockHarnessPackage {
        dir: "harness-acp",
        binary: "harness-acp",
        manifest: concat!(
            "[[harness]]\n",
            "name = \"goose\"\n",
            "agent_noun = \"goose\"\n",
            "run = \"bin/adapter\"\n",
            "command = \"goose\"\n",
            "args = [\"acp\"]\n",
            "\n",
            "[[harness]]\n",
            "name = \"gemini\"\n",
            "agent_noun = \"gemini\"\n",
            "run = \"bin/adapter\"\n",
            "command = \"gemini\"\n",
            "args = [\"--experimental-acp\"]\n",
            "\n",
            "[[harness]]\n",
            "name = \"codex-acp\"\n",
            "agent_noun = \"codex-acp\"\n",
            "run = \"bin/adapter\"\n",
            "command = \"codex-acp\"\n",
            "args = []\n",
        ),
    },
];

/// ALL stock kits, seeded into <root>/kits so every root has the
/// out-of-the-box set resolvable — no env var, no repo checkout. The kit
/// dir is the config; these are just its defaults (write_if_missing, so
/// edits and deletions stick).
const STOCK_KIT_FILES: &[PkgFile] = &[
    PkgFile {
        rel: "core/README.md",
        content: include_str!("../kits/core/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/harness-doctrine/SKILL.md",
        content: include_str!("../kits/core/packages/harness-doctrine/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/self-modify/SKILL.md",
        content: include_str!("../kits/core/packages/self-modify/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/escalate/SKILL.md",
        content: include_str!("../kits/core/packages/escalate/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/self-scheduling/SKILL.md",
        content: include_str!("../kits/core/packages/self-scheduling/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/sibling-coordination/SKILL.md",
        content: include_str!("../kits/core/packages/sibling-coordination/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/sibling-coordination/lanius.toml",
        content: include_str!("../kits/core/packages/sibling-coordination/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/profiles/architect/profile.toml",
        content: include_str!("../kits/core/profiles/architect/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/profiles/architect/blocks/00-architect.md",
        content: include_str!("../kits/core/profiles/architect/blocks/00-architect.md"),
        exec: false,
    },
    // The seeded KB pointer block (kb-core.md M3): its JSON frontmatter carries
    // `meta = {kb,path,lines,sha}` into kb-llm-strengths/kb/role-verifier.md, so
    // the dispatching architect surfaces the model-tiering pointer.
    PkgFile {
        rel: "core/profiles/architect/blocks/10-kb-llm-strengths.md",
        content: include_str!("../kits/core/profiles/architect/blocks/10-kb-llm-strengths.md"),
        exec: false,
    },
    // kb-groundskeeper — the KB's caretaker (docs/handoffs/kb-groundskeeper.md):
    // a no-LLM sweep cron (pointers/orphans/staleness → owner report) plus the
    // setup-gated diff pipeline. Ships in core, pending; `lanius approve` turns on
    // rung 1, the [config] keys + approve gate rung 2.
    PkgFile {
        rel: "core/packages/kb-groundskeeper/lanius.toml",
        content: include_str!("../kits/core/packages/kb-groundskeeper/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/kb-groundskeeper/scripts/dispatch",
        content: include_str!("../kits/core/packages/kb-groundskeeper/scripts/dispatch"),
        exec: true,
    },
    PkgFile {
        rel: "core/packages/kb-groundskeeper/SKILL.md",
        content: include_str!("../kits/core/packages/kb-groundskeeper/SKILL.md"),
        exec: false,
    },
    // kb-pipeline — the exec handler that makes the compactor/ratifier agent
    // mailboxes daemon-drivable (docs/handoffs/kb-groundskeeper.md M3). Without an
    // approved exec package subscribing to in/agent/kb-compactor / in/agent/kb-ratifier,
    // `spawn_core` refuses to launch and `lanius kb groundskeep` cannot spawn the
    // compactor. Mirrors packages/chat; ships pending, approved as part of setup.
    PkgFile {
        rel: "core/packages/kb-pipeline/lanius.toml",
        content: include_str!("../kits/core/packages/kb-pipeline/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/packages/kb-pipeline/scripts/run",
        content: include_str!("../kits/core/packages/kb-pipeline/scripts/run"),
        exec: true,
    },
    // The compactor + ratifier profiles the diff pipeline spawns (M3).
    PkgFile {
        rel: "core/profiles/kb-compactor/profile.toml",
        content: include_str!("../kits/core/profiles/kb-compactor/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "core/profiles/kb-ratifier/profile.toml",
        content: include_str!("../kits/core/profiles/kb-ratifier/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "dev/README.md",
        content: include_str!("../kits/dev/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "dev/packages/git-protect/lanius.toml",
        content: include_str!("../kits/dev/packages/git-protect/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "dev/packages/git-protect/scripts/gate",
        content: include_str!("../kits/dev/packages/git-protect/scripts/gate"),
        exec: true,
    },
    PkgFile {
        rel: "dev/profiles/dev/profile.toml",
        content: include_str!("../kits/dev/profiles/dev/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/README.md",
        content: include_str!("../kits/funnel/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-intake/lanius.toml",
        content: include_str!("../kits/funnel/packages/funnel-intake/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-intake/scripts/main",
        content: include_str!("../kits/funnel/packages/funnel-intake/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/lanius.toml",
        content: include_str!("../kits/funnel/packages/funnel-sift/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/rules.txt",
        content: include_str!("../kits/funnel/packages/funnel-sift/rules.txt"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-sift/scripts/sift",
        content: include_str!("../kits/funnel/packages/funnel-sift/scripts/sift"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/packages/funnel-scout/lanius.toml",
        content: include_str!("../kits/funnel/packages/funnel-scout/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/packages/funnel-scout/scripts/run",
        content: include_str!("../kits/funnel/packages/funnel-scout/scripts/run"),
        exec: true,
    },
    PkgFile {
        rel: "funnel/profiles/scout/profile.toml",
        content: include_str!("../kits/funnel/profiles/scout/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "funnel/profiles/scout/blocks/00-scout.md",
        content: include_str!("../kits/funnel/profiles/scout/blocks/00-scout.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/helper-chat/lanius.toml",
        content: include_str!("../kits/helper/packages/helper-chat/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/helper-chat/scripts/run",
        content: include_str!("../kits/helper/packages/helper-chat/scripts/run"),
        exec: true,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/lanius.toml",
        content: include_str!("../kits/helper/packages/kb-lanius/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/kits-and-packages.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/kits-and-packages.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/llm-access.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/llm-access.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/model-guidance.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/model-guidance.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/mutation-doctrine.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/mutation-doctrine.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/overview.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/overview.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-lanius/kb/setup-checklist.md",
        content: include_str!("../kits/helper/packages/kb-lanius/kb/setup-checklist.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-user/lanius.toml",
        content: include_str!("../kits/helper/packages/kb-user/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "helper/packages/kb-user/kb/README.md",
        content: include_str!("../kits/helper/packages/kb-user/kb/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/profiles/helper/profile.toml",
        content: include_str!("../kits/helper/profiles/helper/profile.toml"),
        exec: false,
    },
    PkgFile {
        rel: "helper/profiles/helper/blocks/00-charter.md",
        content: include_str!("../kits/helper/profiles/helper/blocks/00-charter.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/profiles/helper/blocks/10-setup-progress.md",
        content: include_str!("../kits/helper/profiles/helper/blocks/10-setup-progress.md"),
        exec: false,
    },
    PkgFile {
        rel: "helper/profiles/helper/blocks/20-kb-lanius.md",
        content: include_str!("../kits/helper/profiles/helper/blocks/20-kb-lanius.md"),
        exec: false,
    },
    // stdlib: the protected, always-on kit (docs/config.md). Installed and
    // auto-approved unconditionally in init(); history (the transcript view) is
    // its first member, so the web UI's sessions tab always has something to read.
    PkgFile {
        rel: "stdlib/README.md",
        content: include_str!("../kits/stdlib/README.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/kit.toml",
        content: include_str!("../kits/stdlib/kit.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/history/lanius.toml",
        content: include_str!("../kits/stdlib/packages/history/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/history/scripts/main",
        content: include_str!("../kits/stdlib/packages/history/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/history/SKILL.md",
        content: include_str!("../kits/stdlib/packages/history/SKILL.md"),
        exec: false,
    },
    // comms — the chat/conversation reconstruction view (docs/handoffs/comms-package.md).
    // The SECOND reconstruction view after history, requiring it: it owns the
    // chat-shaped conversation-list + introspection projection that used to be
    // hard-coded in the core web server. Ships in stdlib so a fresh root's web
    // comms list (`/api/conversations` relays here) works out of the box, exactly
    // as history's transcript view does.
    PkgFile {
        rel: "stdlib/packages/comms/lanius.toml",
        content: include_str!("../kits/stdlib/packages/comms/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/comms/scripts/main",
        content: include_str!("../kits/stdlib/packages/comms/scripts/main"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/comms/scripts/comms_view.py",
        content: include_str!("../kits/stdlib/packages/comms/scripts/comms_view.py"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/comms/SKILL.md",
        content: include_str!("../kits/stdlib/packages/comms/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/launching-agents/SKILL.md",
        content: include_str!("../kits/stdlib/packages/launching-agents/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/explain-session/SKILL.md",
        content: include_str!("../kits/stdlib/packages/explain-session/SKILL.md"),
        exec: false,
    },
    // kb-search — the default knowledge-search ENGINE (docs/handoffs/kb-search.md):
    // a read-only indexing daemon (scripts/index) + the `search_knowledge` model
    // tool via the [[tool]] seam (scripts/search). Ships in stdlib so a fresh root
    // gets the index daemon and the tool folds into agents automatically; without
    // this, `lanius kb search` errors "no knowledge index yet".
    PkgFile {
        rel: "stdlib/packages/kb-search/lanius.toml",
        content: include_str!("../kits/stdlib/packages/kb-search/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-search/scripts/index",
        content: include_str!("../kits/stdlib/packages/kb-search/scripts/index"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/kb-search/scripts/search",
        content: include_str!("../kits/stdlib/packages/kb-search/scripts/search"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/kb-search/SKILL.md",
        content: include_str!("../kits/stdlib/packages/kb-search/SKILL.md"),
        exec: false,
    },
    // discovery — the privileged capability search (docs/handoffs/kb-discovery.md):
    // the `find_capability` model tool via the [[tool]] seam (scripts/find), a thin
    // wrapper over `lanius discover --json`. Ships in stdlib so a fresh root can tell
    // an agent "you don't have the discord package enabled, but it exists and matches
    // your query." Its taught availability rides the seeded 20-discovery block.
    PkgFile {
        rel: "stdlib/packages/discovery/lanius.toml",
        content: include_str!("../kits/stdlib/packages/discovery/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/discovery/scripts/find",
        content: include_str!("../kits/stdlib/packages/discovery/scripts/find"),
        exec: true,
    },
    PkgFile {
        rel: "stdlib/packages/discovery/SKILL.md",
        content: include_str!("../kits/stdlib/packages/discovery/SKILL.md"),
        exec: false,
    },
    // kb-llm-strengths — the first knowledge base (docs/handoffs/kb-core.md M2/D5):
    // the [kb] marker + a kb/ seeded with the model-tiering rules (one file per
    // model, one per role, cross-linked). Ships in stdlib so a default agent
    // "just knows" the tiering.
    // knowledge — the taught pattern (D6): a default agent "just knows" how to
    // read, search, and write knowledge bases. Pure skill text, no scripts.
    PkgFile {
        rel: "stdlib/packages/knowledge/SKILL.md",
        content: include_str!("../kits/stdlib/packages/knowledge/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/lanius.toml",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/lanius.toml"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/SKILL.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/SKILL.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/role-planner.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/role-planner.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/role-implementer.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/role-implementer.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/role-verifier.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/role-verifier.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/claude.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/claude.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/fable.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/fable.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/opus.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/opus.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/gpt-5.5.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/gpt-5.5.md"),
        exec: false,
    },
    PkgFile {
        rel: "stdlib/packages/kb-llm-strengths/kb/glm-5.2.md",
        content: include_str!("../kits/stdlib/packages/kb-llm-strengths/kb/glm-5.2.md"),
        exec: false,
    },
];

const PROFILE_TOML: &str = include_str!("../templates/profile.toml");
const RECORDER_TOML: &str = include_str!("../templates/recorder.toml");
const BUS_TOML: &str = include_str!("../templates/bus.toml");
const BLOCK_SYSTEM: &str = include_str!("../templates/block-00-system.md");
const BLOCK_CONTEXT: &str = include_str!("../templates/block-10-context.md");
// The seeded high-awareness block that TEACHES discovery's own availability
// (docs/handoffs/kb-discovery.md M2, journey-14): discovery's whole reason to
// exist is that an agent doesn't know a capability exists, so its own presence
// cannot itself be discovered — it must be taught. This block, static on the
// default/dispatching profile, is that teaching.
const BLOCK_DISCOVERY: &str = include_str!("../templates/block-20-discovery.md");

pub fn init(dir: PathBuf, kits: Vec<String>, copy_kits: bool) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let root = Root {
        dir: dir.canonicalize()?,
    };
    for d in [
        root.packages(),
        root.run_dir(),
        root.profile_dir("default").join("blocks"),
        root.secrets(),
    ] {
        std::fs::create_dir_all(d)?;
    }
    // The secret store is the kernel's; keep it 0700 so even outside the cage
    // it is not casually readable. The cage fences it from actors regardless.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(root.secrets(), std::fs::Permissions::from_mode(0o700));
    }
    // Mint the human and kernel credentials now, so they exist right after
    // init — the human's surfaces can read them before any daemon is up, and
    // the daemon's own ensure() at startup is then idempotent.
    crate::secrets::ensure(&root)?;
    // The configuration repository (docs/config.md): a kernel-owned git repo
    // whose `live` branch holds package config. Created here so every root has
    // it from the start; the cage fences it from agents (sandbox.rs Protect).
    crate::config_repo::init(&root).context("initializing the config repo")?;
    // Seed <root>/kits with the stock kits FIRST so `init --kit core` (and
    // every later `kit add`) resolves without env vars or a repo checkout.
    for f in STOCK_KIT_FILES {
        let path = root.dir.join("kits").join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }
    // Resolve every kit BEFORE installing anything: a typo'd kit name must
    // not leave a half-installed root behind.
    let kit_dirs = kits
        .iter()
        .map(|k| kit::resolve(&root, k))
        .collect::<Result<Vec<_>>>()?;

    write_if_missing(&root.recorder_file(), RECORDER_TOML, false)?;
    write_if_missing(&root.bus_file(), BUS_TOML, false)?;
    write_if_missing(
        &root.profile_dir("default").join("profile.toml"),
        PROFILE_TOML,
        false,
    )?;
    write_if_missing(
        &root.profile_dir("default").join("blocks/00-system.md"),
        BLOCK_SYSTEM,
        false,
    )?;
    write_if_missing(
        &root.profile_dir("default").join("blocks/10-context.md"),
        BLOCK_CONTEXT,
        false,
    )?;
    write_if_missing(
        &root.profile_dir("default").join("blocks/20-discovery.md"),
        BLOCK_DISCOVERY,
        false,
    )?;
    let _ =
        crate::config_repo::commit_agent(&root, "default", "config: seed default agent profile");

    for f in PKG_FILES {
        let path = root.packages().join(f.rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_if_missing(&path, f.content, f.exec)?;
    }

    // Stock harness packages seed the built-in coding agents as discoverable
    // packages without changing dispatch yet.
    seed_stock_harness_packages(&root)?;

    let conn = db::open(&root)?;
    db::init_schema(&conn)?;
    // The algedonic class: never coalesced, never queued behind other work.
    packages::upsert_throttle(
        &conn,
        "signal/#",
        &ThrottleDecl {
            coalesce: Some(false),
            ..Default::default()
        },
    )?;
    if !root.trace_file().exists() {
        std::fs::write(root.trace_file(), "")?;
    }

    // Stock packages ship with the binary and init is a human gesture, so
    // their requests are approved here with that provenance. Anything that
    // lands on the package path later asks like everything else.
    packages::sync(&root, &conn)?;
    for name in ["chat", "echo", "notify", "watchdog"] {
        packages::decide(&root, &conn, name, true, "init")?;
    }

    // Kits: starter packs (src/kit.rs). Linked by default — the kit dir
    // stays the source and a local copy shadows it; --copy vendors. Either
    // way init is the human install gesture, provenance kit:<name>.
    let mode = if copy_kits {
        kit::Mode::Copy
    } else {
        kit::Mode::Link
    };
    // Stdlib is installed and auto-approved in EVERY root (docs/config.md): the
    // protected packages the product itself depends on — history's transcript
    // view first, so the sessions tab is never a dead 503. Linked, not vendored
    // (it stays kernel-managed); its packages are revoke-guarded
    // (kit::protected_packages + `lanius revoke`).
    let stdlib_dir = kit::resolve(&root, "stdlib").context("resolving the stdlib kit")?;
    kit::install(&root, &conn, &stdlib_dir, kit::Mode::Link, true).context("installing stdlib")?;
    let mut readmes: Vec<(String, String)> = Vec::new();
    for (name, kit_dir) in kits.iter().zip(&kit_dirs) {
        if let Some(readme) = kit::install(&root, &conn, kit_dir, mode, true)? {
            readmes.push((name.clone(), readme));
        }
        println!("installed kit {name} from {}", kit_dir.display());
    }

    println!();
    println!("initialized lanius root at {}", root.dir.display());
    println!();
    println!(
        "you are \"{}\" here (the default identity). to use your own name:",
        crate::secrets::owner_name(&root)
    );
    println!("  lanius profile set default owner=<yourname>   # then restart the daemon");
    println!();
    println!("next steps:");
    // The default root needs no env var; only point at $LANIUS_ROOT when
    // this root actually requires it.
    let is_default = crate::paths::default_root()
        .ok()
        .and_then(|d| d.canonicalize().ok())
        .map(|d| d == root.dir)
        .unwrap_or(false);
    if !is_default {
        println!("  export LANIUS_ROOT={}", root.dir.display());
    }
    println!("  lanius daemon &                     # the dispatcher");
    println!("  lanius exec --session hi \"hello\"    # chat (needs ANTHROPIC_API_KEY)");
    println!("  lanius emit in/agent/main --payload '{{\"prompt\":\"check in with me\"}}'");
    println!("  lanius inbox / lanius answer <id> \"...\"");
    println!("  lanius packages                     # what's installed, what's pending");
    println!("  lanius approve history              # transcripts in the web UI (granted serving)");
    println!(
        "  lanius approve recent-history       # cross-run memory of recent mail (a context stage)"
    );
    println!("  lanius bus sub 'obs/#'              # watch the live stream");
    println!("  tail -f {}", root.trace_file().display());
    for (name, readme) in &readmes {
        println!();
        println!("── kit {name} ─────────────────────────────────────────");
        println!("{}", readme.trim_end());
    }
    Ok(())
}

fn write_if_missing(path: &Path, content: &str, exec: bool) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, content)?;
    }
    if exec {
        set_executable(path)?;
    }
    Ok(())
}

fn seed_stock_harness_packages(root: &Root) -> Result<()> {
    let exe = std::env::current_exe().context("locating the running lanius binary")?;
    let exe_dir = exe
        .parent()
        .context("running lanius binary has no parent directory")?;

    for pkg in STOCK_HARNESS_PACKAGES {
        let pkg_dir = root.packages().join(pkg.dir);
        let bin_dir = pkg_dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        write_if_missing(&pkg_dir.join("lanius.toml"), pkg.manifest, false)?;

        let adapter = bin_dir.join("adapter");
        let source = exe_dir.join(format!("{}{}", pkg.binary, std::env::consts::EXE_SUFFIX));
        if source.is_file() {
            refresh_adapter_if_stale(&source, &adapter)?;
            set_executable(&adapter)?;
        } else if !adapter.exists() {
            eprintln!(
                "[init] warning: missing stock harness binary {}; seeded {} without bin/adapter",
                source.display(),
                pkg_dir.display()
            );
        }
    }
    Ok(())
}

/// An adapter is stale if it's missing, or the source binary's mtime is newer
/// than the installed adapter's. Any metadata error is treated as stale too —
/// fail toward re-copying a correct binary rather than leaving a possibly-old
/// one in place.
fn is_adapter_stale(source: &Path, adapter: &Path) -> bool {
    if !adapter.exists() {
        return true;
    }
    let source_mtime = match std::fs::metadata(source).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };
    let adapter_mtime = match std::fs::metadata(adapter).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };
    source_mtime > adapter_mtime
}

/// Re-copy `source` over `adapter` if stale (missing or older than source).
/// macOS-safe: never copies over a running/signed Mach-O in place (that
/// SIGKILLs the next launch of the process using the old inode) — removes
/// the old file first so the copy lands on a fresh inode. A no-op (existing
/// inode left untouched) when the adapter is already up to date.
fn refresh_adapter_if_stale(source: &Path, adapter: &Path) -> Result<()> {
    if is_adapter_stale(source, adapter) {
        let _ = std::fs::remove_file(adapter);
        std::fs::copy(source, adapter)
            .with_context(|| format!("copying {} -> {}", source.display(), adapter.display()))?;
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod adapter_refresh_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};

    /// A fresh, unique scratch dir under the system temp dir (no tempfile
    /// crate dependency — this project has none).
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanius-adapter-refresh-test-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        let f = fs::File::open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    fn inode(path: &Path) -> u64 {
        fs::metadata(path).unwrap().ino()
    }

    #[test]
    fn refreshes_when_source_is_newer_and_lands_on_a_fresh_inode() {
        let dir = scratch_dir("newer");
        let source = dir.join("harness-claude");
        let adapter = dir.join("adapter");

        fs::write(&source, b"old bytes").unwrap();
        fs::write(&adapter, b"already installed").unwrap();

        let base = SystemTime::now() - Duration::from_secs(60);
        set_mtime(&source, base);
        set_mtime(&adapter, base);

        let adapter_ino_before = inode(&adapter);

        // Source gets rebuilt/reinstalled: bump its mtime strictly newer.
        fs::write(&source, b"new bytes").unwrap();
        set_mtime(&source, base + Duration::from_secs(10));

        refresh_adapter_if_stale(&source, &adapter).unwrap();

        assert_ne!(
            inode(&adapter),
            adapter_ino_before,
            "a stale adapter must be refreshed onto a fresh inode, never overwritten in place"
        );
        assert_eq!(fs::read(&adapter).unwrap(), b"new bytes");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leaves_up_to_date_adapter_untouched() {
        let dir = scratch_dir("uptodate");
        let source = dir.join("harness-claude");
        let adapter = dir.join("adapter");

        fs::write(&source, b"same bytes").unwrap();
        fs::write(&adapter, b"same bytes").unwrap();

        let base = SystemTime::now() - Duration::from_secs(60);
        set_mtime(&source, base);
        // Adapter mtime equal to source mtime => not stale (source must be
        // strictly newer to trigger a refresh).
        set_mtime(&adapter, base);

        let adapter_ino_before = inode(&adapter);

        refresh_adapter_if_stale(&source, &adapter).unwrap();

        assert_eq!(
            inode(&adapter),
            adapter_ino_before,
            "an up-to-date adapter must not be re-copied"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refreshes_when_adapter_is_missing() {
        let dir = scratch_dir("missing");
        let source = dir.join("harness-claude");
        let adapter = dir.join("adapter");

        fs::write(&source, b"fresh install").unwrap();
        assert!(!adapter.exists());

        refresh_adapter_if_stale(&source, &adapter).unwrap();

        assert!(adapter.exists());
        assert_eq!(fs::read(&adapter).unwrap(), b"fresh install");

        fs::remove_dir_all(&dir).ok();
    }
}

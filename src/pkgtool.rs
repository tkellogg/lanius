//! The `[[tool]]` seam (docs/handoffs/kb-search.md M0): a package supplies an
//! agent tool (name, description, JSON schema, run script). Once the human
//! approves the "tool" grant AND the package is visible to a profile, the tool
//! folds into that agent's tool array beside the kernel builtins (exec::tool_defs)
//! — the same visibility gate `provides_builtin_tools` rides. A call dispatches
//! exec-mode, the `[[stage]]` contract (src/context.rs run_exec_stage): the call
//! args arrive as JSON on stdin and the script's stdout JSON becomes the tool
//! result, under the declared `timeout_ms`; a nonzero exit or an overrun degrades
//! that ONE call into a legible error result, never the run.
//!
//! Bare names, no `<pkg>__` prefix: a second package declaring the SAME name
//! swaps the engine behind the tool, invisibly to the agent. The approve gate
//! (packages::decide) is what forbids two live holders, so a loaded pool holds at
//! most one tool per name; the duplicate guard here is defence in depth.

use crate::envcompat::EnvDual;
use crate::manifest::ToolDecl;
use crate::packages;
use crate::paths::Root;
use crate::profile::{self, Profile};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct PkgTool {
    pub package: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub script: PathBuf,
    /// The package's state dir (`<root>/run/pkg-<name>`) — the same ELANUS_SCRATCH
    /// the daemon side gets, so a tool script reads what its own daemon indexed.
    pub scratch: PathBuf,
    pub timeout_ms: u64,
}

#[derive(Default)]
pub struct Pool {
    pub tools: Vec<PkgTool>,
}

impl Pool {
    /// Every approved, profile-visible `[[tool]]`. Approval is checked under the
    /// FRESH manifest hash — the dispatch-time pin exec handlers, stages and MCP
    /// servers all use, so an edited script is stale before the next sync.
    pub fn load(root: &Root, conn: &Connection, profile_name: &str, prof: &Profile) -> Pool {
        let mut pool = Pool::default();
        let pkgs = match packages::discover_for_profile(root, profile_name) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[tool] discovery failed: {e:#}");
                return pool;
            }
        };
        for pkg in pkgs {
            if !profile::skill_visible(prof, &pkg.name) {
                continue;
            }
            let Some(lm) = &pkg.manifest else { continue };
            for decl in &lm.manifest.tool {
                let approved = packages::approved_under(conn, &pkg.name, &lm.hash, "tool")
                    .map(|v| v.iter().any(|n| n == &decl.name))
                    .unwrap_or(false);
                if !approved {
                    continue;
                }
                pool.push_tool(root, &pkg.name, &pkg.dir, decl);
            }
        }
        pool
    }

    fn push_tool(&mut self, root: &Root, package: &str, pkg_dir: &Path, decl: &ToolDecl) {
        if let Some(prev) = self.tools.iter().find(|t| t.name == decl.name) {
            eprintln!(
                "[tool] {package}: {:?} already provided by {} — dropping the duplicate \
                 (the approve gate should have refused this)",
                decl.name, prev.package
            );
            return;
        }
        self.tools.push(PkgTool {
            package: package.to_string(),
            name: decl.name.clone(),
            description: decl.description.clone(),
            schema: decl.resolved_schema(pkg_dir),
            script: pkg_dir.join(&decl.run),
            scratch: root.run_dir().join(format!("pkg-{package}")),
            timeout_ms: decl.timeout_ms,
        });
    }

    pub fn tool_defs(&self) -> Vec<genai::chat::Tool> {
        self.tools
            .iter()
            .map(|t| {
                genai::chat::Tool::new(t.name.clone())
                    .with_description(t.description.clone())
                    .with_schema(t.schema.clone())
            })
            .collect()
    }

    /// Route a bare-name call. `None` = no approved package tool by that name
    /// (the caller falls through to the MCP pool, then "unknown tool").
    pub fn call(&self, root: &Root, name: &str, args: &Value) -> Option<String> {
        let t = self.tools.iter().find(|t| t.name == name)?;
        Some(match dispatch(root, t, args) {
            Ok(out) => out,
            Err(e) => {
                json!({ "error": format!("tool {}/{}: {e:#}", t.package, t.name) }).to_string()
            }
        })
    }
}

/// exec-mode dispatch, mirroring `run_exec_stage` (src/context.rs): args JSON on
/// stdin, stdout JSON as the tool result, the declared `timeout_ms` budget with a
/// kill on the deadline, nonzero exit = an error. Stdin is written and stdout
/// drained on their own threads so a script writing past the pipe buffer in
/// either direction cannot deadlock the exec.
fn dispatch(root: &Root, t: &PkgTool, args: &Value) -> Result<String> {
    if !t.script.exists() {
        bail!("script {} missing", t.script.display());
    }
    std::fs::create_dir_all(&t.scratch).ok();
    let input = serde_json::to_string(args)?;
    let mut child = Command::new(&t.script)
        .current_dir(t.script.parent().unwrap_or(&root.dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .env_dual("ROOT", &root.dir)
        .env_dual("DB", root.db())
        .env("ELANUS_PACKAGE", &t.package)
        .env("ELANUS_TOOL", &t.name)
        .env("ELANUS_SCRATCH", &t.scratch)
        .spawn()
        .with_context(|| format!("spawning {}", t.script.display()))?;
    let mut stdin = child.stdin.take().unwrap();
    let w = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out_h = child.stdout.take().map(|mut o| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut b = String::new();
            let _ = o.read_to_string(&mut b);
            b
        })
    });
    let deadline = Instant::now() + Duration::from_millis(t.timeout_ms);
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = w.join();
            let out = out_h
                .map(|h| h.join().unwrap_or_default())
                .unwrap_or_default();
            if !status.success() {
                bail!("exited {:?}: {}", status.code(), out.trim());
            }
            let out = out.trim();
            if out.is_empty() {
                return Ok(json!({ "error": "tool returned no output" }).to_string());
            }
            return Ok(out.to_string());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}ms", t.timeout_ms);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-pkgtool-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("packages")).unwrap();
        Root { dir }
    }

    fn tool(root: &Root, script_body: &str, timeout_ms: u64) -> PkgTool {
        let sdir = root.dir.join("scripts");
        std::fs::create_dir_all(&sdir).unwrap();
        let script = sdir.join("t");
        std::fs::write(&script, script_body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        PkgTool {
            package: "eng".into(),
            name: "echo_args".into(),
            description: "echo".into(),
            schema: json!({ "type": "object" }),
            script,
            scratch: root.run_dir().join("pkg-eng"),
            timeout_ms,
        }
    }

    #[test]
    fn dispatch_round_trips_args_to_stdin_and_stdout_to_result() {
        // The [[stage]] contract: args JSON on stdin, stdout JSON as the result.
        let root = scratch("roundtrip");
        let t = tool(&root, "#!/bin/sh\ncat\n", 5_000);
        let out = dispatch(&root, &t, &json!({ "query": "who verifies" })).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["query"], "who verifies", "the args round-trip through stdin");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn nonzero_exit_is_a_legible_error_not_a_panic() {
        let root = scratch("nonzero");
        let t = tool(&root, "#!/bin/sh\necho boom >&2\nexit 3\n", 5_000);
        // Pool::call wraps the dispatch error into an error result the model sees.
        let mut pool = Pool::default();
        pool.tools.push(t);
        let out = pool.call(&root, "echo_args", &json!({})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(
            v["error"].as_str().unwrap_or("").contains("exited"),
            "nonzero exit degrades to an error result: {out}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn overrun_is_killed_and_reported() {
        let root = scratch("timeout");
        let t = tool(&root, "#!/bin/sh\nsleep 30\n", 120);
        let started = Instant::now();
        let mut pool = Pool::default();
        pool.tools.push(t);
        let out = pool.call(&root, "echo_args", &json!({})).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "killed at the deadline");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(
            v["error"].as_str().unwrap_or("").contains("timed out"),
            "an overrun yields a timeout error result: {out}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn tool_is_invisible_until_approved_then_folds_in() {
        // M0 acceptance: a [[tool]] package is invisible to agents until approved;
        // once approved (and visible to the profile) the tool folds into the array
        // with its declared schema, and a call round-trips args→stdin→stdout.
        let root = scratch("gating");
        let d = root.dir.join("packages/echoer");
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("elanus.toml"),
            "[[tool]]\nname = \"echo_args\"\ndescription = \"echo the args back\"\nrun = \"scripts/echo\"\n\n\
             [tool.schema]\ntype = \"object\"\n\n[tool.schema.properties.x]\ntype = \"string\"\n",
        )
        .unwrap();
        std::fs::write(d.join("scripts/echo"), "#!/bin/sh\ncat\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            d.join("scripts/echo"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        let prof = Profile::default();

        // Requested but not approved: absent from the array.
        let pool = Pool::load(&root, &conn, "default", &prof);
        assert!(pool.tools.is_empty(), "unapproved tool is invisible to the agent");

        packages::decide(&root, &conn, "echoer", true, "test").unwrap();
        let pool = Pool::load(&root, &conn, "default", &prof);
        assert_eq!(pool.tools.len(), 1, "approved + visible tool folds in");
        let defs = pool.tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.to_string(), "echo_args");
        // The declared schema rides through to the model's tool array.
        let t = &pool.tools[0];
        assert_eq!(t.schema["properties"]["x"]["type"], "string");
        // A call round-trips.
        let out = pool.call(&root, "echo_args", &json!({ "x": "hi" })).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["x"], "hi");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    fn write_engine(root: &Root, name: &str, marker: &str) {
        // A toy engine: a package declaring [[tool]] search_knowledge whose script
        // stamps its identity into the result, so a swap is observable while the
        // TOOL NAME the agent sees stays fixed.
        let d = root.dir.join("packages").join(name);
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("elanus.toml"),
            "[[tool]]\nname = \"search_knowledge\"\ndescription = \"search\"\n\
             run = \"scripts/search\"\n\n[tool.schema]\ntype = \"object\"\n\
             required = [\"query\"]\n\n[tool.schema.properties.query]\ntype = \"string\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("scripts/search"),
            format!("#!/bin/sh\ncat >/dev/null\necho '{{\"engine\":\"{marker}\"}}'\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            d.join("scripts/search"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    #[test]
    fn engine_swap_is_invisible_and_dual_enable_is_refused() {
        // THE SWAP PROOF (docs/handoffs/kb-search.md M3): two packages declare the
        // SAME bare [[tool]] name = "search_knowledge" with different engines.
        //  - approve one → the agent's array carries search_knowledge, its engine
        //    answers;
        //  - with BOTH enabled, approving the second is refused loudly, naming the
        //    holder (M0's collision rule, end-to-end);
        //  - revoke the first + approve the second → the array STILL carries
        //    search_knowledge, unchanged in name, now answered by the other engine.
        //    The engine swap is invisible to the agent.
        let root = scratch("swap");
        write_engine(&root, "engine-a", "a");
        write_engine(&root, "engine-b", "b");
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();
        let prof = Profile::default();

        // Engine A live: the tool exists, A answers.
        packages::decide(&root, &conn, "engine-a", true, "test").unwrap();
        let pool = Pool::load(&root, &conn, "default", &prof);
        assert_eq!(pool.tools.len(), 1);
        assert_eq!(pool.tools[0].name.to_string(), "search_knowledge");
        let out = pool.call(&root, "search_knowledge", &json!({ "query": "x" })).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["engine"], "a");

        // Dual-enable refused, naming the incumbent.
        let err = packages::decide(&root, &conn, "engine-b", true, "test").unwrap_err();
        assert!(
            format!("{err:#}").contains("engine-a"),
            "approving the second holder must refuse loudly naming engine-a: {err:#}"
        );

        // Swap: revoke A, approve B — same tool name, the OTHER engine answers.
        packages::decide(&root, &conn, "engine-a", false, "test").unwrap();
        packages::decide(&root, &conn, "engine-b", true, "test").unwrap();
        let pool = Pool::load(&root, &conn, "default", &prof);
        assert_eq!(pool.tools.len(), 1, "still exactly one holder after the swap");
        assert_eq!(
            pool.tools[0].name.to_string(),
            "search_knowledge",
            "the tool NAME the agent sees is unchanged across the engine swap"
        );
        let out = pool.call(&root, "search_knowledge", &json!({ "query": "x" })).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["engine"], "b");
        std::fs::remove_dir_all(&root.dir).ok();
    }
}

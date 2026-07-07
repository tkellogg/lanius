//! MCP client — stdio transport, hand-rolled (docs/security.md entry 8;
//! HANDOFF phase 4).
//!
//! Doctrine: **MCP is a border protocol.** It exists here so third-party
//! tool servers (playwright, databases — code other people maintain) can
//! plug into the agent's native tool array. First-party mechanisms never
//! speak it: stages ride stdin/stdout or the bus, history rides HTTP.
//! Hand-rolled for the same reason the kernel's QoS 0 mirror is: three
//! JSON-RPC methods (initialize, tools/list, tools/call) over
//! newline-delimited stdio don't justify an SDK dependency tree, and the
//! failure modes stay legible. The streamable-HTTP transport (stateful
//! servers on negotiated ports) is the designed next step.
//!
//! Security posture:
//! - An [[mcp]] declaration is a grant request (kind "mcp"); servers spawn
//!   only approved, inside the agent's cage (same write-fence as the shell
//!   tool — a tool server is agent-reachable code).
//! - Tool descriptions are untrusted input that lands in the model's
//!   context (tool poisoning). They are pinned trust-on-first-use: the
//!   sorted tools JSON is hashed into kv on first load; a changed hash
//!   makes the server's tools vanish LOUDLY until the human re-approves
//!   the package (decide() clears the pin, next load re-pins). Weaker than
//!   pin-at-review — that needs running the server during review — and
//!   recorded as such in security.md entry 8.
//! - A server that fails to spawn or answer is skipped loudly, not fatal:
//!   missing tools degrade the agent, they don't corrupt meaning (stages
//!   fail closed for the opposite reason).

use crate::envcompat::EnvDual;
use crate::packages;
use crate::paths::Root;
use crate::profile::{self, Profile};
use crate::sandbox;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INIT_BUDGET: Duration = Duration::from_secs(10);
const CALL_BUDGET: Duration = Duration::from_secs(60);
const PROTOCOL: &str = "2025-03-26";

pub struct ToolDef {
    pub name: String,      // wire name on the server ("add")
    pub full_name: String, // what the model sees ("adder__add")
    pub description: String,
    pub schema: Value,
}

struct Io {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
}

pub struct Server {
    pub package: String,
    pub name: String,
    pub tools: Vec<ToolDef>,
    io: Mutex<Io>,
}

#[derive(Default)]
pub struct Pool {
    pub servers: Vec<Server>,
}

fn kv_pin_key(package: &str, server: &str) -> String {
    format!("mcp_tools:{package}:{server}")
}

/// decide() calls this so a human approval gesture re-pins tool
/// descriptions (the TOFU loop's second half).
pub fn clear_pins(conn: &Connection, package: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM kv WHERE key LIKE 'mcp_tools:' || ?1 || ':%'",
        [package],
    )?;
    Ok(())
}

impl Pool {
    /// Spawn every approved, profile-visible [[mcp]] server and collect its
    /// tools. Approval is checked under the FRESH manifest hash (the same
    /// dispatch-time pin as exec handlers and stages).
    pub fn load(
        root: &Root,
        conn: &Connection,
        profile_name: &str,
        prof: &Profile,
        cage: &sandbox::Cage,
    ) -> Pool {
        let mut pool = Pool::default();
        let pkgs = match packages::discover_for_profile(root, profile_name) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[mcp] discovery failed: {e:#}");
                return pool;
            }
        };
        for pkg in pkgs {
            if !profile::skill_visible(prof, &pkg.name) {
                continue;
            }
            let Some(lm) = &pkg.manifest else { continue };
            for decl in &lm.manifest.mcp {
                let approved = packages::approved_under(conn, &pkg.name, &lm.hash, "mcp")
                    .map(|v| v.iter().any(|n| n == &decl.name))
                    .unwrap_or(false);
                if !approved {
                    eprintln!(
                        "[mcp] {}/{} requested but not approved — tools absent",
                        pkg.name, decl.name
                    );
                    continue;
                }
                let script = pkg.dir.join(&decl.run);
                match Server::spawn(root, cage, &pkg.name, &decl.name, &script, &decl.args) {
                    Ok(mut srv) => {
                        // TOFU pin on the sorted tools JSON (descriptions
                        // included — they enter the model's context).
                        let mut blob: Vec<Value> = srv
                            .tools
                            .iter()
                            .map(|t| json!({ "name": t.name, "description": t.description, "schema": t.schema }))
                            .collect();
                        blob.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
                        let hash = format!(
                            "{:x}",
                            Sha256::digest(serde_json::to_string(&blob).unwrap_or_default())
                        );
                        let key = kv_pin_key(&pkg.name, &decl.name);
                        match crate::db::kv_get(conn, &key) {
                            Ok(None) => {
                                let _ = crate::db::kv_set(conn, &key, &hash);
                            }
                            Ok(Some(pinned)) if pinned == hash => {}
                            Ok(Some(_)) => {
                                eprintln!(
                                    "[mcp] {}/{}: TOOLS CHANGED since they were pinned — refusing them; \
                                     review and `lanius approve {}` to re-pin",
                                    pkg.name, decl.name, pkg.name
                                );
                                srv.shutdown();
                                continue;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[mcp] {}/{}: pin read failed ({e}); refusing tools",
                                    pkg.name, decl.name
                                );
                                srv.shutdown();
                                continue;
                            }
                        }
                        pool.servers.push(srv);
                    }
                    Err(e) => {
                        eprintln!(
                            "[mcp] {}/{} failed to start: {e:#} — tools absent",
                            pkg.name, decl.name
                        );
                    }
                }
            }
        }
        pool
    }

    pub fn tool_defs(&self) -> Vec<genai::chat::Tool> {
        self.servers
            .iter()
            .flat_map(|s| s.tools.iter())
            .map(|t| {
                genai::chat::Tool::new(t.full_name.clone())
                    .with_description(t.description.clone())
                    .with_schema(t.schema.clone())
            })
            .collect()
    }

    /// Route a namespaced call ("server__tool"). None = not an MCP tool.
    pub fn call(&self, full_name: &str, args: &Value) -> Option<String> {
        let (server, tool) = full_name.split_once("__")?;
        let srv = self.servers.iter().find(|s| s.name == server)?;
        srv.tools.iter().find(|t| t.name == tool)?;
        Some(match srv.call_tool(tool, args) {
            Ok(text) => text,
            Err(e) => json!({ "error": format!("mcp {}/{}/{tool}: {e:#}", srv.package, srv.name) })
                .to_string(),
        })
    }
}

impl Server {
    fn spawn(
        root: &Root,
        cage: &sandbox::Cage,
        package: &str,
        name: &str,
        script: &std::path::Path,
        args: &[String],
    ) -> Result<Server> {
        if !script.exists() {
            bail!("script {} missing", script.display());
        }
        let mut cmd = cage.command(script);
        cmd.args(args)
            .current_dir(script.parent().unwrap_or(&root.dir))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_dual("ROOT", &root.dir);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", script.display()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name(format!("mcp-{name}"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })?;
        let mut srv = Server {
            package: package.to_string(),
            name: name.to_string(),
            tools: Vec::new(),
            io: Mutex::new(Io {
                child,
                stdin,
                rx,
                next_id: 0,
            }),
        };
        let deadline = Instant::now() + INIT_BUDGET;
        srv.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "lanius", "version": env!("CARGO_PKG_VERSION") }
            }),
            deadline,
        )?;
        srv.notify("notifications/initialized")?;
        let listed = srv.request("tools/list", json!({}), deadline)?;
        let tools = listed["tools"].as_array().cloned().unwrap_or_default();
        for t in tools {
            let Some(tname) = t["name"].as_str() else {
                continue;
            };
            if !crate::topic::valid_name(tname) {
                eprintln!("[mcp] {name}: skipping tool with unusable name {tname:?}");
                continue;
            }
            srv.tools.push(ToolDef {
                name: tname.to_string(),
                full_name: format!("{name}__{tname}"),
                description: t["description"].as_str().unwrap_or("").to_string(),
                schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or(json!({ "type": "object" })),
            });
        }
        Ok(srv)
    }

    fn call_tool(&self, tool: &str, args: &Value) -> Result<String> {
        let res = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": args }),
            Instant::now() + CALL_BUDGET,
        )?;
        // Concatenate text parts; non-text content is reported, not dropped.
        let mut out = String::new();
        for part in res["content"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            match part["type"].as_str() {
                Some("text") => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(part["text"].as_str().unwrap_or(""));
                }
                Some(other) => {
                    out.push_str(&format!("[{other} content omitted]"));
                }
                None => {}
            }
        }
        if res["isError"].as_bool() == Some(true) {
            return Ok(json!({ "error": out }).to_string());
        }
        Ok(out)
    }

    /// One JSON-RPC round trip. Out-of-order responses are tolerated only
    /// in the trivial sense (other ids are discarded) — calls are serial.
    fn request(&self, method: &str, params: Value, deadline: Instant) -> Result<Value> {
        let mut io = self
            .io
            .lock()
            .map_err(|_| anyhow::anyhow!("client poisoned"))?;
        io.next_id += 1;
        let id = io.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(io.stdin, "{msg}").context("server stdin closed")?;
        io.stdin.flush().ok();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{method}: timed out");
            }
            match io.rx.recv_timeout(left) {
                Ok(line) => {
                    let v: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue, // servers may log junk on stdout; skip
                    };
                    if v["id"].as_u64() != Some(id) {
                        continue; // a notification or stale response
                    }
                    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                        bail!("{method}: server error {err}");
                    }
                    return Ok(v["result"].clone());
                }
                Err(RecvTimeoutError::Timeout) => bail!("{method}: timed out"),
                Err(RecvTimeoutError::Disconnected) => bail!("{method}: server exited"),
            }
        }
    }

    fn notify(&self, method: &str) -> Result<()> {
        let mut io = self
            .io
            .lock()
            .map_err(|_| anyhow::anyhow!("client poisoned"))?;
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        writeln!(io.stdin, "{msg}").context("server stdin closed")?;
        io.stdin.flush().ok();
        Ok(())
    }

    fn shutdown(&mut self) {
        if let Ok(mut io) = self.io.lock() {
            let _ = io.child.kill();
            let _ = io.child.wait();
        }
    }
}

/// The exec has many exits (suspend, bail, signal preemption); servers must
/// not outlive it on any of them.
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

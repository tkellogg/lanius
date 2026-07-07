//! `lanius provider` — manage model providers (docs/handoffs/model-providers.md,
//! M1). A provider is a named, encrypted credential; this is the human-direct
//! surface to define/list/test/remove one. The secret is NEVER printed — `list`
//! and `get` show metadata and a redaction; `test` decrypts transiently to probe
//! `/models` and reports reachability only.

use crate::models;
use crate::paths::Root;
use crate::provider::{self, Credential, HarnessId, Provider, Secret, Wire, REDACTED};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Read;

/// Inputs for `provider add`, parsed from the CLI by main.
pub struct AddArgs {
    pub name: String,
    /// Build a NativeLogin instead of an ApiKey.
    pub native: bool,
    /// NativeLogin harness pin (optional), or codex/claude/opencode wire selection is via `wire`.
    pub tool: Option<String>,
    pub wire: Option<String>,
    pub base_url: Option<String>,
    /// The literal key (convenience; visible to the process table). Prefer `key_env`/stdin.
    pub key: Option<String>,
    /// Read the key from this environment variable (keeps it off the command line).
    pub key_env: Option<String>,
    /// Repeatable `Name=Value` extra headers (LiteLLM/OpenRouter).
    pub headers: Vec<String>,
}

/// `lanius provider add` — store (and encrypt) a provider.
pub fn add(root: &Root, conn: &Connection, a: AddArgs) -> Result<()> {
    let cred = if a.native {
        let tool = a.tool.as_deref().map(HarnessId::parse).transpose()?;
        Credential::NativeLogin { tool }
    } else {
        let wire = Wire::parse(a.wire.as_deref().unwrap_or("anthropic"))?;
        let base_url = a
            .base_url
            .clone()
            .context("--base-url is required for an api-key provider")?;
        let key = resolve_key(&a)?;
        let mut headers = Vec::new();
        for h in &a.headers {
            let (name, value) = h
                .split_once('=')
                .with_context(|| format!("--header must be Name=Value, got {h:?}"))?;
            headers.push((name.trim().to_string(), Secret::new(value.to_string())));
        }
        Credential::ApiKey {
            wire,
            base_url,
            key: Secret::new(key),
            headers,
        }
    };
    provider::add(
        root,
        conn,
        &Provider {
            name: a.name.clone(),
            credential: cred,
        },
    )?;
    println!("added provider {} ({})", a.name, kind_label(conn, &a.name)?);
    Ok(())
}

/// Resolve the api-key secret without ever defaulting to printing it: explicit
/// `--key`, else `--key-env <VAR>`, else stdin (when piped). Fail closed.
fn resolve_key(a: &AddArgs) -> Result<String> {
    if let Some(k) = &a.key {
        return Ok(k.clone());
    }
    if let Some(var) = &a.key_env {
        return std::env::var(var)
            .ok()
            .filter(|s| !s.is_empty())
            .with_context(|| format!("--key-env {var}: that env var is unset or empty"));
    }
    // Stdin fallback: read a piped secret (so it stays off the process table).
    let mut buf = String::new();
    let n = std::io::stdin().read_to_string(&mut buf).unwrap_or(0);
    let key = buf.trim().to_string();
    if n == 0 || key.is_empty() {
        bail!("no api key supplied — pass --key, --key-env <VAR>, or pipe it on stdin");
    }
    Ok(key)
}

fn kind_label(conn: &Connection, name: &str) -> Result<String> {
    Ok(provider::get_meta(conn, name)?
        .map(|m| m.kind)
        .unwrap_or_default())
}

/// `lanius provider list` — one JSON line per provider, metadata only.
pub fn list(conn: &Connection, want_json: bool) -> Result<()> {
    let providers = provider::list(conn)?;
    for p in &providers {
        if want_json {
            println!(
                "{}",
                json!({
                    "name": p.name,
                    "kind": p.kind,
                    "wire": p.wire,
                    "base_url": p.base_url,
                    "tool": p.tool,
                    "headers": p.header_names,
                    "secret": if p.kind == "api_key" { Some(REDACTED) } else { None },
                })
            );
        } else {
            let detail = match p.kind.as_str() {
                "api_key" => format!(
                    "{} {}  key={}",
                    p.wire.as_deref().unwrap_or("?"),
                    p.base_url.as_deref().unwrap_or(""),
                    REDACTED
                ),
                _ => format!(
                    "native-login{}",
                    p.tool
                        .as_deref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default()
                ),
            };
            println!("{:<20} {:<14} {}", p.name, p.kind, detail);
        }
    }
    Ok(())
}

/// `lanius provider get <name>` — one provider's metadata (secret redacted).
pub fn get(conn: &Connection, name: &str, want_json: bool) -> Result<()> {
    let Some(p) = provider::get_meta(conn, name)? else {
        bail!("no provider {name:?}");
    };
    if want_json {
        println!(
            "{}",
            json!({
                "name": p.name,
                "kind": p.kind,
                "wire": p.wire,
                "base_url": p.base_url,
                "tool": p.tool,
                "headers": p.header_names,
                "secret": if p.kind == "api_key" { Some(REDACTED) } else { None },
            })
        );
    } else {
        println!("name      {}", p.name);
        println!("kind      {}", p.kind);
        if let Some(w) = &p.wire {
            println!("wire      {w}");
        }
        if let Some(b) = &p.base_url {
            println!("base_url  {b}");
        }
        if let Some(t) = &p.tool {
            println!("tool      {t}");
        }
        if !p.header_names.is_empty() {
            println!("headers   {} = {}", p.header_names.join(", "), REDACTED);
        }
        if p.kind == "api_key" {
            println!("key       {REDACTED}");
        }
    }
    Ok(())
}

/// `lanius provider test <name>` — reachability via the `/models` probe. For an
/// ApiKey the key is decrypted transiently and used to probe; for a NativeLogin
/// there is nothing to probe. With `--json` the result is machine-readable (a
/// single JSON object) and a probe FAILURE is reported in-band (`reachable:false`
/// + `error`) with a success exit so the web layer can render it — without
/// `--json` an unreachable provider is a hard error, as before.
pub fn test(root: &Root, conn: &Connection, name: &str, want_json: bool) -> Result<()> {
    let Some(p) = provider::get(root, conn, name)? else {
        if want_json {
            println!(
                "{}",
                json!({ "ok": false, "error": format!("no provider {name:?}") })
            );
            return Ok(());
        }
        bail!("no provider {name:?}");
    };
    match &p.credential {
        Credential::NativeLogin { tool } => {
            if want_json {
                // A native-login credential has no API-key /models endpoint to
                // probe — the harness uses its own login. Report it as such: no
                // model list, but explicitly NOT an error (this is the case that
                // fixes the spurious "provider list unavailable" warning).
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "name": name,
                        "kind": "native_login",
                        "native": true,
                        "tool": tool.map(|t| t.as_str()),
                        "reachable": Value::Null,
                        "models": [],
                        "count": 0,
                    })
                );
            } else {
                println!("{name}: native login — nothing to probe");
            }
            Ok(())
        }
        Credential::ApiKey { base_url, key, .. } => {
            if want_json {
                match models::probe(base_url, key.expose()) {
                    Ok(models) => println!(
                        "{}",
                        json!({
                            "ok": true,
                            "name": name,
                            "kind": "api_key",
                            "reachable": true,
                            "base_url": base_url,
                            "count": models.len(),
                            "models": models,
                        })
                    ),
                    Err(e) => println!(
                        "{}",
                        json!({
                            "ok": true,
                            "name": name,
                            "kind": "api_key",
                            "reachable": false,
                            "base_url": base_url,
                            "models": [],
                            "count": 0,
                            "error": format!("{e:#}"),
                        })
                    ),
                }
                return Ok(());
            }
            let models = models::probe(base_url, key.expose())?;
            println!("{name}: ok — {} models at {base_url}", models.len());
            for m in models.iter().take(20) {
                let id = m["id"].as_str().unwrap_or_default();
                println!("  {id}");
            }
            Ok(())
        }
    }
}

/// `lanius provider rm <name>` — delete a provider.
pub fn rm(conn: &Connection, name: &str) -> Result<()> {
    if provider::rm(conn, name)? {
        println!("removed provider {name}");
    } else {
        bail!("no provider {name:?}");
    }
    Ok(())
}

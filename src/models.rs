//! `lanius models` — ask the configured provider what models it serves
//! (Anthropic-compatible GET /v1/models). Resolution mirrors
//! exec::build_client: profile base_url > ANTHROPIC_BASE_URL > the real
//! Anthropic API; key from profile.api_key_env > ANTHROPIC_API_KEY. A
//! provider without the endpoint (some compat layers skip it) is an
//! honest error, not a guess — callers (the web UI's model picker) fall
//! back to static suggestions.

use crate::paths::Root;
use crate::profile;
use anyhow::{bail, Context, Result};
use serde_json::Value;

pub fn list(root: &Root, profile_name: &str, json: bool) -> Result<()> {
    let (prof, _) = profile::load(root, profile_name)?;
    let base = prof
        .model
        .base_url
        .clone()
        .or_else(|| {
            std::env::var("ANTHROPIC_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "https://api.anthropic.com".into());
    let key_env = prof
        .model
        .api_key_env
        .clone()
        .unwrap_or_else(|| "ANTHROPIC_API_KEY".into());
    let key = std::env::var(&key_env)
        .ok()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("no API key in ${key_env}"))?;
    let models = probe(&base, &key)?;
    for m in &models {
        let id = m["id"].as_str().unwrap_or_default();
        let name = m["display_name"].as_str().unwrap_or("");
        if json {
            println!("{}", serde_json::json!({ "id": id, "display_name": name }));
        } else {
            println!("{id:<40} {name}");
        }
    }
    Ok(())
}

/// Probe an Anthropic-compatible provider for its model list (GET /v1/models),
/// given a base URL and a literal API key. Reused by `lanius models` (profile
/// coordinates) and `lanius provider test` (a vault provider's decrypted key).
/// Returns the `data` array; an empty/absent endpoint is an honest error.
pub fn probe(base: &str, key: &str) -> Result<Vec<Value>> {
    let base = base.trim_end_matches('/');
    // Compat layers put /models in different places: Anthropic serves
    // {base}/v1/models, DeepSeek's anthropic shim serves NOTHING under
    // /anthropic but the account's native API has {origin}/models
    // (OpenAI-style). Probe the plausible spots in order, first list wins.
    let mut candidates: Vec<String> = Vec::new();
    if base.ends_with("/v1") {
        candidates.push(format!("{base}/models"));
    } else {
        candidates.push(format!("{base}/v1/models"));
        candidates.push(format!("{base}/models"));
    }
    if let Ok(u) = reqwest::Url::parse(base) {
        if let Some(host) = u.host_str() {
            let origin = format!(
                "{}://{host}{}",
                u.scheme(),
                u.port().map(|p| format!(":{p}")).unwrap_or_default()
            );
            for c in [format!("{origin}/models"), format!("{origin}/v1/models")] {
                if !candidates.contains(&c) {
                    candidates.push(c);
                }
            }
        }
    }
    let rt = tokio::runtime::Runtime::new()?;
    let body: Value = rt.block_on(async {
        let client = reqwest::Client::new();
        let mut tried: Vec<String> = Vec::new();
        for url in &candidates {
            // Both auth dialects on every attempt: Anthropic wants
            // x-api-key, OpenAI-style wants a Bearer. Extra headers are
            // ignored by whichever side doesn't use them.
            let res = client
                .get(url)
                .header("x-api-key", key)
                .header("authorization", format!("Bearer {key}"))
                .header("anthropic-version", "2023-06-01")
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    let v: Value = r.json().await.context("response was not JSON")?;
                    if v["data"].as_array().is_some_and(|a| !a.is_empty()) {
                        return Ok(v);
                    }
                    tried.push(format!("{url} (200 but no model list)"));
                }
                Ok(r) => tried.push(format!("{url} ({})", r.status())),
                Err(e) => tried.push(format!("{url} ({e})")),
            }
        }
        bail!("no /models endpoint answered (tried: {})", tried.join(", "));
    })?;
    Ok(body["data"].as_array().cloned().unwrap_or_default())
}

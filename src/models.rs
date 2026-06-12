//! `elanus models` — ask the configured provider what models it serves
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
        .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "https://api.anthropic.com".into());
    let base = base.trim_end_matches('/');
    let url = if base.ends_with("/v1") { format!("{base}/models") } else { format!("{base}/v1/models") };
    let key_env = prof.model.api_key_env.clone().unwrap_or_else(|| "ANTHROPIC_API_KEY".into());
    let key = std::env::var(&key_env)
        .ok()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("no API key in ${key_env}"))?;

    let rt = tokio::runtime::Runtime::new()?;
    let body: Value = rt.block_on(async {
        let res = reqwest::Client::new()
            .get(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !res.status().is_success() {
            bail!("{url} answered {} (provider may not implement /models)", res.status());
        }
        res.json::<Value>().await.context("response was not JSON")
    })?;
    let models = body["data"].as_array().cloned().unwrap_or_default();
    if models.is_empty() {
        bail!("{url} answered but listed no models");
    }
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

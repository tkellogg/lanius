//! Model providers as a first-class, protected resource (docs/handoffs/model-providers.md, M1).
//!
//! A **provider** is a named credential plus the rule for *how to consume it*.
//! The credential is a **sum type** (`Credential`) whose variants are NOT valid
//! everywhere — an API key feeds both the genai dispatcher and a coding harness;
//! a "use the tool's own login" feeds only a harness, never the dispatcher — so
//! resolution is a **partial** function `materialize(credential, consumer)` that
//! fails closed with a legible message (the validity matrix is the crux).
//!
//! Storage is encrypted-at-rest in the ledger (`providers` table): CLEAR columns
//! for non-secret metadata (name, kind, wire, base_url, header NAMES, tool) so
//! `list`/`test`/UI are plain queries, and ONE encrypted blob (+ nonce) holding
//! the secret material (the API key + any secret header values). The master key
//! is a 32-byte file at `<root>/secret.key` (0600, generated on first use). The
//! secret is decrypted only transiently in memory at `materialize`/`test` time and
//! is NEVER written to logs, obs, the config git, or printed by the CLI.
//!
//! Scope of M1: the vault + the credential model + `materialize` + the
//! `elanus provider` CLI. No consumer is wired yet (M2 harness launch, M3
//! dispatcher build_client) — but `materialize` returns the right SHAPES so they
//! can be.

use crate::paths::Root;
use anyhow::{anyhow, bail, Context, Result};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fmt;

// ───────────────────────────── the model ─────────────────────────────

/// Which env vars / adapter / wire-format a credential speaks. Decides which
/// consumers can accept it (the validity matrix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Anthropic,
    OpenAI,
}

impl Wire {
    pub fn as_str(&self) -> &'static str {
        match self {
            Wire::Anthropic => "anthropic",
            Wire::OpenAI => "openai",
        }
    }
    pub fn parse(s: &str) -> Result<Wire> {
        match s.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Ok(Wire::Anthropic),
            "openai" => Ok(Wire::OpenAI),
            other => bail!("unknown wire {other:?} — expected 'anthropic' or 'openai'"),
        }
    }
}

/// The coding harnesses elanus can point at a provider. Mirrors the CLI ids in
/// src/codeagent.rs (`claude` | `codex` | `opencode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessId {
    Claude,
    Codex,
    Opencode,
}

impl HarnessId {
    pub fn as_str(&self) -> &'static str {
        match self {
            HarnessId::Claude => "claude",
            HarnessId::Codex => "codex",
            HarnessId::Opencode => "opencode",
        }
    }
    pub fn parse(s: &str) -> Result<HarnessId> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(HarnessId::Claude),
            "codex" => Ok(HarnessId::Codex),
            "opencode" => Ok(HarnessId::Opencode),
            other => bail!("unknown harness {other:?} — expected 'claude', 'codex', or 'opencode'"),
        }
    }
}

/// A secret string whose Debug/Display never reveal the contents. The plaintext
/// is reachable only through `expose()`, which every call site that hands it to a
/// consumer must use deliberately — so an accidental `{:?}` or log can't leak it.
// No Serialize/Deserialize: a Secret must never be emitted in the clear. The
// sealed `SecretBlob` (plain strings, AEAD-encrypted) is the only persistence path.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Secret {
        Secret(s.into())
    }
    /// The only way to read the plaintext. Deliberately verbose at call sites.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(••• redacted)")
    }
}

/// The redaction shown wherever a secret would otherwise be printed.
pub const REDACTED: &str = "••• (encrypted)";

/// A credential: KEY+URL (consumable everywhere) or "use the tool's own login"
/// (a harness only). See the validity matrix in `materialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    /// KEY + URL (+ optional extra headers: LiteLLM, OpenRouter, …). Encrypted at
    /// rest. Consumable by the genai dispatcher AND coding harnesses (per-harness
    /// injection).
    ApiKey {
        wire: Wire,
        base_url: String,
        key: Secret,
        /// Optional extra headers; values may be secret (so they ride the blob).
        headers: Vec<(String, Secret)>,
    },
    /// "Use the coding agent's own login; inject nothing." Today's default,
    /// *named* and *selectable*. No secret. Consumable by coding harnesses ONLY —
    /// the dispatcher can't touch it (there is no secret to feed a genai client).
    NativeLogin { tool: Option<HarnessId> },
}

impl Credential {
    /// The stored `kind` discriminant (clear column).
    #[allow(dead_code)] // `add` matches inline; kept as the canonical mapping
    pub fn kind(&self) -> &'static str {
        match self {
            Credential::ApiKey { .. } => "api_key",
            Credential::NativeLogin { .. } => "native_login",
        }
    }
}

/// A named provider: the resource the CLI/UI manages and a consumer references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
    pub credential: Credential,
}

/// Non-secret metadata for listing — enough to show a row without decrypting.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderMeta {
    pub name: String,
    pub kind: String,
    pub wire: Option<String>,
    pub base_url: Option<String>,
    pub tool: Option<String>,
    pub header_names: Vec<String>,
}

// ───────────────────────────── consumers & injection ─────────────────────────────
//
// M1 ships this surface and proves the matrix in isolation (tests below); the
// consumers that CALL `materialize` land in M2 (harness launch, src/codeagent.rs)
// and M3 (dispatcher build_client, src/exec.rs). Until then these are
// deliberately unwired — the `#[allow(dead_code)]` on `materialize` and its
// injection types records that, not neglect.

/// Who is asking to consume a credential. The validity matrix is keyed on this.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumer {
    /// The genai dispatcher (`build_client`, src/exec.rs). ApiKey only.
    Dispatcher,
    /// A coding harness launch (src/codeagent.rs).
    Harness(HarnessId),
}

/// What `materialize` produces. The two consumer families need different shapes,
/// so this is a sum — a single return type that keeps the partial function total
/// over its valid domain.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Injection {
    /// For M3's `build_client`: the literal coordinates to set endpoint/auth/headers.
    Dispatcher(DispatcherInjection),
    /// For M2's harness launch: env to set + args to append (per-tool shape).
    Harness(HarnessInjection),
}

/// The dispatcher coordinates — enough for genai's `ServiceTargetResolver` +
/// `with_extra_headers`. Carries the LITERAL key (decrypted transiently); never
/// logged.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatcherInjection {
    pub wire: Wire,
    pub base_url: String,
    pub key: Secret,
    pub headers: Vec<(String, Secret)>,
}

/// The per-tool harness injection: environment to set + CLI args to append after
/// the existing scrub. `NativeLogin` yields an empty injection (scrub-only).
#[allow(dead_code)]
#[derive(Clone, PartialEq, Eq, Default)]
pub struct HarnessInjection {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
}

impl fmt::Debug for HarnessInjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // env VALUES carry decrypted secrets (ANTHROPIC_AUTH_TOKEN, the codex
        // env_key value, the OPENCODE_CONFIG_CONTENT JSON). Redact them so a stray
        // `{:?}` in a consumer (M2) can't leak the key. Keys and args are non-secret
        // (they name env vars / select a provider), so they stay legible.
        f.debug_struct("HarnessInjection")
            .field(
                "env",
                &self
                    .env
                    .iter()
                    .map(|(k, _)| (k.as_str(), "•••"))
                    .collect::<Vec<_>>(),
            )
            .field("args", &self.args)
            .finish()
    }
}

// ───────────────────────────── materialize: the partial function ─────────────────────────────

/// The validity matrix, as a partial function. Returns the right `Injection` for
/// every valid `(variant, consumer)` and a legible refusal for every invalid one.
///
/// |                     | dispatcher | claude | codex | opencode |
/// |---------------------|------------|--------|-------|----------|
/// | `ApiKey{Anthropic}` | ✅ resolver | ✅ env | ❌    | ✅ config |
/// | `ApiKey{OpenAI}`    | ✅ resolver | ❌     | ✅ cfg | ✅ config |
/// | `NativeLogin`       | ❌          | ✅ scrub| ✅ scrub| ✅ scrub |
///
/// `model` is the launch's selected model (the harness `--model`/`-m` value), used
/// ONLY by the opencode harness arm: opencode needs an explicit `models` entry to
/// resolve a custom provider id (see that arm). It is ignored by every other
/// `(variant, consumer)` pair (claude/codex carry the model on their own flags; the
/// dispatcher names it on the profile).
#[allow(dead_code)] // called by M2 (harness) / M3 (dispatcher); proven by tests now
pub fn materialize(
    name: &str,
    cred: &Credential,
    consumer: Consumer,
    model: Option<&str>,
) -> Result<Injection> {
    match consumer {
        Consumer::Dispatcher => materialize_dispatcher(name, cred).map(Injection::Dispatcher),
        Consumer::Harness(h) => materialize_harness(name, cred, h, model).map(Injection::Harness),
    }
}

fn materialize_dispatcher(name: &str, cred: &Credential) -> Result<DispatcherInjection> {
    match cred {
        Credential::ApiKey {
            wire,
            base_url,
            key,
            headers,
        } => Ok(DispatcherInjection {
            wire: *wire,
            base_url: base_url.clone(),
            key: key.clone(),
            headers: headers.clone(),
        }),
        Credential::NativeLogin { .. } => bail!(
            "provider {name:?} is a native-login credential and can't drive the elanus dispatcher \
             (there is no secret to feed a genai client) — point the dispatcher at an ApiKey provider"
        ),
    }
}

fn materialize_harness(
    name: &str,
    cred: &Credential,
    h: HarnessId,
    model: Option<&str>,
) -> Result<HarnessInjection> {
    let (wire, base_url, key, headers) = match cred {
        Credential::NativeLogin { tool } => {
            // Scrub-only: the named default. A pin, if present, must match the
            // harness being launched — otherwise the selection is a mistake.
            if let Some(t) = tool {
                if *t != h {
                    bail!(
                        "provider {name:?} is a native-login pinned to {} — can't drive {}",
                        t.as_str(),
                        h.as_str()
                    );
                }
            }
            return Ok(HarnessInjection::default());
        }
        Credential::ApiKey {
            wire,
            base_url,
            key,
            headers,
        } => (*wire, base_url.as_str(), key, headers),
    };

    match (h, wire) {
        // ── claude: ANTHROPIC_BASE_URL + ANTHROPIC_AUTH_TOKEN env (overrides the
        // Claude.AI OAuth login). Anthropic-wire only.
        (HarnessId::Claude, Wire::Anthropic) => {
            // Claude Code has no generic extra-header mechanism at the env layer;
            // headers are a LiteLLM/dispatcher concern (wired in M3). They are not
            // silently mis-injected here.
            Ok(HarnessInjection {
                env: vec![
                    ("ANTHROPIC_BASE_URL".into(), base_url.to_string()),
                    ("ANTHROPIC_AUTH_TOKEN".into(), key.expose().to_string()),
                ],
                args: vec![],
            })
        }
        (HarnessId::Claude, Wire::OpenAI) => bail!(
            "provider {name:?} speaks the OpenAI wire — claude (Claude Code) needs an \
             Anthropic-wire provider"
        ),

        // ── codex: config, not env. OPENAI_BASE_URL is ignored and the built-in
        // `openai` id is locked, so inject a custom `model_provider` via `-c`
        // args and carry the secret(s) in env (env_key / env_http_headers) so no
        // secret ever lands on the command line. `responses`-wire only.
        (HarnessId::Codex, Wire::OpenAI) => {
            let id = name; // the custom model_provider id == the provider name
            let key_var = codex_key_var(name);
            let mut env = vec![(key_var.clone(), key.expose().to_string())];
            let mut args = vec![
                "-c".into(),
                // The id is a TOML *value* here, so it must be quoted: a hyphenated
                // name (`deepseek-anthropic`, allowed by `valid_name`) is NOT a valid
                // bare TOML scalar and a bare emit fails codex's `-c` value parse.
                // Quoting is also correct for the bare lowercase-alnum case (the
                // spike-proven one) — `"deepseek"` parses to the same string. The
                // dotted *key* segments below (`model_providers.<id>.…`) stay bare:
                // hyphens ARE valid in TOML bare keys, so the table key still matches.
                format!("model_provider={}", toml_str(id)),
                "-c".into(),
                // codex 0.141 REQUIRES a non-empty `name` on every custom provider
                // table — without it `codex exec` fails at config load with
                // "provider name must not be empty" (verified against codex-cli
                // 0.141.0). Use the provider id as the display name.
                format!("model_providers.{id}.name={}", toml_str(id)),
                "-c".into(),
                format!("model_providers.{id}.base_url={}", toml_str(base_url)),
                "-c".into(),
                format!("model_providers.{id}.wire_api={}", toml_str("responses")),
                "-c".into(),
                format!("model_providers.{id}.env_key={}", toml_str(&key_var)),
            ];
            for (i, (hn, hv)) in headers.iter().enumerate() {
                let hvar = codex_header_var(name, i);
                env.push((hvar.clone(), hv.expose().to_string()));
                args.push("-c".into());
                args.push(format!(
                    "model_providers.{id}.env_http_headers.{}={}",
                    toml_str(hn),
                    toml_str(&hvar)
                ));
            }
            Ok(HarnessInjection { env, args })
        }
        (HarnessId::Codex, Wire::Anthropic) => bail!(
            "provider {name:?} speaks the Anthropic wire — codex needs an OpenAI-wire \
             (responses) provider"
        ),

        // ── opencode: config outranks a stored login (config > auth.json > env),
        // so inject OPENCODE_CONFIG_CONTENT (inline JSON provider). Multi-wire: it
        // accepts both Anthropic and OpenAI ApiKey. The secret rides inside the env
        // var's JSON value, not the command line.
        //
        // A custom provider id must be FULLY defined — `options` alone is NOT
        // enough. opencode only auto-derives the AI-SDK loader (`npm`) and a model
        // catalog for a provider whose id is a known models.dev slug; for ANY other
        // id (the general case — `deepseek-test`, `litellm`, `openrouter`…) the
        // config must supply BOTH the `npm` SDK package AND an explicit `models`
        // entry for the selected model, or opencode raises
        // `ProviderModelNotFoundError: <id>/<model>` and never opens a connection.
        // Empirically verified against opencode 1.17.9: `options` only → 0 hits;
        // `npm` only → 0 hits; `npm` + `models.<model>` → routes (POST with the
        // injected Bearer). So inject all three.
        //
        // The model is the user's `--model <id>/<model>` tool arg (M2 threads it
        // here). opencode resolves the provider from the `<id>/` prefix and looks up
        // `<model>` in this provider's `models` map, so the map KEY is the part
        // AFTER the prefix.
        (HarnessId::Opencode, w) => {
            let id = name;
            let npm = match w {
                Wire::OpenAI => "@ai-sdk/openai-compatible",
                Wire::Anthropic => "@ai-sdk/anthropic",
            };
            let model = model.ok_or_else(|| {
                anyhow!(
                    "provider {name:?} drives opencode, which needs an explicit model to \
                     register the custom provider — pass `--model {name}/<model>` \
                     (opencode resolves the provider from the `<id>/` prefix and looks up \
                     `<model>` in the injected config)"
                )
            })?;
            // Strip the `<id>/` provider prefix if the user supplied the opencode
            // `provider/model` form; a bare model id is used as-is.
            let model_id = model
                .strip_prefix(&format!("{id}/"))
                .unwrap_or(model);
            let mut options = serde_json::Map::new();
            options.insert("baseURL".into(), serde_json::json!(base_url));
            options.insert("apiKey".into(), serde_json::json!(key.expose()));
            if !headers.is_empty() {
                let mut hmap = serde_json::Map::new();
                for (hn, hv) in headers {
                    hmap.insert(hn.clone(), serde_json::json!(hv.expose()));
                }
                options.insert("headers".into(), serde_json::Value::Object(hmap));
            }
            let mut models = serde_json::Map::new();
            models.insert(model_id.to_string(), serde_json::json!({}));
            let config = serde_json::json!({
                "provider": { id: {
                    "npm": npm,
                    "options": serde_json::Value::Object(options),
                    "models": serde_json::Value::Object(models),
                } }
            });
            Ok(HarnessInjection {
                env: vec![("OPENCODE_CONFIG_CONTENT".into(), config.to_string())],
                args: vec![],
            })
        }
    }
}

/// Sanitize a provider name into a SHOUTY env-var token (alnum + underscore).
fn env_token(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    t.to_ascii_uppercase()
}

fn codex_key_var(name: &str) -> String {
    format!("ELANUS_PV_{}_KEY", env_token(name))
}

fn codex_header_var(name: &str, i: usize) -> String {
    format!("ELANUS_PV_{}_H{i}", env_token(name))
}

/// Render a string as a double-quoted TOML scalar for a codex `-c key=value`
/// flag. codex parses the value as TOML, so a bare URL/word must be quoted.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ───────────────────────────── master key + AEAD ─────────────────────────────

const KEY_FILE: &str = "secret.key";

/// Read-or-generate the 32-byte master key at `<root>/secret.key` (0600,
/// generated on first use). Fail closed on a key of the wrong size.
fn master_key(root: &Root) -> Result<[u8; 32]> {
    let path = root.dir.join(KEY_FILE);
    if path.exists() {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading master key {}", path.display()))?;
        if bytes.len() != 32 {
            bail!(
                "master key {} is {} bytes, expected 32 — refusing to use a corrupt key",
                path.display(),
                bytes.len()
            );
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes);
        return Ok(k);
    }
    // First use: mint a fresh key and persist it 0600 before anything is sealed.
    let mut k = [0u8; 32];
    use chacha20poly1305::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut k);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    write_0600(&path, &k)
        .with_context(|| format!("writing master key {}", path.display()))?;
    Ok(k)
}

#[cfg(unix)]
fn write_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Seal plaintext under the master key: returns `(nonce[24], ciphertext)`. A
/// random per-call nonce is generated and returned beside the ciphertext.
fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| anyhow!("AEAD seal failed"))?;
    Ok((nonce.to_vec(), ct))
}

/// Open ciphertext sealed by `seal`. Fail closed: a wrong key, tampered nonce, or
/// tampered ciphertext yields an error, never garbage plaintext.
fn open(key: &[u8; 32], nonce: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 24 {
        bail!("stored nonce is {} bytes, expected 24 — refusing", nonce.len());
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::from_slice(nonce);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| anyhow!("AEAD open failed — wrong master key or tampered ciphertext"))
}

/// The serialized secret material that gets sealed into the blob: the API key and
/// any secret header values (paired with their names so a decrypt reconstructs the
/// full ordered headers). Never serialized in the clear anywhere else.
#[derive(Serialize, Deserialize)]
struct SecretBlob {
    key: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
}

// ───────────────────────────── the SQL vault ─────────────────────────────

/// Create the `providers` table. Idempotent; safe to call at every open.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
-- Model providers (docs/handoffs/model-providers.md). CLEAR columns hold only
-- non-secret metadata so list/test/UI are plain queries; the secret material
-- (API key + secret header values, serialized then XChaCha20-Poly1305 sealed)
-- lives ONLY in `secret` (+ its random `nonce`). A NativeLogin row carries no
-- blob. A raw `SELECT *` reveals no key — that is the threat model (accidental
-- disclosure: git/backups/obs/SELECT/screen-share), see docs/security.md.
CREATE TABLE IF NOT EXISTS providers (
  name         TEXT PRIMARY KEY,       -- the provider id (referenced by consumers)
  kind         TEXT NOT NULL,          -- api_key | native_login
  wire         TEXT,                   -- anthropic | openai (ApiKey only)
  base_url     TEXT,                   -- ApiKey only
  tool         TEXT,                   -- NativeLogin optional harness pin
  header_names TEXT,                   -- JSON array of header NAMES (clear; values are sealed)
  nonce        BLOB,                   -- 24-byte AEAD nonce (ApiKey only)
  secret       BLOB,                   -- sealed SecretBlob (ApiKey only)
  created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"#,
    )?;
    Ok(())
}

/// Provider names flow into env-var tokens (`ELANUS_PV_<NAME>_KEY`), codex TOML
/// keys (`model_providers.<name>`), opencode provider ids, and CLI args — so
/// restrict them to a safe, collision-free shape: lowercase alphanumeric + hyphen,
/// starting alphanumeric, ≤64 chars. Disallowing `_` keeps `env_token`'s `-`→`_`
/// mapping injective (no two names collide on one env var); disallowing `.`/space
/// keeps codex dotted-key parsing and the unquoted id intact.
fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        bail!(
            "invalid provider name {name:?} — use lowercase letters, digits, and hyphens \
             (must start alphanumeric, ≤64 chars)"
        );
    }
    Ok(())
}

/// Store (and encrypt) a provider. Overwrites an existing same-named provider.
pub fn add(root: &Root, conn: &Connection, provider: &Provider) -> Result<()> {
    valid_name(&provider.name)?;
    init_schema(conn)?;
    match &provider.credential {
        Credential::ApiKey {
            wire,
            base_url,
            key,
            headers,
        } => {
            let header_names: Vec<&str> = headers.iter().map(|(n, _)| n.as_str()).collect();
            let blob = SecretBlob {
                key: key.expose().to_string(),
                headers: headers
                    .iter()
                    .map(|(n, v)| (n.clone(), v.expose().to_string()))
                    .collect(),
            };
            let plaintext = serde_json::to_vec(&blob)?;
            let mkey = master_key(root)?;
            let (nonce, ct) = seal(&mkey, &plaintext)?;
            conn.execute(
                "INSERT INTO providers(name, kind, wire, base_url, tool, header_names, nonce, secret)
                 VALUES (?1, 'api_key', ?2, ?3, NULL, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                   kind='api_key', wire=?2, base_url=?3, tool=NULL,
                   header_names=?4, nonce=?5, secret=?6",
                rusqlite::params![
                    provider.name,
                    wire.as_str(),
                    base_url,
                    serde_json::to_string(&header_names)?,
                    nonce,
                    ct,
                ],
            )?;
        }
        Credential::NativeLogin { tool } => {
            conn.execute(
                "INSERT INTO providers(name, kind, wire, base_url, tool, header_names, nonce, secret)
                 VALUES (?1, 'native_login', NULL, NULL, ?2, NULL, NULL, NULL)
                 ON CONFLICT(name) DO UPDATE SET
                   kind='native_login', wire=NULL, base_url=NULL, tool=?2,
                   header_names=NULL, nonce=NULL, secret=NULL",
                rusqlite::params![provider.name, tool.map(|t| t.as_str())],
            )?;
        }
    }
    Ok(())
}

/// Non-secret metadata for every provider — never touches the blob.
pub fn list(conn: &Connection) -> Result<Vec<ProviderMeta>> {
    init_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT name, kind, wire, base_url, tool, header_names FROM providers ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| {
        let header_names: Option<String> = r.get(5)?;
        Ok(ProviderMeta {
            name: r.get(0)?,
            kind: r.get(1)?,
            wire: r.get(2)?,
            base_url: r.get(3)?,
            tool: r.get(4)?,
            header_names: header_names
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One provider's non-secret metadata (no decrypt).
pub fn get_meta(conn: &Connection, name: &str) -> Result<Option<ProviderMeta>> {
    Ok(list(conn)?.into_iter().find(|p| p.name == name))
}

/// Load a full provider, DECRYPTING the secret material transiently. The returned
/// `Credential` holds the plaintext key/headers in memory (a `Secret`) — hand it
/// straight to a consumer; never print it.
pub fn get(root: &Root, conn: &Connection, name: &str) -> Result<Option<Provider>> {
    init_schema(conn)?;
    let row = conn
        .query_row(
            "SELECT kind, wire, base_url, tool, nonce, secret FROM providers WHERE name=?1",
            [name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<Vec<u8>>>(4)?,
                    r.get::<_, Option<Vec<u8>>>(5)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((kind, wire, base_url, tool, nonce, secret)) = row else {
        return Ok(None);
    };
    let cred = match kind.as_str() {
        "native_login" => Credential::NativeLogin {
            tool: tool.as_deref().map(HarnessId::parse).transpose()?,
        },
        "api_key" => {
            let wire = Wire::parse(&wire.context("api_key row missing wire")?)?;
            let base_url = base_url.context("api_key row missing base_url")?;
            let nonce = nonce.context("api_key row missing nonce")?;
            let secret = secret.context("api_key row missing secret blob")?;
            let mkey = master_key(root)?;
            let plaintext = open(&mkey, &nonce, &secret)?;
            let blob: SecretBlob = serde_json::from_slice(&plaintext)
                .context("decrypted secret blob is not valid JSON")?;
            Credential::ApiKey {
                wire,
                base_url,
                key: Secret::new(blob.key),
                headers: blob
                    .headers
                    .into_iter()
                    .map(|(n, v)| (n, Secret::new(v)))
                    .collect(),
            }
        }
        other => bail!("provider {name:?} has unknown kind {other:?}"),
    };
    Ok(Some(Provider {
        name: name.to_string(),
        credential: cred,
    }))
}

/// Delete a provider. Returns whether a row was removed.
pub fn rm(conn: &Connection, name: &str) -> Result<bool> {
    init_schema(conn)?;
    let n = conn.execute("DELETE FROM providers WHERE name=?1", [name])?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests;

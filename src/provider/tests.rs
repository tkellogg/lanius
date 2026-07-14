//! M1 unit tests: the full validity matrix, encrypt/decrypt round-trip +
//! fail-closed on corruption, the per-harness HarnessInjection shapes, and that
//! the SQL store never exposes the secret in the clear.

use super::*;
use crate::paths::Root;
use rusqlite::Connection;

fn tmp_root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("el-prov-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    Root { dir }
}

fn api(wire: Wire) -> Credential {
    Credential::ApiKey {
        wire,
        base_url: "https://api.example.com".into(),
        key: Secret::new("sk-secret-123"),
        headers: vec![("X-LiteLLM".into(), Secret::new("hdr-secret"))],
    }
}

// ───────────────────────── the validity matrix ─────────────────────────

#[test]
fn matrix_dispatcher() {
    // ApiKey (either wire) -> Dispatcher OK, carries the literal key.
    for wire in [Wire::Anthropic, Wire::OpenAI] {
        let inj = materialize("p", &api(wire), Consumer::Dispatcher, None).unwrap();
        let Injection::Dispatcher(d) = inj else {
            panic!("expected dispatcher injection")
        };
        assert_eq!(d.wire, wire);
        assert_eq!(d.base_url, "https://api.example.com");
        assert_eq!(d.key.expose(), "sk-secret-123");
        assert_eq!(d.headers[0].0, "X-LiteLLM");
        assert_eq!(d.headers[0].1.expose(), "hdr-secret");
    }
    // NativeLogin -> Dispatcher REFUSED.
    let err = materialize(
        "chatgpt",
        &Credential::NativeLogin { tool: None },
        Consumer::Dispatcher,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("native-login"),
        "refusal must be legible: {err}"
    );
    assert!(err.contains("chatgpt"));
}

#[test]
fn matrix_claude() {
    // ApiKey{Anthropic} -> claude env injection.
    let inj = materialize(
        "ds",
        &api(Wire::Anthropic),
        Consumer::Harness(HarnessId::Claude),
        None,
    )
    .unwrap();
    let Injection::Harness(h) = inj else { panic!() };
    assert_eq!(
        h.env,
        vec![
            (
                "ANTHROPIC_BASE_URL".to_string(),
                "https://api.example.com".to_string()
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                "sk-secret-123".to_string()
            ),
        ]
    );
    assert!(h.args.is_empty());
    // ApiKey{OpenAI} -> claude REFUSED (wire mismatch).
    let err = materialize(
        "oai",
        &api(Wire::OpenAI),
        Consumer::Harness(HarnessId::Claude),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("OpenAI wire"), "{err}");
}

#[test]
fn matrix_codex() {
    // ApiKey{OpenAI} -> codex -c custom-provider args + secret in env.
    let inj = materialize(
        "ds",
        &api(Wire::OpenAI),
        Consumer::Harness(HarnessId::Codex),
        None,
    )
    .unwrap();
    let Injection::Harness(h) = inj else { panic!() };
    // The key rides env (off the command line), named by env_key.
    assert!(h
        .env
        .contains(&("LANIUS_PV_DS_KEY".to_string(), "sk-secret-123".to_string())));
    // The secret header value also rides env.
    assert!(h
        .env
        .contains(&("LANIUS_PV_DS_H0".to_string(), "hdr-secret".to_string())));
    // The -c flags select a custom provider, never the literal key.
    let joined = h.args.join(" ");
    // The model_provider VALUE is quoted (TOML scalar; required for hyphenated ids,
    // safe for bare ones). The dotted KEY segments stay bare.
    assert!(joined.contains("model_provider=\"ds\""));
    // codex 0.141 requires a non-empty provider `name` (else config load fails).
    assert!(joined.contains("model_providers.ds.name=\"ds\""));
    assert!(joined.contains("model_providers.ds.base_url=\"https://api.example.com\""));
    assert!(joined.contains("model_providers.ds.wire_api=\"responses\""));
    assert!(joined.contains("model_providers.ds.env_key=\"LANIUS_PV_DS_KEY\""));
    assert!(
        joined.contains("model_providers.ds.env_http_headers.\"X-LiteLLM\"=\"LANIUS_PV_DS_H0\"")
    );
    assert!(
        !joined.contains("sk-secret-123"),
        "secret must never be a -c arg"
    );
    // ApiKey{Anthropic} -> codex REFUSED (wire mismatch).
    let err = materialize(
        "ds",
        &api(Wire::Anthropic),
        Consumer::Harness(HarnessId::Codex),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("Anthropic wire"), "{err}");
}

#[test]
fn matrix_opencode() {
    // opencode is multi-wire: accepts both Anthropic and OpenAI ApiKey via config.
    // The custom provider id must be FULLY defined — `npm` (the AI-SDK loader) AND
    // an explicit `models.<model>` entry — or opencode raises
    // ProviderModelNotFoundError and never connects (verified vs opencode 1.17.9).
    for (wire, npm) in [
        (Wire::Anthropic, "@ai-sdk/anthropic"),
        (Wire::OpenAI, "@ai-sdk/openai-compatible"),
    ] {
        // The user passes `--model <id>/<model>`; the `models` key is the part after
        // the `<id>/` prefix.
        let inj = materialize(
            "ds",
            &api(wire),
            Consumer::Harness(HarnessId::Opencode),
            Some("ds/gpt-4o-mini"),
        )
        .unwrap();
        let Injection::Harness(h) = inj else { panic!() };
        assert_eq!(h.env.len(), 1);
        assert_eq!(h.env[0].0, "OPENCODE_CONFIG_CONTENT");
        let cfg: serde_json::Value = serde_json::from_str(&h.env[0].1).unwrap();
        let prov = &cfg["provider"]["ds"];
        assert_eq!(prov["npm"], npm, "the AI-SDK loader must be declared");
        let opts = &prov["options"];
        assert_eq!(opts["baseURL"], "https://api.example.com");
        assert_eq!(opts["apiKey"], "sk-secret-123");
        assert_eq!(opts["headers"]["X-LiteLLM"], "hdr-secret");
        // The selected model must be registered (prefix stripped) so opencode can
        // resolve `ds/gpt-4o-mini` against this custom id.
        assert!(
            prov["models"]["gpt-4o-mini"].is_object(),
            "the selected model must be registered: {prov}"
        );
    }
    // A bare model id (no `<id>/` prefix) is registered as-is.
    let inj = materialize(
        "ds",
        &api(Wire::OpenAI),
        Consumer::Harness(HarnessId::Opencode),
        Some("gpt-4o-mini"),
    )
    .unwrap();
    let Injection::Harness(h) = inj else { panic!() };
    let cfg: serde_json::Value = serde_json::from_str(&h.env[0].1).unwrap();
    assert!(cfg["provider"]["ds"]["models"]["gpt-4o-mini"].is_object());

    // No model → legible refusal (opencode can't resolve a custom id without one).
    let err = materialize(
        "ds",
        &api(Wire::OpenAI),
        Consumer::Harness(HarnessId::Opencode),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("needs an explicit model"), "{err}");
}

#[test]
fn matrix_native_login_is_scrub_only_for_every_harness() {
    for h in [HarnessId::Claude, HarnessId::Codex, HarnessId::Opencode] {
        let inj = materialize(
            "login",
            &Credential::NativeLogin { tool: None },
            Consumer::Harness(h),
            None,
        )
        .unwrap();
        assert_eq!(
            inj,
            Injection::Harness(HarnessInjection::default()),
            "empty injection"
        );
    }
}

#[test]
fn native_login_pin_must_match_harness() {
    let cred = Credential::NativeLogin {
        tool: Some(HarnessId::Claude),
    };
    // Matching harness -> empty injection.
    assert_eq!(
        materialize("l", &cred, Consumer::Harness(HarnessId::Claude), None).unwrap(),
        Injection::Harness(HarnessInjection::default())
    );
    // Mismatched harness -> legible refusal.
    let err = materialize("l", &cred, Consumer::Harness(HarnessId::Codex), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("pinned to claude"), "{err}");
}

// ───────────────────────── crypto round-trip + fail-closed ─────────────────────────

#[test]
fn encrypt_decrypt_round_trip() {
    let root = tmp_root("crypt");
    let key = master_key(&root).unwrap();
    let pt = b"the launch codes";
    let (nonce, ct) = seal(&key, pt).unwrap();
    assert_eq!(nonce.len(), 24);
    assert_ne!(ct, pt, "ciphertext must differ from plaintext");
    assert_eq!(open(&key, &nonce, &ct).unwrap(), pt);
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn master_key_is_stable_and_0600() {
    let root = tmp_root("mkey");
    let k1 = master_key(&root).unwrap();
    let k2 = master_key(&root).unwrap();
    assert_eq!(k1, k2, "the key persists across reads");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.dir.join("secret.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "master key must be 0600");
    }
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn decrypt_fails_closed_on_corruption() {
    let root = tmp_root("corrupt");
    let key = master_key(&root).unwrap();
    let (nonce, mut ct) = seal(&key, b"secret").unwrap();
    // Flip a ciphertext byte: the AEAD tag must reject it (no garbage plaintext).
    ct[0] ^= 0xff;
    assert!(
        open(&key, &nonce, &ct).is_err(),
        "tampered ciphertext must fail"
    );
    // A wrong key must also fail.
    let mut wrong = key;
    wrong[0] ^= 0xff;
    let (n2, c2) = seal(&key, b"secret").unwrap();
    assert!(open(&wrong, &n2, &c2).is_err(), "wrong key must fail");
    std::fs::remove_dir_all(&root.dir).ok();
}

// ───────────────────────── the SQL vault ─────────────────────────

#[test]
fn vault_round_trip_and_no_plaintext_at_rest() {
    let root = tmp_root("vault");
    let conn = Connection::open(root.db()).unwrap();
    let p = Provider {
        name: "deepseek".into(),
        credential: api(Wire::Anthropic),
    };
    add(&root, &conn, &p).unwrap();

    // get() decrypts back to the original.
    let got = get(&root, &conn, "deepseek").unwrap().unwrap();
    assert_eq!(got, p);

    // A raw SELECT * reveals no key and no header value in the clear.
    let (wire, base_url, names, secret): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Vec<u8>>,
    ) = conn
        .query_row(
            "SELECT wire, base_url, header_names, secret FROM providers WHERE name='deepseek'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(wire.as_deref(), Some("anthropic"));
    assert_eq!(base_url.as_deref(), Some("https://api.example.com"));
    assert_eq!(names.as_deref(), Some(r#"["X-LiteLLM"]"#)); // header NAME is clear
    let blob = secret.unwrap();
    let hay = String::from_utf8_lossy(&blob);
    assert!(
        !hay.contains("sk-secret-123"),
        "key must not be in the blob in clear"
    );
    assert!(
        !hay.contains("hdr-secret"),
        "header value must not be in the blob in clear"
    );

    // list() / get_meta() never carry the secret.
    let meta = get_meta(&conn, "deepseek").unwrap().unwrap();
    assert_eq!(meta.kind, "api_key");
    assert_eq!(meta.header_names, vec!["X-LiteLLM".to_string()]);

    // rm() deletes.
    assert!(rm(&conn, "deepseek").unwrap());
    assert!(get(&root, &conn, "deepseek").unwrap().is_none());
    assert!(!rm(&conn, "deepseek").unwrap());
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn vault_native_login_carries_no_blob() {
    let root = tmp_root("vaultnl");
    let conn = Connection::open(root.db()).unwrap();
    add(
        &root,
        &conn,
        &Provider {
            name: "chatgpt".into(),
            credential: Credential::NativeLogin {
                tool: Some(HarnessId::Codex),
            },
        },
    )
    .unwrap();
    let secret: Option<Vec<u8>> = conn
        .query_row(
            "SELECT secret FROM providers WHERE name='chatgpt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(secret.is_none(), "native-login row must carry no blob");
    let got = get(&root, &conn, "chatgpt").unwrap().unwrap();
    assert_eq!(
        got.credential,
        Credential::NativeLogin {
            tool: Some(HarnessId::Codex)
        }
    );
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn corrupt_master_key_size_fails_closed() {
    let root = tmp_root("badkey");
    std::fs::write(root.dir.join("secret.key"), b"too short").unwrap();
    assert!(
        master_key(&root).is_err(),
        "a wrong-size key must be refused"
    );
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn add_rejects_unsafe_names() {
    let root = tmp_root("badname");
    let conn = Connection::open(root.db()).unwrap();
    // Dots break codex dotted keys; underscores/uppercase/spaces collide or break
    // env tokens; a leading hyphen and the empty name are malformed.
    for bad in [
        "deep.seek",
        "Deep",
        "deep seek",
        "deep_seek",
        "-x",
        "",
        "a/b",
    ] {
        let p = Provider {
            name: bad.into(),
            credential: api(Wire::Anthropic),
        };
        assert!(
            add(&root, &conn, &p).is_err(),
            "name {bad:?} must be rejected"
        );
    }
    // A clean hyphenated name is accepted.
    let p = Provider {
        name: "deepseek-anthropic".into(),
        credential: api(Wire::OpenAI),
    };
    assert!(add(&root, &conn, &p).is_ok());
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn get_fails_closed_on_corrupt_blob() {
    let root = tmp_root("getcorrupt");
    let conn = Connection::open(root.db()).unwrap();
    add(
        &root,
        &conn,
        &Provider {
            name: "ds".into(),
            credential: api(Wire::OpenAI),
        },
    )
    .unwrap();
    // Corrupt the stored ciphertext: get() must error, never return garbage.
    conn.execute(
        "UPDATE providers SET secret=?1 WHERE name='ds'",
        rusqlite::params![vec![0u8, 1, 2, 3, 4]],
    )
    .unwrap();
    assert!(
        get(&root, &conn, "ds").is_err(),
        "a corrupt blob must fail closed"
    );
    std::fs::remove_dir_all(&root.dir).ok();
}

// ───────────────────── M3: package secrets (telegram-bridge.md) ─────────────────────

#[test]
fn package_secret_round_trip_and_ciphertext_not_plaintext() {
    let root = tmp_root("pkgsecret");
    let conn = Connection::open(root.db()).unwrap();
    set_package_secret(&root, &conn, "telegram", "TELEGRAM_TOKEN", "bot-secret-abc").unwrap();

    // Round trip returns the exact plaintext.
    let got = get_package_secret(&root, &conn, "telegram", "TELEGRAM_TOKEN").unwrap();
    assert_eq!(got.as_deref(), Some("bot-secret-abc"));

    // The stored blob is NOT the plaintext bytes.
    let stored: Vec<u8> = conn
        .query_row(
            "SELECT secret FROM package_secrets WHERE package='telegram' AND key='TELEGRAM_TOKEN'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(stored, b"bot-secret-abc".to_vec());

    // A second key for the same package coexists.
    set_package_secret(&root, &conn, "telegram", "OTHER_KEY", "other-value").unwrap();
    let names = list_package_secrets(&conn, "telegram").unwrap();
    assert_eq!(names, vec!["OTHER_KEY".to_string(), "TELEGRAM_TOKEN".to_string()]);
    // list never carries the value.
    for n in &names {
        assert!(n != "bot-secret-abc" && n != "other-value");
    }

    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn package_secret_absent_returns_none() {
    let root = tmp_root("pkgsecret-absent");
    let conn = Connection::open(root.db()).unwrap();
    let got = get_package_secret(&root, &conn, "telegram", "TELEGRAM_TOKEN").unwrap();
    assert!(got.is_none());
    std::fs::remove_dir_all(&root.dir).ok();
}

#[test]
fn package_secret_fails_closed_on_corrupt_blob() {
    let root = tmp_root("pkgsecret-corrupt");
    let conn = Connection::open(root.db()).unwrap();
    set_package_secret(&root, &conn, "telegram", "TELEGRAM_TOKEN", "bot-secret-abc").unwrap();
    conn.execute(
        "UPDATE package_secrets SET secret=?1 WHERE package='telegram' AND key='TELEGRAM_TOKEN'",
        rusqlite::params![vec![0u8, 1, 2, 3, 4]],
    )
    .unwrap();
    assert!(
        get_package_secret(&root, &conn, "telegram", "TELEGRAM_TOKEN").is_err(),
        "a tampered package secret must fail closed"
    );
    std::fs::remove_dir_all(&root.dir).ok();
}

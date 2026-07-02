fn main() -> anyhow::Result<()> {
    let ctx = elanus::harness::Ctx::from_env()?;
    ctx.emit("session/start", serde_json::json!({ "adapter": "echo" }));
    let echoed = ctx.prompt().unwrap_or("").to_string();
    ctx.emit("assistant/message", serde_json::json!({ "text": echoed }));
    ctx.claim(&format!("{}/ECHO_ADAPTER_RAN", ctx.workdir().display()));
    ctx.emit("session/idle", serde_json::json!({}));
    // Test hook (cross-harness-death e2e): a caller can force a nonzero exit so the
    // detached-spawn FAILURE path can be exercised end to end with the stub adapter.
    // Absent the var this is a no-op — the normal success path is unchanged.
    if let Ok(code) = std::env::var("ELANUS_HARNESS_ECHO_EXIT") {
        if let Ok(code) = code.parse::<i32>() {
            std::process::exit(code);
        }
    }
    Ok(())
}

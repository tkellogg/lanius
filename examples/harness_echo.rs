fn main() -> anyhow::Result<()> {
    let ctx = elanus::harness::Ctx::from_env()?;
    ctx.emit("session/start", serde_json::json!({ "adapter": "echo" }));
    let echoed = ctx.prompt().unwrap_or("").to_string();
    ctx.emit("assistant/message", serde_json::json!({ "text": echoed }));
    ctx.claim(&format!("{}/ECHO_ADAPTER_RAN", ctx.workdir().display()));
    ctx.emit("session/idle", serde_json::json!({}));
    Ok(())
}

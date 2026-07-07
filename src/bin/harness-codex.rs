fn main() -> anyhow::Result<()> {
    let ctx = lanius::harness::Ctx::from_env()?;
    let status = lanius::codeagent::run_codex_adapter(&ctx)?;
    std::process::exit(status.code().unwrap_or(0));
}

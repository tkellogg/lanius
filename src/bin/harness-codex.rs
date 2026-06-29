fn main() -> anyhow::Result<()> {
    let ctx = elanus::harness::Ctx::from_env()?;
    let status = elanus::codeagent::run_codex_adapter(&ctx)?;
    std::process::exit(status.code().unwrap_or(0));
}

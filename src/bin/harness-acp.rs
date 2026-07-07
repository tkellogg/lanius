fn main() -> anyhow::Result<()> {
    let ctx = lanius::harness::Ctx::from_env()?;
    let status = lanius::acp::run_acp_adapter(&ctx)?;
    std::process::exit(status.code().unwrap_or(0));
}

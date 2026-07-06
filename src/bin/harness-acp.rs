fn main() -> anyhow::Result<()> {
    let ctx = elanus::harness::Ctx::from_env()?;
    let status = elanus::acp::run_acp_adapter(&ctx)?;
    std::process::exit(status.code().unwrap_or(0));
}

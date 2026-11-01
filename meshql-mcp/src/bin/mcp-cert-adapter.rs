#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("backend or stdio required"))?;
    meshql_mcp::cert_adapter(&mode, meshql_mcp::cli::options(args)?).await
}

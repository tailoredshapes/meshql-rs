#[tokio::main]
async fn main() -> anyhow::Result<()> {
    meshql_mcp::cli::run(meshql_mcp::cli::options(std::env::args().skip(1))?).await
}

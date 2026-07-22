use meshql_server::run_ext;

const MONGO_URI: &str = "mongodb://127.0.0.1:27017";
const DB_NAME: &str = "farm_db";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (config, extra) = farm::build(MONGO_URI, DB_NAME).await?;
    run_ext(config, extra).await
}

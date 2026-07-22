use meshql_server::run_ext;

const MONGO_URI: &str = "mongodb://127.0.0.1:27017";
const DB_NAME: &str = "farm_db";
const MANIFEST_JSON: &str = include_str!("../config/manifest.json");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (config, extra) = farm::build(MONGO_URI, DB_NAME).await?;
    let extra = extra.route(
        "/manifest",
        axum::routing::get(move || async move {
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                MANIFEST_JSON,
            )
        }),
    );
    run_ext(config, extra).await
}

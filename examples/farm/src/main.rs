use meshql_core::{GraphletteConfig, NoAuth, RestletteConfig, RootConfig, ServerConfig};
use meshql_mongo::{MongoRepository, MongoSearcher};
use meshql_server::run;
use std::sync::Arc;

const MONGO_URI: &str = "mongodb://127.0.0.1:27017";
const DB_NAME: &str = "farm_db";

const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);

    // --- Repositories ---
    let farm_repo = Arc::new(
        MongoRepository::new(MONGO_URI, DB_NAME, "farms", Arc::clone(&auth)).await?,
    );
    let coop_repo = Arc::new(
        MongoRepository::new(MONGO_URI, DB_NAME, "coops", Arc::clone(&auth)).await?,
    );
    let hen_repo = Arc::new(
        MongoRepository::new(MONGO_URI, DB_NAME, "hens", Arc::clone(&auth)).await?,
    );
    let lay_report_repo = Arc::new(
        MongoRepository::new(MONGO_URI, DB_NAME, "lay_reports", Arc::clone(&auth)).await?,
    );

    // --- Searchers ---
    let farm_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(MONGO_URI, DB_NAME, "farms", Arc::clone(&auth)).await?,
    );
    let coop_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(MONGO_URI, DB_NAME, "coops", Arc::clone(&auth)).await?,
    );
    let hen_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(MONGO_URI, DB_NAME, "hens", Arc::clone(&auth)).await?,
    );
    let lay_report_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(MONGO_URI, DB_NAME, "lay_reports", Arc::clone(&auth)).await?,
    );

    // --- Root Configs ---
    let farm_config = RootConfig::builder()
        .singleton("getFarm", r#"{"id": "{{id}}"}"#)
        .vector("getFarms", r#"{"name": "{{name}}"}"#)
        // Farms have coops (resolved via coop graphlette)
        .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getCoop", r#"{"id": "{{id}}"}"#)
        .vector("getCoops", r#"{"name": "{{name}}"}"#)
        .vector("getCoopsByFarm", r#"{"farmId": "{{id}}"}"#)
        // Coops have a farm (resolved via farm graphlette)
        .singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
        // Coops have hens (resolved via hen graphlette)
        .vector_resolver("hens", None, "getHensByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getHen", r#"{"id": "{{id}}"}"#)
        .vector("getHens", r#"{"name": "{{name}}"}"#)
        .vector("getHensByCoop", r#"{"coopId": "{{id}}"}"#)
        // Hens have a coop (resolved via coop graphlette)
        .singleton_resolver("coop", Some("coopId"), "getCoop", "/coop/graph")
        // Hens have lay reports (resolved via lay_report graphlette)
        .vector_resolver("layReports", None, "getLayReportsByHen", "/lay_report/graph")
        .build();

    let lay_report_config = RootConfig::builder()
        .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
        .vector("getLayReports", r#"{"date": "{{date}}"}"#)
        .vector("getLayReportsByHen", r#"{"henId": "{{id}}"}"#)
        // Lay reports have a hen (resolved via hen graphlette)
        .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
        .build();

    let config = ServerConfig {
        port: 3033,
        graphlettes: vec![
            GraphletteConfig {
                path: "/farm/graph".to_string(),
                schema_text: FARM_GRAPHQL.to_string(),
                root_config: farm_config,
                searcher: farm_searcher,
            },
            GraphletteConfig {
                path: "/coop/graph".to_string(),
                schema_text: COOP_GRAPHQL.to_string(),
                root_config: coop_config,
                searcher: coop_searcher,
            },
            GraphletteConfig {
                path: "/hen/graph".to_string(),
                schema_text: HEN_GRAPHQL.to_string(),
                root_config: hen_config,
                searcher: hen_searcher,
            },
            GraphletteConfig {
                path: "/lay_report/graph".to_string(),
                schema_text: LAY_REPORT_GRAPHQL.to_string(),
                root_config: lay_report_config,
                searcher: lay_report_searcher,
            },
        ],
        restlettes: vec![
            RestletteConfig {
                path: "/farm".to_string(),
                schema_json: serde_json::json!({}),
                repository: farm_repo,
            },
            RestletteConfig {
                path: "/coop".to_string(),
                schema_json: serde_json::json!({}),
                repository: coop_repo,
            },
            RestletteConfig {
                path: "/hen".to_string(),
                schema_json: serde_json::json!({}),
                repository: hen_repo,
            },
            RestletteConfig {
                path: "/lay_report".to_string(),
                schema_json: serde_json::json!({}),
                repository: lay_report_repo,
            },
        ],
    };

    run(config).await
}

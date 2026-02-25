use merkql::broker::{Broker, BrokerConfig};
use meshql_core::{GraphletteConfig, RestletteConfig, RootConfig, ServerConfig};
use meshql_merkql::{MerkqlRepository, MerkqlSearcher};
use std::path::PathBuf;
use std::sync::Arc;

// --- GraphQL schemas (4) ---
const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");

// --- JSON schemas (4) ---
const FARM_JSON: &str = include_str!("../config/json/farm.schema.json");
const COOP_JSON: &str = include_str!("../config/json/coop.schema.json");
const HEN_JSON: &str = include_str!("../config/json/hen.schema.json");
const LAY_REPORT_JSON: &str = include_str!("../config/json/lay_report.schema.json");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Azure Functions custom handler port, or default for local dev
    let port: u16 = std::env::var("FUNCTIONS_CUSTOMHANDLER_PORT")
        .unwrap_or_else(|_| "3000".into())
        .parse()
        .expect("FUNCTIONS_CUSTOMHANDLER_PORT must be a valid port number");

    // merkql data directory — Azure Files NFS mount, or local for dev
    let data_path = std::env::var("MERKQL_DATA_PATH").unwrap_or_else(|_| "/mnt/merkql".to_string());

    let broker = Broker::open(BrokerConfig::new(PathBuf::from(&data_path)))?;

    // ===== REPOSITORIES (4) =====
    let farm_repo = Arc::new(MerkqlRepository::new(broker.clone(), "farm"));
    let coop_repo = Arc::new(MerkqlRepository::new(broker.clone(), "coop"));
    let hen_repo = Arc::new(MerkqlRepository::new(broker.clone(), "hen"));
    let lay_report_repo = Arc::new(MerkqlRepository::new(broker.clone(), "lay_report"));

    // ===== SEARCHERS (4) =====
    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "farm"));
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "coop"));
    let hen_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "hen"));
    let lay_report_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "lay_report"));

    // ===== ROOT CONFIGS (4) =====
    // merkql stores payload fields nested under "payload.", so we use internal resolvers
    // and payload-prefixed templates.

    let farm_config = RootConfig::builder()
        .singleton("getFarm", r#"{"id": "{{id}}"}"#)
        .vector("getFarms", "{}")
        .internal_vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getCoop", r#"{"id": "{{id}}"}"#)
        .vector("getCoops", "{}")
        .vector("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)
        .internal_singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
        .internal_vector_resolver("hens", None, "getHensByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getHen", r#"{"id": "{{id}}"}"#)
        .vector("getHens", "{}")
        .vector("getHensByCoop", r#"{"payload.coopId": "{{id}}"}"#)
        .internal_singleton_resolver("coop", Some("coopId"), "getCoop", "/coop/graph")
        .internal_vector_resolver(
            "layReports",
            None,
            "getLayReportsByHen",
            "/lay_report/graph",
        )
        .build();

    let lay_report_config = RootConfig::builder()
        .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
        .vector("getLayReports", "{}")
        .vector("getLayReportsByHen", r#"{"payload.henId": "{{id}}"}"#)
        .internal_singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
        .build();

    // ===== SERVER CONFIG =====
    let config = ServerConfig {
        port,
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
                path: "/farm/api".to_string(),
                schema_json: serde_json::from_str(FARM_JSON).expect("invalid farm JSON schema"),
                repository: farm_repo,
            },
            RestletteConfig {
                path: "/coop/api".to_string(),
                schema_json: serde_json::from_str(COOP_JSON).expect("invalid coop JSON schema"),
                repository: coop_repo,
            },
            RestletteConfig {
                path: "/hen/api".to_string(),
                schema_json: serde_json::from_str(HEN_JSON).expect("invalid hen JSON schema"),
                repository: hen_repo,
            },
            RestletteConfig {
                path: "/lay_report/api".to_string(),
                schema_json: serde_json::from_str(LAY_REPORT_JSON)
                    .expect("invalid lay_report JSON schema"),
                repository: lay_report_repo,
            },
        ],
    };

    meshql_server::run(config).await
}

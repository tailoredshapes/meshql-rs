use merkql::broker::{Broker, BrokerConfig};
use meshql_core::{GraphletteConfig, NoAuth, RestletteConfig, RootConfig, ServerConfig};
use meshql_merkql::{MerkqlRepository, MerkqlSearcher};
use std::path::PathBuf;
use std::sync::Arc;

// --- GraphQL schemas (13) ---

// Actors
const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const CONTAINER_GRAPHQL: &str = include_str!("../config/graph/container.graphql");
const CONSUMER_GRAPHQL: &str = include_str!("../config/graph/consumer.graphql");

// Events
const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");
const STORAGE_DEPOSIT_GRAPHQL: &str = include_str!("../config/graph/storage_deposit.graphql");
const STORAGE_WITHDRAWAL_GRAPHQL: &str = include_str!("../config/graph/storage_withdrawal.graphql");
const CONTAINER_TRANSFER_GRAPHQL: &str = include_str!("../config/graph/container_transfer.graphql");
const CONSUMPTION_REPORT_GRAPHQL: &str = include_str!("../config/graph/consumption_report.graphql");

// Projections
const CONTAINER_INVENTORY_GRAPHQL: &str =
    include_str!("../config/graph/container_inventory.graphql");
const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");
const FARM_OUTPUT_GRAPHQL: &str = include_str!("../config/graph/farm_output.graphql");

// --- JSON schemas (13) ---

// Actors
const FARM_JSON: &str = include_str!("../config/json/farm.schema.json");
const COOP_JSON: &str = include_str!("../config/json/coop.schema.json");
const HEN_JSON: &str = include_str!("../config/json/hen.schema.json");
const CONTAINER_JSON: &str = include_str!("../config/json/container.schema.json");
const CONSUMER_JSON: &str = include_str!("../config/json/consumer.schema.json");

// Events
const LAY_REPORT_JSON: &str = include_str!("../config/json/lay_report.schema.json");
const STORAGE_DEPOSIT_JSON: &str = include_str!("../config/json/storage_deposit.schema.json");
const STORAGE_WITHDRAWAL_JSON: &str = include_str!("../config/json/storage_withdrawal.schema.json");
const CONTAINER_TRANSFER_JSON: &str = include_str!("../config/json/container_transfer.schema.json");
const CONSUMPTION_REPORT_JSON: &str = include_str!("../config/json/consumption_report.schema.json");

// Projections
const CONTAINER_INVENTORY_JSON: &str =
    include_str!("../config/json/container_inventory.schema.json");
const HEN_PRODUCTIVITY_JSON: &str = include_str!("../config/json/hen_productivity.schema.json");
const FARM_OUTPUT_JSON: &str = include_str!("../config/json/farm_output.schema.json");

#[tokio::main]
async fn main() -> Result<(), lambda_http::Error> {
    let efs_path = std::env::var("EFS_MOUNT_PATH").unwrap_or_else(|_| "/mnt/efs".to_string());

    let broker = Broker::open(BrokerConfig::new(PathBuf::from(&efs_path)))
        .map_err(|e| lambda_http::Error::from(format!("broker open failed: {e}")))?;

    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);
    let _ = auth; // NoAuth used implicitly via MerkqlRepository

    // ===== REPOSITORIES (13) =====

    // Actors
    let farm_repo = Arc::new(MerkqlRepository::new(broker.clone(), "farm"));
    let coop_repo = Arc::new(MerkqlRepository::new(broker.clone(), "coop"));
    let hen_repo = Arc::new(MerkqlRepository::new(broker.clone(), "hen"));
    let container_repo = Arc::new(MerkqlRepository::new(broker.clone(), "container"));
    let consumer_repo = Arc::new(MerkqlRepository::new(broker.clone(), "consumer"));

    // Events
    let lay_report_repo = Arc::new(MerkqlRepository::new(broker.clone(), "lay_report"));
    let storage_deposit_repo = Arc::new(MerkqlRepository::new(broker.clone(), "storage_deposit"));
    let storage_withdrawal_repo =
        Arc::new(MerkqlRepository::new(broker.clone(), "storage_withdrawal"));
    let container_transfer_repo =
        Arc::new(MerkqlRepository::new(broker.clone(), "container_transfer"));
    let consumption_report_repo =
        Arc::new(MerkqlRepository::new(broker.clone(), "consumption_report"));

    // Projections
    let container_inventory_repo =
        Arc::new(MerkqlRepository::new(broker.clone(), "container_inventory"));
    let hen_productivity_repo = Arc::new(MerkqlRepository::new(broker.clone(), "hen_productivity"));
    let farm_output_repo = Arc::new(MerkqlRepository::new(broker.clone(), "farm_output"));

    // ===== SEARCHERS (13) =====

    // Actors
    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "farm"));
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "coop"));
    let hen_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "hen"));
    let container_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "container"));
    let consumer_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "consumer"));

    // Events
    let lay_report_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "lay_report"));
    let storage_deposit_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "storage_deposit"));
    let storage_withdrawal_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "storage_withdrawal"));
    let container_transfer_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "container_transfer"));
    let consumption_report_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "consumption_report"));

    // Projections
    let container_inventory_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "container_inventory"));
    let hen_productivity_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "hen_productivity"));
    let farm_output_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MerkqlSearcher::new(broker.clone(), "farm_output"));

    // ===== ROOT CONFIGS (13) =====

    // --- Actors ---

    let farm_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
        .internal_vector_resolver("coops", None, "getByFarm", "/coop/graph")
        .internal_vector_resolver("farmOutput", None, "getByFarm", "/farm_output/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
        .internal_vector_resolver("hens", None, "getByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByCoop", r#"{"payload.coop_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver("coop", Some("coop_id"), "getById", "/coop/graph")
        .internal_vector_resolver("layReports", None, "getByHen", "/lay_report/graph")
        .internal_vector_resolver("productivity", None, "getByHen", "/hen_productivity/graph")
        .build();

    let container_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
        .internal_vector_resolver(
            "inventory",
            None,
            "getByContainer",
            "/container_inventory/graph",
        )
        .build();

    let consumer_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
        .internal_vector_resolver(
            "consumptionReports",
            None,
            "getByConsumer",
            "/consumption_report/graph",
        )
        .build();

    // --- Events ---

    let lay_report_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByHen", r#"{"payload.hen_id": "{{id}}"}"#)
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph")
        .build();

    let storage_deposit_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver(
            "container",
            Some("container_id"),
            "getById",
            "/container/graph",
        )
        .build();

    let storage_withdrawal_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver(
            "container",
            Some("container_id"),
            "getById",
            "/container/graph",
        )
        .build();

    let container_transfer_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector(
            "getBySourceContainer",
            r#"{"payload.source_container_id": "{{id}}"}"#,
        )
        .vector(
            "getByDestContainer",
            r#"{"payload.dest_container_id": "{{id}}"}"#,
        )
        .vector("getAll", "{}")
        .internal_singleton_resolver(
            "sourceContainer",
            Some("source_container_id"),
            "getById",
            "/container/graph",
        )
        .internal_singleton_resolver(
            "destContainer",
            Some("dest_container_id"),
            "getById",
            "/container/graph",
        )
        .build();

    let consumption_report_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByConsumer", r#"{"payload.consumer_id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver(
            "consumer",
            Some("consumer_id"),
            "getById",
            "/consumer/graph",
        )
        .internal_singleton_resolver(
            "container",
            Some("container_id"),
            "getById",
            "/container/graph",
        )
        .build();

    // --- Projections ---

    let container_inventory_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver(
            "container",
            Some("container_id"),
            "getById",
            "/container/graph",
        )
        .build();

    let hen_productivity_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByHen", r#"{"payload.hen_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph")
        .build();

    let farm_output_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .vector("getAll", "{}")
        .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
        .build();

    // ===== SERVER CONFIG =====

    let config = ServerConfig {
        port: 0, // unused in Lambda mode
        graphlettes: vec![
            // Actors (5)
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
                path: "/container/graph".to_string(),
                schema_text: CONTAINER_GRAPHQL.to_string(),
                root_config: container_config,
                searcher: container_searcher,
            },
            GraphletteConfig {
                path: "/consumer/graph".to_string(),
                schema_text: CONSUMER_GRAPHQL.to_string(),
                root_config: consumer_config,
                searcher: consumer_searcher,
            },
            // Events (5)
            GraphletteConfig {
                path: "/lay_report/graph".to_string(),
                schema_text: LAY_REPORT_GRAPHQL.to_string(),
                root_config: lay_report_config,
                searcher: lay_report_searcher,
            },
            GraphletteConfig {
                path: "/storage_deposit/graph".to_string(),
                schema_text: STORAGE_DEPOSIT_GRAPHQL.to_string(),
                root_config: storage_deposit_config,
                searcher: storage_deposit_searcher,
            },
            GraphletteConfig {
                path: "/storage_withdrawal/graph".to_string(),
                schema_text: STORAGE_WITHDRAWAL_GRAPHQL.to_string(),
                root_config: storage_withdrawal_config,
                searcher: storage_withdrawal_searcher,
            },
            GraphletteConfig {
                path: "/container_transfer/graph".to_string(),
                schema_text: CONTAINER_TRANSFER_GRAPHQL.to_string(),
                root_config: container_transfer_config,
                searcher: container_transfer_searcher,
            },
            GraphletteConfig {
                path: "/consumption_report/graph".to_string(),
                schema_text: CONSUMPTION_REPORT_GRAPHQL.to_string(),
                root_config: consumption_report_config,
                searcher: consumption_report_searcher,
            },
            // Projections (3)
            GraphletteConfig {
                path: "/container_inventory/graph".to_string(),
                schema_text: CONTAINER_INVENTORY_GRAPHQL.to_string(),
                root_config: container_inventory_config,
                searcher: container_inventory_searcher,
            },
            GraphletteConfig {
                path: "/hen_productivity/graph".to_string(),
                schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
                root_config: hen_productivity_config,
                searcher: hen_productivity_searcher,
            },
            GraphletteConfig {
                path: "/farm_output/graph".to_string(),
                schema_text: FARM_OUTPUT_GRAPHQL.to_string(),
                root_config: farm_output_config,
                searcher: farm_output_searcher,
            },
        ],
        restlettes: vec![
            // Actors (5)
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
                path: "/container/api".to_string(),
                schema_json: serde_json::from_str(CONTAINER_JSON)
                    .expect("invalid container JSON schema"),
                repository: container_repo,
            },
            RestletteConfig {
                path: "/consumer/api".to_string(),
                schema_json: serde_json::from_str(CONSUMER_JSON)
                    .expect("invalid consumer JSON schema"),
                repository: consumer_repo,
            },
            // Events (5)
            RestletteConfig {
                path: "/lay_report/api".to_string(),
                schema_json: serde_json::from_str(LAY_REPORT_JSON)
                    .expect("invalid lay_report JSON schema"),
                repository: lay_report_repo,
            },
            RestletteConfig {
                path: "/storage_deposit/api".to_string(),
                schema_json: serde_json::from_str(STORAGE_DEPOSIT_JSON)
                    .expect("invalid storage_deposit JSON schema"),
                repository: storage_deposit_repo,
            },
            RestletteConfig {
                path: "/storage_withdrawal/api".to_string(),
                schema_json: serde_json::from_str(STORAGE_WITHDRAWAL_JSON)
                    .expect("invalid storage_withdrawal JSON schema"),
                repository: storage_withdrawal_repo,
            },
            RestletteConfig {
                path: "/container_transfer/api".to_string(),
                schema_json: serde_json::from_str(CONTAINER_TRANSFER_JSON)
                    .expect("invalid container_transfer JSON schema"),
                repository: container_transfer_repo,
            },
            RestletteConfig {
                path: "/consumption_report/api".to_string(),
                schema_json: serde_json::from_str(CONSUMPTION_REPORT_JSON)
                    .expect("invalid consumption_report JSON schema"),
                repository: consumption_report_repo,
            },
            // Projections (3)
            RestletteConfig {
                path: "/container_inventory/api".to_string(),
                schema_json: serde_json::from_str(CONTAINER_INVENTORY_JSON)
                    .expect("invalid container_inventory JSON schema"),
                repository: container_inventory_repo,
            },
            RestletteConfig {
                path: "/hen_productivity/api".to_string(),
                schema_json: serde_json::from_str(HEN_PRODUCTIVITY_JSON)
                    .expect("invalid hen_productivity JSON schema"),
                repository: hen_productivity_repo,
            },
            RestletteConfig {
                path: "/farm_output/api".to_string(),
                schema_json: serde_json::from_str(FARM_OUTPUT_JSON)
                    .expect("invalid farm_output JSON schema"),
                repository: farm_output_repo,
            },
        ],
    };

    meshql_lambda::run_lambda(config).await
}

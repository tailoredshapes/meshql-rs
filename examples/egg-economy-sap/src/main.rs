use meshql_core::{GraphletteConfig, NoAuth, RestletteConfig, RootConfig, ServerConfig};
use meshql_mongo::{MongoRepository, MongoSearcher};
use meshql_server::run;
use std::sync::Arc;

// --- GraphQL Schemas (13 entities) ---
const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const CONTAINER_GRAPHQL: &str = include_str!("../config/graph/container.graphql");
const CONSUMER_GRAPHQL: &str = include_str!("../config/graph/consumer.graphql");
const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");
const STORAGE_DEPOSIT_GRAPHQL: &str = include_str!("../config/graph/storage_deposit.graphql");
const STORAGE_WITHDRAWAL_GRAPHQL: &str = include_str!("../config/graph/storage_withdrawal.graphql");
const CONTAINER_TRANSFER_GRAPHQL: &str = include_str!("../config/graph/container_transfer.graphql");
const CONSUMPTION_REPORT_GRAPHQL: &str = include_str!("../config/graph/consumption_report.graphql");
const CONTAINER_INVENTORY_GRAPHQL: &str =
    include_str!("../config/graph/container_inventory.graphql");
const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");
const FARM_OUTPUT_GRAPHQL: &str = include_str!("../config/graph/farm_output.graphql");

// --- JSON Schemas (13 entities) ---
const FARM_JSON_SCHEMA: &str = include_str!("../config/json/farm.schema.json");
const COOP_JSON_SCHEMA: &str = include_str!("../config/json/coop.schema.json");
const HEN_JSON_SCHEMA: &str = include_str!("../config/json/hen.schema.json");
const CONTAINER_JSON_SCHEMA: &str = include_str!("../config/json/container.schema.json");
const CONSUMER_JSON_SCHEMA: &str = include_str!("../config/json/consumer.schema.json");
const LAY_REPORT_JSON_SCHEMA: &str = include_str!("../config/json/lay_report.schema.json");
const STORAGE_DEPOSIT_JSON_SCHEMA: &str =
    include_str!("../config/json/storage_deposit.schema.json");
const STORAGE_WITHDRAWAL_JSON_SCHEMA: &str =
    include_str!("../config/json/storage_withdrawal.schema.json");
const CONTAINER_TRANSFER_JSON_SCHEMA: &str =
    include_str!("../config/json/container_transfer.schema.json");
const CONSUMPTION_REPORT_JSON_SCHEMA: &str =
    include_str!("../config/json/consumption_report.schema.json");
const CONTAINER_INVENTORY_JSON_SCHEMA: &str =
    include_str!("../config/json/container_inventory.schema.json");
const HEN_PRODUCTIVITY_JSON_SCHEMA: &str =
    include_str!("../config/json/hen_productivity.schema.json");
const FARM_OUTPUT_JSON_SCHEMA: &str = include_str!("../config/json/farm_output.schema.json");

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = env_or("PORT", "5089").parse()?;
    let mongo_uri = env_or("MONGO_URI", "mongodb://127.0.0.1:27017");
    let prefix = env_or("PREFIX", "egg_economy_sap");
    let env = env_or("ENV", "development");
    let db_name = format!("{}_{}", prefix, env);

    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);

    // ===== Repositories (13) =====
    let farm_repo =
        Arc::new(MongoRepository::new(&mongo_uri, &db_name, "farm", Arc::clone(&auth)).await?);
    let coop_repo =
        Arc::new(MongoRepository::new(&mongo_uri, &db_name, "coop", Arc::clone(&auth)).await?);
    let hen_repo =
        Arc::new(MongoRepository::new(&mongo_uri, &db_name, "hen", Arc::clone(&auth)).await?);
    let container_repo =
        Arc::new(MongoRepository::new(&mongo_uri, &db_name, "container", Arc::clone(&auth)).await?);
    let consumer_repo =
        Arc::new(MongoRepository::new(&mongo_uri, &db_name, "consumer", Arc::clone(&auth)).await?);
    let lay_report_repo = Arc::new(
        MongoRepository::new(&mongo_uri, &db_name, "lay_report", Arc::clone(&auth)).await?,
    );
    let storage_deposit_repo = Arc::new(
        MongoRepository::new(&mongo_uri, &db_name, "storage_deposit", Arc::clone(&auth)).await?,
    );
    let storage_withdrawal_repo = Arc::new(
        MongoRepository::new(
            &mongo_uri,
            &db_name,
            "storage_withdrawal",
            Arc::clone(&auth),
        )
        .await?,
    );
    let container_transfer_repo = Arc::new(
        MongoRepository::new(
            &mongo_uri,
            &db_name,
            "container_transfer",
            Arc::clone(&auth),
        )
        .await?,
    );
    let consumption_report_repo = Arc::new(
        MongoRepository::new(
            &mongo_uri,
            &db_name,
            "consumption_report",
            Arc::clone(&auth),
        )
        .await?,
    );
    let container_inventory_repo = Arc::new(
        MongoRepository::new(
            &mongo_uri,
            &db_name,
            "container_inventory",
            Arc::clone(&auth),
        )
        .await?,
    );
    let hen_productivity_repo = Arc::new(
        MongoRepository::new(&mongo_uri, &db_name, "hen_productivity", Arc::clone(&auth)).await?,
    );
    let farm_output_repo = Arc::new(
        MongoRepository::new(&mongo_uri, &db_name, "farm_output", Arc::clone(&auth)).await?,
    );

    // ===== Searchers (13) =====
    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "farm", Arc::clone(&auth)).await?);
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "coop", Arc::clone(&auth)).await?);
    let hen_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "hen", Arc::clone(&auth)).await?);
    let container_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "container", Arc::clone(&auth)).await?);
    let consumer_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "consumer", Arc::clone(&auth)).await?);
    let lay_report_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "lay_report", Arc::clone(&auth)).await?);
    let storage_deposit_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(&mongo_uri, &db_name, "storage_deposit", Arc::clone(&auth)).await?,
    );
    let storage_withdrawal_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(
            &mongo_uri,
            &db_name,
            "storage_withdrawal",
            Arc::clone(&auth),
        )
        .await?,
    );
    let container_transfer_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(
            &mongo_uri,
            &db_name,
            "container_transfer",
            Arc::clone(&auth),
        )
        .await?,
    );
    let consumption_report_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(
            &mongo_uri,
            &db_name,
            "consumption_report",
            Arc::clone(&auth),
        )
        .await?,
    );
    let container_inventory_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(
            &mongo_uri,
            &db_name,
            "container_inventory",
            Arc::clone(&auth),
        )
        .await?,
    );
    let hen_productivity_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(&mongo_uri, &db_name, "hen_productivity", Arc::clone(&auth)).await?,
    );
    let farm_output_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(&mongo_uri, &db_name, "farm_output", Arc::clone(&auth)).await?);

    // ===== Root Configs (13) =====

    // --- Actor Graphlettes (5) ---

    let farm_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
        .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
        .internal_vector_resolver("coops", None, "getByFarm", "/coop/graph")
        .internal_vector_resolver("farmOutput", None, "getByFarm", "/farm_output/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
        .internal_vector_resolver("hens", None, "getByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByCoop", r#"{"payload.coop_id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
        .internal_singleton_resolver("coop", Some("coop_id"), "getById", "/coop/graph")
        .internal_vector_resolver("layReports", None, "getByHen", "/lay_report/graph")
        .internal_vector_resolver("productivity", None, "getByHen", "/hen_productivity/graph")
        .build();

    let container_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
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
        .vector("getAll", r#"{}"#)
        .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
        .internal_vector_resolver(
            "consumptionReports",
            None,
            "getByConsumer",
            "/consumption_report/graph",
        )
        .build();

    // --- Event Graphlettes (5) ---

    let lay_report_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByHen", r#"{"payload.hen_id": "{{id}}"}"#)
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
        .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph")
        .build();

    let storage_deposit_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
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
        .vector("getAll", r#"{}"#)
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
        .vector("getAll", r#"{}"#)
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
        .vector("getAll", r#"{}"#)
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

    // --- Projection Graphlettes (3) ---

    let container_inventory_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
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
        .vector("getAll", r#"{}"#)
        .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph")
        .build();

    let farm_output_config = RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
        .vector("getAll", r#"{}"#)
        .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
        .build();

    // ===== Server Config =====

    let config = ServerConfig {
        port,
        graphlettes: vec![
            // Actor Graphlettes (5)
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
            // Event Graphlettes (5)
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
            // Projection Graphlettes (3)
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
            RestletteConfig {
                path: "/farm".to_string(),
                schema_json: serde_json::from_str(FARM_JSON_SCHEMA)?,
                repository: farm_repo,
            },
            RestletteConfig {
                path: "/coop".to_string(),
                schema_json: serde_json::from_str(COOP_JSON_SCHEMA)?,
                repository: coop_repo,
            },
            RestletteConfig {
                path: "/hen".to_string(),
                schema_json: serde_json::from_str(HEN_JSON_SCHEMA)?,
                repository: hen_repo,
            },
            RestletteConfig {
                path: "/container".to_string(),
                schema_json: serde_json::from_str(CONTAINER_JSON_SCHEMA)?,
                repository: container_repo,
            },
            RestletteConfig {
                path: "/consumer".to_string(),
                schema_json: serde_json::from_str(CONSUMER_JSON_SCHEMA)?,
                repository: consumer_repo,
            },
            RestletteConfig {
                path: "/lay_report".to_string(),
                schema_json: serde_json::from_str(LAY_REPORT_JSON_SCHEMA)?,
                repository: lay_report_repo,
            },
            RestletteConfig {
                path: "/storage_deposit".to_string(),
                schema_json: serde_json::from_str(STORAGE_DEPOSIT_JSON_SCHEMA)?,
                repository: storage_deposit_repo,
            },
            RestletteConfig {
                path: "/storage_withdrawal".to_string(),
                schema_json: serde_json::from_str(STORAGE_WITHDRAWAL_JSON_SCHEMA)?,
                repository: storage_withdrawal_repo,
            },
            RestletteConfig {
                path: "/container_transfer".to_string(),
                schema_json: serde_json::from_str(CONTAINER_TRANSFER_JSON_SCHEMA)?,
                repository: container_transfer_repo,
            },
            RestletteConfig {
                path: "/consumption_report".to_string(),
                schema_json: serde_json::from_str(CONSUMPTION_REPORT_JSON_SCHEMA)?,
                repository: consumption_report_repo,
            },
            RestletteConfig {
                path: "/container_inventory".to_string(),
                schema_json: serde_json::from_str(CONTAINER_INVENTORY_JSON_SCHEMA)?,
                repository: container_inventory_repo,
            },
            RestletteConfig {
                path: "/hen_productivity".to_string(),
                schema_json: serde_json::from_str(HEN_PRODUCTIVITY_JSON_SCHEMA)?,
                repository: hen_productivity_repo,
            },
            RestletteConfig {
                path: "/farm_output".to_string(),
                schema_json: serde_json::from_str(FARM_OUTPUT_JSON_SCHEMA)?,
                repository: farm_output_repo,
            },
        ],
    };

    run(config).await
}

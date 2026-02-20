//! SQLite-backed egg-economy performance server.
//! Starts the full 13-entity server on the configured port for k6 benchmarking.
//!
//! Usage: cargo run -p meshql-sqlite --release --bin perf_server

use meshql_core::{GraphletteConfig, RestletteConfig, RootConfig, ServerConfig};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

// ===== GraphQL Schemas =====

const FARM_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): Farm  getAll(at: Float): [Farm]  getByZone(zone: String, at: Float): [Farm] }
type Farm { id: ID  name: String!  farm_type: String!  zone: String!  owner: String  coops: [Coop]  farmOutput: [FarmOutput] }
type Coop { id: ID  name: String!  capacity: Int  coop_type: String  hens: [Hen] }
type Hen { id: ID  name: String!  breed: String  status: String }
type FarmOutput { id: ID  farm_type: String  eggs_today: Int  eggs_week: Int  eggs_month: Int  active_hens: Int  total_hens: Int  avg_per_hen_per_week: Float }
"#;

const COOP_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): Coop  getAll(at: Float): [Coop]  getByFarm(id: ID, at: Float): [Coop] }
type Farm { id: ID  name: String!  farm_type: String!  zone: String! }
type Coop { id: ID  name: String!  farm_id: String!  capacity: Int!  coop_type: String!  farm: Farm  hens: [Hen] }
type Hen { id: ID  name: String!  breed: String  dob: Date  status: String }
"#;

const HEN_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): Hen  getByCoop(id: ID, at: Float): [Hen]  getAll(at: Float): [Hen] }
type Coop { id: ID  name: String!  capacity: Int }
type Hen { id: ID  name: String!  coop_id: String!  breed: String!  dob: Date  status: String  coop: Coop  layReports: [LayReport]  productivity: [HenProductivity] }
type LayReport { id: ID  eggs: Int!  timestamp: String!  quality: String }
type HenProductivity { id: ID  eggs_today: Int  eggs_week: Int  eggs_month: Int  avg_per_week: Float  total_eggs: Int  quality_rate: Float }
"#;

const CONTAINER_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): Container  getAll(at: Float): [Container]  getByZone(zone: String, at: Float): [Container] }
type Container { id: ID  name: String!  container_type: String!  capacity: Int!  zone: String!  inventory: [ContainerInventory] }
type ContainerInventory { id: ID  current_eggs: Int  total_deposits: Int  total_withdrawals: Int  total_transfers_in: Int  total_transfers_out: Int  total_consumed: Int  utilization_pct: Float }
"#;

const CONSUMER_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): Consumer  getAll(at: Float): [Consumer]  getByZone(zone: String, at: Float): [Consumer] }
type Consumer { id: ID  name: String!  consumer_type: String!  zone: String!  weekly_demand: Int  consumptionReports: [ConsumptionReport] }
type ConsumptionReport { id: ID  eggs: Int!  timestamp: String!  purpose: String }
"#;

const LAY_REPORT_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): LayReport  getByHen(id: ID, at: Float): [LayReport]  getByFarm(id: ID, at: Float): [LayReport]  getAll(at: Float): [LayReport] }
type Hen { id: ID  name: String!  breed: String  status: String }
type LayReport { id: ID  hen_id: String!  coop_id: String!  farm_id: String!  eggs: Int!  timestamp: String!  quality: String  hen: Hen }
"#;

const STORAGE_DEPOSIT_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): StorageDeposit  getByContainer(id: ID, at: Float): [StorageDeposit]  getAll(at: Float): [StorageDeposit] }
type Container { id: ID  name: String!  container_type: String  zone: String }
type StorageDeposit { id: ID  container_id: String!  source_type: String!  source_id: String!  eggs: Int!  timestamp: String!  container: Container }
"#;

const STORAGE_WITHDRAWAL_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): StorageWithdrawal  getByContainer(id: ID, at: Float): [StorageWithdrawal]  getAll(at: Float): [StorageWithdrawal] }
type Container { id: ID  name: String!  container_type: String  zone: String }
type StorageWithdrawal { id: ID  container_id: String!  reason: String!  eggs: Int!  timestamp: String!  container: Container }
"#;

const CONTAINER_TRANSFER_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): ContainerTransfer  getBySourceContainer(id: ID, at: Float): [ContainerTransfer]  getByDestContainer(id: ID, at: Float): [ContainerTransfer]  getAll(at: Float): [ContainerTransfer] }
type Container { id: ID  name: String!  container_type: String  zone: String }
type ContainerTransfer { id: ID  source_container_id: String!  dest_container_id: String!  eggs: Int!  timestamp: String!  transport_method: String  sourceContainer: Container  destContainer: Container }
"#;

const CONSUMPTION_REPORT_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): ConsumptionReport  getByConsumer(id: ID, at: Float): [ConsumptionReport]  getByContainer(id: ID, at: Float): [ConsumptionReport]  getAll(at: Float): [ConsumptionReport] }
type Consumer { id: ID  name: String!  consumer_type: String  zone: String }
type Container { id: ID  name: String!  container_type: String  zone: String }
type ConsumptionReport { id: ID  consumer_id: String!  container_id: String!  eggs: Int!  timestamp: String!  purpose: String  consumer: Consumer  container: Container }
"#;

const CONTAINER_INVENTORY_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): ContainerInventory  getByContainer(id: ID, at: Float): [ContainerInventory]  getAll(at: Float): [ContainerInventory] }
type Container { id: ID  name: String!  container_type: String  capacity: Int  zone: String }
type ContainerInventory { id: ID  container_id: String!  current_eggs: Int!  total_deposits: Int!  total_withdrawals: Int!  total_transfers_in: Int!  total_transfers_out: Int!  total_consumed: Int!  utilization_pct: Float  container: Container }
"#;

const HEN_PRODUCTIVITY_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): HenProductivity  getByHen(id: ID, at: Float): [HenProductivity]  getAll(at: Float): [HenProductivity] }
type Hen { id: ID  name: String!  breed: String  status: String }
type HenProductivity { id: ID  hen_id: String!  farm_id: String!  eggs_today: Int!  eggs_week: Int!  eggs_month: Int!  avg_per_week: Float!  total_eggs: Int!  quality_rate: Float  hen: Hen }
"#;

const FARM_OUTPUT_GRAPHQL: &str = r#"
scalar Date
type Query { getById(id: ID, at: Float): FarmOutput  getByFarm(id: ID, at: Float): [FarmOutput]  getAll(at: Float): [FarmOutput] }
type Farm { id: ID  name: String!  farm_type: String!  zone: String! }
type FarmOutput { id: ID  farm_id: String!  farm_type: String  eggs_today: Int!  eggs_week: Int!  eggs_month: Int!  active_hens: Int!  total_hens: Int!  avg_per_hen_per_week: Float  farm: Farm }
"#;

struct Entity {
    repo: Arc<dyn meshql_core::Repository>,
    searcher: Arc<dyn meshql_core::Searcher>,
}

async fn make_entity(dir: &str, name: &str) -> Entity {
    let db_path = format!("{dir}/{name}.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::from_str(&format!("sqlite:{db_path}"))
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());
    Entity { repo, searcher }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "5088".into())
        .parse()
        .expect("PORT must be a valid u16");
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "/tmp/meshql-perf".into());

    std::fs::create_dir_all(&data_dir)?;

    // Create 13 entity stores
    let farm = make_entity(&data_dir, "farm").await;
    let coop = make_entity(&data_dir, "coop").await;
    let hen = make_entity(&data_dir, "hen").await;
    let container = make_entity(&data_dir, "container").await;
    let consumer = make_entity(&data_dir, "consumer").await;
    let lay_report = make_entity(&data_dir, "lay_report").await;
    let storage_deposit = make_entity(&data_dir, "storage_deposit").await;
    let storage_withdrawal = make_entity(&data_dir, "storage_withdrawal").await;
    let container_transfer = make_entity(&data_dir, "container_transfer").await;
    let consumption_report = make_entity(&data_dir, "consumption_report").await;
    let container_inventory = make_entity(&data_dir, "container_inventory").await;
    let hen_productivity = make_entity(&data_dir, "hen_productivity").await;
    let farm_output = make_entity(&data_dir, "farm_output").await;

    // Root configs (same as egg_economy_cert.rs)
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

    let config = ServerConfig {
        port,
        graphlettes: vec![
            GraphletteConfig {
                path: "/farm/graph".into(),
                schema_text: FARM_GRAPHQL.into(),
                root_config: farm_config,
                searcher: farm.searcher,
            },
            GraphletteConfig {
                path: "/coop/graph".into(),
                schema_text: COOP_GRAPHQL.into(),
                root_config: coop_config,
                searcher: coop.searcher,
            },
            GraphletteConfig {
                path: "/hen/graph".into(),
                schema_text: HEN_GRAPHQL.into(),
                root_config: hen_config,
                searcher: hen.searcher,
            },
            GraphletteConfig {
                path: "/container/graph".into(),
                schema_text: CONTAINER_GRAPHQL.into(),
                root_config: container_config,
                searcher: container.searcher,
            },
            GraphletteConfig {
                path: "/consumer/graph".into(),
                schema_text: CONSUMER_GRAPHQL.into(),
                root_config: consumer_config,
                searcher: consumer.searcher,
            },
            GraphletteConfig {
                path: "/lay_report/graph".into(),
                schema_text: LAY_REPORT_GRAPHQL.into(),
                root_config: lay_report_config,
                searcher: lay_report.searcher,
            },
            GraphletteConfig {
                path: "/storage_deposit/graph".into(),
                schema_text: STORAGE_DEPOSIT_GRAPHQL.into(),
                root_config: storage_deposit_config,
                searcher: storage_deposit.searcher,
            },
            GraphletteConfig {
                path: "/storage_withdrawal/graph".into(),
                schema_text: STORAGE_WITHDRAWAL_GRAPHQL.into(),
                root_config: storage_withdrawal_config,
                searcher: storage_withdrawal.searcher,
            },
            GraphletteConfig {
                path: "/container_transfer/graph".into(),
                schema_text: CONTAINER_TRANSFER_GRAPHQL.into(),
                root_config: container_transfer_config,
                searcher: container_transfer.searcher,
            },
            GraphletteConfig {
                path: "/consumption_report/graph".into(),
                schema_text: CONSUMPTION_REPORT_GRAPHQL.into(),
                root_config: consumption_report_config,
                searcher: consumption_report.searcher,
            },
            GraphletteConfig {
                path: "/container_inventory/graph".into(),
                schema_text: CONTAINER_INVENTORY_GRAPHQL.into(),
                root_config: container_inventory_config,
                searcher: container_inventory.searcher,
            },
            GraphletteConfig {
                path: "/hen_productivity/graph".into(),
                schema_text: HEN_PRODUCTIVITY_GRAPHQL.into(),
                root_config: hen_productivity_config,
                searcher: hen_productivity.searcher,
            },
            GraphletteConfig {
                path: "/farm_output/graph".into(),
                schema_text: FARM_OUTPUT_GRAPHQL.into(),
                root_config: farm_output_config,
                searcher: farm_output.searcher,
            },
        ],
        restlettes: vec![
            RestletteConfig {
                path: "/farm/api".into(),
                schema_json: serde_json::json!({}),
                repository: farm.repo,
            },
            RestletteConfig {
                path: "/coop/api".into(),
                schema_json: serde_json::json!({}),
                repository: coop.repo,
            },
            RestletteConfig {
                path: "/hen/api".into(),
                schema_json: serde_json::json!({}),
                repository: hen.repo,
            },
            RestletteConfig {
                path: "/container/api".into(),
                schema_json: serde_json::json!({}),
                repository: container.repo,
            },
            RestletteConfig {
                path: "/consumer/api".into(),
                schema_json: serde_json::json!({}),
                repository: consumer.repo,
            },
            RestletteConfig {
                path: "/lay_report/api".into(),
                schema_json: serde_json::json!({}),
                repository: lay_report.repo,
            },
            RestletteConfig {
                path: "/storage_deposit/api".into(),
                schema_json: serde_json::json!({}),
                repository: storage_deposit.repo,
            },
            RestletteConfig {
                path: "/storage_withdrawal/api".into(),
                schema_json: serde_json::json!({}),
                repository: storage_withdrawal.repo,
            },
            RestletteConfig {
                path: "/container_transfer/api".into(),
                schema_json: serde_json::json!({}),
                repository: container_transfer.repo,
            },
            RestletteConfig {
                path: "/consumption_report/api".into(),
                schema_json: serde_json::json!({}),
                repository: consumption_report.repo,
            },
            RestletteConfig {
                path: "/container_inventory/api".into(),
                schema_json: serde_json::json!({}),
                repository: container_inventory.repo,
            },
            RestletteConfig {
                path: "/hen_productivity/api".into(),
                schema_json: serde_json::json!({}),
                repository: hen_productivity.repo,
            },
            RestletteConfig {
                path: "/farm_output/api".into(),
                schema_json: serde_json::json!({}),
                repository: farm_output.repo,
            },
        ],
    };

    meshql_server::run(config).await
}

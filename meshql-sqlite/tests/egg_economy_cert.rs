use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::farm;
use meshql_cert::CertWorld;
use meshql_core::{GraphletteConfig, RestletteConfig, RootConfig, ServerConfig};
use meshql_server::build_app;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

// ===== GraphQL Schemas (13) =====

// Actors
const FARM_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Farm
  getAll(at: Float): [Farm]
  getByZone(zone: String, at: Float): [Farm]
}
type Farm {
  id: ID
  name: String!
  farm_type: String!
  zone: String!
  owner: String
  coops: [Coop]
  farmOutput: [FarmOutput]
}
type Coop { id: ID name: String! capacity: Int coop_type: String hens: [Hen] }
type Hen { id: ID name: String! breed: String status: String }
type FarmOutput { id: ID farm_type: String eggs_today: Int eggs_week: Int eggs_month: Int active_hens: Int total_hens: Int avg_per_hen_per_week: Float }
"#;

const COOP_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Coop
  getAll(at: Float): [Coop]
  getByFarm(id: ID, at: Float): [Coop]
}
type Farm { id: ID name: String! farm_type: String! zone: String! }
type Coop {
  id: ID
  name: String!
  farm_id: String!
  capacity: Int!
  coop_type: String!
  farm: Farm
  hens: [Hen]
}
type Hen { id: ID name: String! breed: String dob: Date status: String }
"#;

const HEN_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Hen
  getByCoop(id: ID, at: Float): [Hen]
  getAll(at: Float): [Hen]
}
type Coop { id: ID name: String! capacity: Int }
type Hen {
  id: ID
  name: String!
  coop_id: String!
  breed: String!
  dob: Date
  status: String
  coop: Coop
  layReports: [LayReport]
  productivity: [HenProductivity]
}
type LayReport { id: ID eggs: Int! timestamp: String! quality: String }
type HenProductivity { id: ID eggs_today: Int eggs_week: Int eggs_month: Int avg_per_week: Float total_eggs: Int quality_rate: Float }
"#;

const CONTAINER_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Container
  getAll(at: Float): [Container]
  getByZone(zone: String, at: Float): [Container]
}
type Container {
  id: ID
  name: String!
  container_type: String!
  capacity: Int!
  zone: String!
  inventory: [ContainerInventory]
}
type ContainerInventory { id: ID current_eggs: Int total_deposits: Int total_withdrawals: Int total_transfers_in: Int total_transfers_out: Int total_consumed: Int utilization_pct: Float }
"#;

const CONSUMER_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Consumer
  getAll(at: Float): [Consumer]
  getByZone(zone: String, at: Float): [Consumer]
}
type Consumer {
  id: ID
  name: String!
  consumer_type: String!
  zone: String!
  weekly_demand: Int
  consumptionReports: [ConsumptionReport]
}
type ConsumptionReport { id: ID eggs: Int! timestamp: String! purpose: String }
"#;

// Events
const LAY_REPORT_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): LayReport
  getByHen(id: ID, at: Float): [LayReport]
  getByFarm(id: ID, at: Float): [LayReport]
  getAll(at: Float): [LayReport]
}
type Hen { id: ID name: String! breed: String status: String }
type LayReport {
  id: ID
  hen_id: String!
  coop_id: String!
  farm_id: String!
  eggs: Int!
  timestamp: String!
  quality: String
  hen: Hen
}
"#;

const STORAGE_DEPOSIT_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): StorageDeposit
  getByContainer(id: ID, at: Float): [StorageDeposit]
  getAll(at: Float): [StorageDeposit]
}
type Container { id: ID name: String! container_type: String zone: String }
type StorageDeposit {
  id: ID
  container_id: String!
  source_type: String!
  source_id: String!
  eggs: Int!
  timestamp: String!
  container: Container
}
"#;

const STORAGE_WITHDRAWAL_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): StorageWithdrawal
  getByContainer(id: ID, at: Float): [StorageWithdrawal]
  getAll(at: Float): [StorageWithdrawal]
}
type Container { id: ID name: String! container_type: String zone: String }
type StorageWithdrawal {
  id: ID
  container_id: String!
  reason: String!
  eggs: Int!
  timestamp: String!
  container: Container
}
"#;

const CONTAINER_TRANSFER_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): ContainerTransfer
  getBySourceContainer(id: ID, at: Float): [ContainerTransfer]
  getByDestContainer(id: ID, at: Float): [ContainerTransfer]
  getAll(at: Float): [ContainerTransfer]
}
type Container { id: ID name: String! container_type: String zone: String }
type ContainerTransfer {
  id: ID
  source_container_id: String!
  dest_container_id: String!
  eggs: Int!
  timestamp: String!
  transport_method: String
  sourceContainer: Container
  destContainer: Container
}
"#;

const CONSUMPTION_REPORT_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): ConsumptionReport
  getByConsumer(id: ID, at: Float): [ConsumptionReport]
  getByContainer(id: ID, at: Float): [ConsumptionReport]
  getAll(at: Float): [ConsumptionReport]
}
type Consumer { id: ID name: String! consumer_type: String zone: String }
type Container { id: ID name: String! container_type: String zone: String }
type ConsumptionReport {
  id: ID
  consumer_id: String!
  container_id: String!
  eggs: Int!
  timestamp: String!
  purpose: String
  consumer: Consumer
  container: Container
}
"#;

// Projections
const CONTAINER_INVENTORY_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): ContainerInventory
  getByContainer(id: ID, at: Float): [ContainerInventory]
  getAll(at: Float): [ContainerInventory]
}
type Container { id: ID name: String! container_type: String capacity: Int zone: String }
type ContainerInventory {
  id: ID
  container_id: String!
  current_eggs: Int!
  total_deposits: Int!
  total_withdrawals: Int!
  total_transfers_in: Int!
  total_transfers_out: Int!
  total_consumed: Int!
  utilization_pct: Float
  container: Container
}
"#;

const HEN_PRODUCTIVITY_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): HenProductivity
  getByHen(id: ID, at: Float): [HenProductivity]
  getAll(at: Float): [HenProductivity]
}
type Hen { id: ID name: String! breed: String status: String }
type HenProductivity {
  id: ID
  hen_id: String!
  farm_id: String!
  eggs_today: Int!
  eggs_week: Int!
  eggs_month: Int!
  avg_per_week: Float!
  total_eggs: Int!
  quality_rate: Float
  hen: Hen
}
"#;

const FARM_OUTPUT_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): FarmOutput
  getByFarm(id: ID, at: Float): [FarmOutput]
  getAll(at: Float): [FarmOutput]
}
type Farm { id: ID name: String! farm_type: String! zone: String! }
type FarmOutput {
  id: ID
  farm_id: String!
  farm_type: String
  eggs_today: Int!
  eggs_week: Int!
  eggs_month: Int!
  active_hens: Int!
  total_hens: Int!
  avg_per_hen_per_week: Float
  farm: Farm
}
"#;

async fn make_pool() -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

struct EntityPool {
    repo: Arc<dyn meshql_core::Repository>,
    searcher: Arc<dyn meshql_core::Searcher>,
}

async fn make_entity() -> EntityPool {
    let pool = make_pool().await;
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());
    EntityPool { repo, searcher }
}

async fn build_egg_economy_server() -> String {
    // Create 13 entity pools
    let farm = make_entity().await;
    let coop = make_entity().await;
    let hen = make_entity().await;
    let container = make_entity().await;
    let consumer = make_entity().await;
    let lay_report = make_entity().await;
    let storage_deposit = make_entity().await;
    let storage_withdrawal = make_entity().await;
    let container_transfer = make_entity().await;
    let consumption_report = make_entity().await;
    let container_inventory = make_entity().await;
    let hen_productivity = make_entity().await;
    let farm_output = make_entity().await;

    // ===== ROOT CONFIGS (13) =====

    // Actors
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

    // Events
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

    // Projections
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

    let server_config = ServerConfig {
        port: 0,
        graphlettes: vec![
            // Actors
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
            // Events
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
            // Projections
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
            // Actors
            RestletteConfig {
                path: "/farm".into(),
                schema_json: serde_json::json!({}),
                repository: farm.repo,
            },
            RestletteConfig {
                path: "/coop".into(),
                schema_json: serde_json::json!({}),
                repository: coop.repo,
            },
            RestletteConfig {
                path: "/hen".into(),
                schema_json: serde_json::json!({}),
                repository: hen.repo,
            },
            RestletteConfig {
                path: "/container".into(),
                schema_json: serde_json::json!({}),
                repository: container.repo,
            },
            RestletteConfig {
                path: "/consumer".into(),
                schema_json: serde_json::json!({}),
                repository: consumer.repo,
            },
            // Events
            RestletteConfig {
                path: "/lay_report".into(),
                schema_json: serde_json::json!({}),
                repository: lay_report.repo,
            },
            RestletteConfig {
                path: "/storage_deposit".into(),
                schema_json: serde_json::json!({}),
                repository: storage_deposit.repo,
            },
            RestletteConfig {
                path: "/storage_withdrawal".into(),
                schema_json: serde_json::json!({}),
                repository: storage_withdrawal.repo,
            },
            RestletteConfig {
                path: "/container_transfer".into(),
                schema_json: serde_json::json!({}),
                repository: container_transfer.repo,
            },
            RestletteConfig {
                path: "/consumption_report".into(),
                schema_json: serde_json::json!({}),
                repository: consumption_report.repo,
            },
            // Projections
            RestletteConfig {
                path: "/container_inventory".into(),
                schema_json: serde_json::json!({}),
                repository: container_inventory.repo,
            },
            RestletteConfig {
                path: "/hen_productivity".into(),
                schema_json: serde_json::json!({}),
                repository: hen_productivity.repo,
            },
            RestletteConfig {
                path: "/farm_output".into(),
                schema_json: serde_json::json!({}),
                repository: farm_output.repo,
            },
        ],
    };

    let app = build_app(server_config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://127.0.0.1:{}", addr.port())
}

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            Box::pin(async move {
                let addr = build_egg_economy_server().await;
                world.server_addr = Some(addr);
                world.ids.clear();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/egg_economy.feature")
        .await;
}

//! The egg economy deployment: fourteen entities in three kinds, and the
//! harness around them that the certification (and only the certification) needs.
//!
//! `egg_economy.feature` in the contract (`features/proposed/egg-economy/`,
//! `specs/egg-economy.md`) certifies the *surfaces* of a mesh built the way
//! meshql is meant to be used:
//!
//! | kind        | entities                                                                       | written by             | declared as                  |
//! |-------------|---------------------------------------------------------------------------------|------------------------|------------------------------|
//! | actors      | farm coop hen container consumer                                                | the front end          | [`WriteMode::Open`]          |
//! | events      | lay_report storage_deposit storage_withdrawal container_transfer consumption_report | the front end, create-only | [`WriteMode::Event`]     |
//! | projections | hen_productivity farm_output container_inventory rejection                      | a worker only          | [`WriteMode::ProducedBy`]    |
//!
//! [`deploy`] builds that server over whatever stores the backend under test
//! hands it. Everything in [`worker`] is the *harness's*: a poller standing in
//! for CDC and a small reference hen worker. The library (the restlettes and
//! graphlettes) neither starts nor knows about either; it only refuses the
//! front end's writes to a projection and admits the worker's, because the worker
//! presents the identity a projection is declared `ProducedBy`.

use axum::middleware;
use axum::Router;
use meshql_core::{
    Auth, AuthContext, AuthMark, Envelope, GraphletteConfig, Operation, Repository,
    RestletteConfig, RootConfig, Searcher, ServerConfig, Session, WriteMode, WriteModes,
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::authz::IDENTITY_KEY;

pub mod worker;

/// Credentials of the front end: not a producer of anything.
pub const FRONT_END: &str = "frontend";
/// The reference worker's credentials. It is the producer of `hen_productivity`.
pub const HEN_WORKER: &str = "hen-worker";
/// The producers of the other projections. The harness has no workers for them
/// (the feature does not certify any), but the declarations are the deployment's.
pub const FARM_WORKER: &str = "farm-worker";
pub const CONTAINER_WORKER: &str = "container-worker";

pub const ACTORS: [&str; 5] = ["farm", "coop", "hen", "container", "consumer"];
pub const EVENTS: [&str; 5] = [
    "lay_report",
    "storage_deposit",
    "storage_withdrawal",
    "container_transfer",
    "consumption_report",
];
pub const PROJECTIONS: [&str; 4] = [
    "hen_productivity",
    "farm_output",
    "container_inventory",
    "rejection",
];

/// All fourteen, in the order a backend creates their stores.
pub fn entities() -> Vec<&'static str> {
    ACTORS
        .iter()
        .chain(EVENTS.iter())
        .chain(PROJECTIONS.iter())
        .copied()
        .collect()
}

/// One entity's storage: the pair a restlette and a graphlette are built over.
pub struct Store {
    pub repo: Arc<dyn Repository>,
    pub searcher: Arc<dyn Searcher>,
}

// ===== GraphQL schemas (14) =====

// Actors
const FARM_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Farm
  getAll(at: Float): [Farm]
  getByZone(zone: String, at: Float): [Farm]
  getByName(name: String, at: Float): [Farm]
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
type Hen { id: ID name: String! breed: String status: String productivity: [HenProductivity] }
type HenProductivity { id: ID total_eggs: Int quality_rate: Float }
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
type Farm { id: ID name: String! }
type Coop { id: ID name: String! capacity: Int farm: Farm }
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
  current_eggs: Int
  total_deposits: Int
  total_withdrawals: Int
  total_transfers_in: Int
  total_transfers_out: Int
  total_consumed: Int
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
  eggs_today: Int
  eggs_week: Int
  eggs_month: Int
  avg_per_week: Float
  total_eggs: Int
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
  eggs_today: Int
  eggs_week: Int
  eggs_month: Int
  active_hens: Int
  total_hens: Int
  avg_per_hen_per_week: Float
  farm: Farm
}
"#;

const REJECTION_GRAPHQL: &str = r#"
scalar Date
type Query {
  getById(id: ID, at: Float): Rejection
  getByAggregate(id: ID, at: Float): [Rejection]
  getAll(at: Float): [Rejection]
}
type Rejection {
  id: ID
  event_entity: String!
  event_id: String!
  actor: String
  timestamp: String
  reason: String!
  aggregate_id: String
}
"#;

// ===== Root configs =====

fn by_id() -> meshql_core::RootConfigBuilder {
    RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getAll", "{}")
}

fn by(field: &str) -> String {
    format!(r#"{{"payload.{field}": "{{{{id}}}}"}}"#)
}

fn zone() -> &'static str {
    r#"{"payload.zone": "{{zone}}"}"#
}

fn root_config(entity: &str) -> RootConfig {
    match entity {
        "farm" => by_id()
            .vector("getByZone", zone())
            .vector("getByName", r#"{"payload.name": "{{name}}"}"#)
            .internal_vector_resolver("coops", None, "getByFarm", "/coop/graph")
            .internal_vector_resolver("farmOutput", None, "getByFarm", "/farm_output/graph"),
        "coop" => by_id()
            .vector("getByFarm", by("farm_id"))
            .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
            .internal_vector_resolver("hens", None, "getByCoop", "/hen/graph"),
        "hen" => by_id()
            .vector("getByCoop", by("coop_id"))
            .internal_singleton_resolver("coop", Some("coop_id"), "getById", "/coop/graph")
            .internal_vector_resolver("layReports", None, "getByHen", "/lay_report/graph")
            .internal_vector_resolver("productivity", None, "getByHen", "/hen_productivity/graph"),
        "container" => by_id()
            .vector("getByZone", zone())
            .internal_vector_resolver(
                "inventory",
                None,
                "getByContainer",
                "/container_inventory/graph",
            ),
        "consumer" => by_id()
            .vector("getByZone", zone())
            .internal_vector_resolver(
                "consumptionReports",
                None,
                "getByConsumer",
                "/consumption_report/graph",
            ),
        "lay_report" => by_id()
            .vector("getByHen", by("hen_id"))
            .vector("getByFarm", by("farm_id"))
            .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph"),
        "storage_deposit" | "storage_withdrawal" => by_id()
            .vector("getByContainer", by("container_id"))
            .internal_singleton_resolver(
                "container",
                Some("container_id"),
                "getById",
                "/container/graph",
            ),
        "container_transfer" => by_id()
            .vector("getBySourceContainer", by("source_container_id"))
            .vector("getByDestContainer", by("dest_container_id"))
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
            ),
        "consumption_report" => by_id()
            .vector("getByConsumer", by("consumer_id"))
            .vector("getByContainer", by("container_id"))
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
            ),
        "container_inventory" => by_id()
            .vector("getByContainer", by("container_id"))
            .internal_singleton_resolver(
                "container",
                Some("container_id"),
                "getById",
                "/container/graph",
            ),
        "hen_productivity" => by_id()
            .vector("getByHen", by("hen_id"))
            .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph"),
        "farm_output" => by_id()
            .vector("getByFarm", by("farm_id"))
            .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph"),
        "rejection" => by_id().vector("getByAggregate", by("aggregate_id")),
        other => panic!("not an egg economy entity: {other}"),
    }
    .build()
}

fn schema_text(entity: &str) -> &'static str {
    match entity {
        "farm" => FARM_GRAPHQL,
        "coop" => COOP_GRAPHQL,
        "hen" => HEN_GRAPHQL,
        "container" => CONTAINER_GRAPHQL,
        "consumer" => CONSUMER_GRAPHQL,
        "lay_report" => LAY_REPORT_GRAPHQL,
        "storage_deposit" => STORAGE_DEPOSIT_GRAPHQL,
        "storage_withdrawal" => STORAGE_WITHDRAWAL_GRAPHQL,
        "container_transfer" => CONTAINER_TRANSFER_GRAPHQL,
        "consumption_report" => CONSUMPTION_REPORT_GRAPHQL,
        "container_inventory" => CONTAINER_INVENTORY_GRAPHQL,
        "hen_productivity" => HEN_PRODUCTIVITY_GRAPHQL,
        "farm_output" => FARM_OUTPUT_GRAPHQL,
        "rejection" => REJECTION_GRAPHQL,
        other => panic!("not an egg economy entity: {other}"),
    }
}

/// How each restlette is declared: events are create-only, a projection has its
/// producers. (`rejection` is written by every worker, as the spec has it.)
pub fn write_modes() -> WriteModes {
    let mut modes = WriteModes::new();
    for e in EVENTS {
        modes.insert(format!("/{e}/api"), WriteMode::Event);
    }
    let producers: [(&str, Vec<&str>); 4] = [
        ("hen_productivity", vec![HEN_WORKER]),
        ("farm_output", vec![FARM_WORKER]),
        ("container_inventory", vec![CONTAINER_WORKER]),
        ("rejection", vec![HEN_WORKER, FARM_WORKER, CONTAINER_WORKER]),
    ];
    for (e, who) in producers {
        modes.insert(
            format!("/{e}/api"),
            WriteMode::ProducedBy(who.into_iter().map(String::from).collect()),
        );
    }
    modes
}

// ===== Auth =====

/// The deployment's auth plugin. Authorization of records is not the point of
/// this suite (every record is public), identity is: the edge asserts who is
/// calling in a header, and a caller is a *principal* when it names itself
/// (`Session::is_principal`). The front end is not one; a worker is.
///
/// A real deployment would use its own plugin and give workers their own
/// tokens; whatever it is, a projection stays closed to a caller that is not
/// one of its producers.
pub struct EggAuth;

struct EggSession {
    caller: Option<String>,
}

impl Session for EggSession {
    fn stamp(&self, mut envelope: Envelope) -> Envelope {
        envelope.auth = AuthMark::empty();
        envelope
    }

    fn is_authorized(&self, _operation: Operation, _envelope: &Envelope) -> bool {
        true
    }

    fn is_principal(&self, name: &str) -> bool {
        self.caller.as_deref() == Some(name)
    }
}

impl Auth for EggAuth {
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(EggSession {
            caller: context
                .stash()
                .get(IDENTITY_KEY)
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    }
}

/// Stand up the economy over the given stores (one per entity, keyed by name)
/// on an ephemeral port and return its base URL and a handle that stops it
/// (and so lets the stores' connection pools go).
pub async fn deploy(mut stores: HashMap<String, Store>) -> (String, tokio::task::AbortHandle) {
    let mut graphlettes = Vec::new();
    let mut restlettes = Vec::new();
    for e in entities() {
        let store = stores
            .remove(e)
            .unwrap_or_else(|| panic!("no store for entity {e}"));
        graphlettes.push(GraphletteConfig {
            path: format!("/{e}/graph"),
            schema_text: schema_text(e).into(),
            root_config: root_config(e),
            searcher: store.searcher,
        });
        restlettes.push(RestletteConfig {
            path: format!("/{e}/api"),
            schema_json: serde_json::json!({}),
            repository: store.repo,
        });
    }
    let config = ServerConfig {
        port: 0,
        graphlettes,
        restlettes,
    };
    let app = meshql_server::build_app_with_modes(
        config,
        Arc::new(EggAuth),
        Router::new(),
        write_modes(),
    )
    .await
    .expect("build egg economy app")
    .layer(middleware::from_fn(crate::authz::edge_identity));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://127.0.0.1:{}", addr.port()),
        server.abort_handle(),
    )
}

/// The contract's egg economy features: the surfaces, and the graph of actors.
pub const CONTRACT_FEATURES: [&str; 2] = [
    "../meshql-cert/tests/features/contract/features/proposed/egg-economy/egg_economy.feature",
    "../meshql-cert/tests/features/contract/features/proposed/egg-economy/egg_graph.feature",
];

/// This repository's own feature over the same deployment: the graph of all
/// fourteen entities (federation, `at:` through actors) that the contract's does
/// not exercise.
pub const GRAPH_FEATURE: &str = "../meshql-cert/tests/features/egg_graph_full.feature";

/// Run both features against a fresh economy per scenario, over the stores
/// `make_stores` hands out, and exit non-zero if anything failed or was skipped.
/// Each backend's `egg_economy_cert` is this and its stores.
pub async fn run<F, Fut>(make_stores: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = HashMap<String, Store>> + 'static,
{
    use cucumber::{writer::Stats, World as _};
    let make_stores = Arc::new(make_stores);
    let mut failed = false;
    for feature in CONTRACT_FEATURES.iter().copied().chain([GRAPH_FEATURE]) {
        let make = Arc::clone(&make_stores);
        let writer = crate::CertWorld::cucumber()
            .max_concurrent_scenarios(1)
            .before(move |_feature, _rule, _scenario, world| {
                let make = Arc::clone(&make);
                Box::pin(async move {
                    let (base, server) = deploy(make().await).await;
                    world.server_addr = Some(base.clone());
                    world.ids.clear();
                    world.reset_authz();
                    world.egg_status = None;
                    world.egg_lay_report = None;
                    // The harness's CDC and worker; the deployment above does not know them.
                    world.egg_stop = vec![server, worker::start(base)];
                })
            })
            .after(|_feature, _rule, _scenario, _finished, world| {
                Box::pin(async move {
                    // Stop the worker and the server, so a scenario's connection
                    // pools do not pile up over the run.
                    for handle in world
                        .map(|w| std::mem::take(&mut w.egg_stop))
                        .unwrap_or_default()
                    {
                        handle.abort();
                    }
                })
            })
            // A scenario whose steps do not match is *skipped*, and cucumber
            // exits 0 on a skip: without this a suite where nothing ran reports
            // success.
            .fail_on_skipped()
            .run(feature)
            .await;
        failed |= writer.execution_has_failed();
    }
    if failed {
        eprintln!("egg economy certification FAILED");
        std::process::exit(1);
    }
}

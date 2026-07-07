//! egg-economy — a fully event-sourced meshql deployment.
//!
//! The only public write surface is the business-event meshes (the verbs). A CDC
//! connector mirrors committed event writes onto the `egg-events` topic, and one
//! worker per noun folds those events into the domain models. Nothing writes a
//! noun directly — the noun meshes expose read-only graphlettes only.
//!
//!   POST /<verb>/api ─► event mesh (Mongo) ─► CDC ─► egg-events ─► worker ─► noun mesh (Mongo) ─► GET /<noun>/graph
//!
//! See `lib.rs` and the `events` / `projectors` / `worker` / `source` modules.

use egg_economy::projectors::{
    BuildProjector, ContainerInventoryProjector, FarmOutputProjector, HenProductivityProjector,
    HenProjector, Projector,
};
use egg_economy::{run_connector, EventSource, RepositoryTail, Worker};
use meshql_core::{
    Auth, GraphletteConfig, NoAuth, Repository, RestletteConfig, RootConfig, Searcher, ServerConfig,
};
use meshql_mongo::{MongoRepository, MongoSearcher};
use meshql_server::run;
use merkql::broker::{Broker, BrokerConfig};
use std::sync::Arc;
use std::time::Duration;

// ---- Business-event schemas (verbs): write via restlette, audit via graphlette ----
const BUILD_FARM_GRAPHQL: &str = include_str!("../config/graph/build_farm.graphql");
const BUILD_FARM_JSON: &str = include_str!("../config/json/build_farm.schema.json");
const BUILD_COOP_GRAPHQL: &str = include_str!("../config/graph/build_coop.graphql");
const BUILD_COOP_JSON: &str = include_str!("../config/json/build_coop.schema.json");
const BUY_HENS_GRAPHQL: &str = include_str!("../config/graph/buy_hens.graphql");
const BUY_HENS_JSON: &str = include_str!("../config/json/buy_hens.schema.json");
const MOVE_HEN_TO_COOP_GRAPHQL: &str = include_str!("../config/graph/move_hen_to_coop.graphql");
const MOVE_HEN_TO_COOP_JSON: &str = include_str!("../config/json/move_hen_to_coop.schema.json");
const RETIRE_HEN_GRAPHQL: &str = include_str!("../config/graph/retire_hen.graphql");
const RETIRE_HEN_JSON: &str = include_str!("../config/json/retire_hen.schema.json");
const BUILD_CONTAINER_GRAPHQL: &str = include_str!("../config/graph/build_container.graphql");
const BUILD_CONTAINER_JSON: &str = include_str!("../config/json/build_container.schema.json");
const REGISTER_CONSUMER_GRAPHQL: &str = include_str!("../config/graph/register_consumer.graphql");
const REGISTER_CONSUMER_JSON: &str = include_str!("../config/json/register_consumer.schema.json");
const EGGS_LAID_GRAPHQL: &str = include_str!("../config/graph/eggs_laid.graphql");
const EGGS_LAID_JSON: &str = include_str!("../config/json/eggs_laid.schema.json");
const EGGS_STORED_GRAPHQL: &str = include_str!("../config/graph/eggs_stored.graphql");
const EGGS_STORED_JSON: &str = include_str!("../config/json/eggs_stored.schema.json");
const EGGS_WITHDRAWN_GRAPHQL: &str = include_str!("../config/graph/eggs_withdrawn.graphql");
const EGGS_WITHDRAWN_JSON: &str = include_str!("../config/json/eggs_withdrawn.schema.json");
const EGGS_TRANSFERRED_GRAPHQL: &str = include_str!("../config/graph/eggs_transferred.graphql");
const EGGS_TRANSFERRED_JSON: &str = include_str!("../config/json/eggs_transferred.schema.json");
const EGGS_CONSUMED_GRAPHQL: &str = include_str!("../config/graph/eggs_consumed.graphql");
const EGGS_CONSUMED_JSON: &str = include_str!("../config/json/eggs_consumed.schema.json");

// ---- Noun schemas: read-only graphlettes; workers write them, not restlettes ----
const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const CONTAINER_GRAPHQL: &str = include_str!("../config/graph/container.graphql");
const CONSUMER_GRAPHQL: &str = include_str!("../config/graph/consumer.graphql");
const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");
const CONTAINER_INVENTORY_GRAPHQL: &str =
    include_str!("../config/graph/container_inventory.graphql");
const FARM_OUTPUT_GRAPHQL: &str = include_str!("../config/graph/farm_output.graphql");

async fn mesh(
    uri: &str,
    db: &str,
    coll: &str,
    auth: &Arc<dyn Auth>,
) -> anyhow::Result<(Arc<MongoRepository>, Arc<dyn Searcher>)> {
    let repo = Arc::new(MongoRepository::new(uri, db, coll, Arc::clone(auth)).await?);
    let searcher: Arc<dyn Searcher> =
        Arc::new(MongoSearcher::new(uri, db, coll, Arc::clone(auth)).await?);
    Ok((repo, searcher))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mongo_uri =
        std::env::var("MONGO_URI").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_string());
    let prefix = std::env::var("PREFIX").unwrap_or_else(|_| "eggs".to_string());
    let env = std::env::var("ENV").unwrap_or_else(|_| "development".to_string());
    let merkql_dir = std::env::var("MERKQL_DIR").unwrap_or_else(|_| "./egg-events-log".to_string());
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "5088".to_string())
        .parse()
        .expect("PORT must be a valid u16");
    let db_name = format!("{}_{}", prefix, env);
    let auth: Arc<dyn Auth> = Arc::new(NoAuth);

    // The event log every business event lands on. In production this broker is
    // fed by a native change-stream connector; here the portable RepositoryTail
    // observes the committed event collections (still storage-layer, not the
    // write path — no restlette is touched).
    let broker = Broker::open(BrokerConfig::new(&merkql_dir))?;

    let mut graphlettes: Vec<GraphletteConfig> = Vec::new();
    let mut restlettes: Vec<RestletteConfig> = Vec::new();
    let mut sources: Vec<Arc<dyn EventSource>> = Vec::new();

    // ===== Business-event meshes (the only write surface) =====
    let event_defs: [(&str, &str, &str); 12] = [
        ("build_farm", BUILD_FARM_GRAPHQL, BUILD_FARM_JSON),
        ("build_coop", BUILD_COOP_GRAPHQL, BUILD_COOP_JSON),
        ("buy_hens", BUY_HENS_GRAPHQL, BUY_HENS_JSON),
        ("move_hen_to_coop", MOVE_HEN_TO_COOP_GRAPHQL, MOVE_HEN_TO_COOP_JSON),
        ("retire_hen", RETIRE_HEN_GRAPHQL, RETIRE_HEN_JSON),
        ("build_container", BUILD_CONTAINER_GRAPHQL, BUILD_CONTAINER_JSON),
        ("register_consumer", REGISTER_CONSUMER_GRAPHQL, REGISTER_CONSUMER_JSON),
        ("eggs_laid", EGGS_LAID_GRAPHQL, EGGS_LAID_JSON),
        ("eggs_stored", EGGS_STORED_GRAPHQL, EGGS_STORED_JSON),
        ("eggs_withdrawn", EGGS_WITHDRAWN_GRAPHQL, EGGS_WITHDRAWN_JSON),
        ("eggs_transferred", EGGS_TRANSFERRED_GRAPHQL, EGGS_TRANSFERRED_JSON),
        ("eggs_consumed", EGGS_CONSUMED_GRAPHQL, EGGS_CONSUMED_JSON),
    ];
    for (verb, gql, json) in event_defs {
        let (repo, searcher) = mesh(&mongo_uri, &db_name, verb, &auth).await?;
        let cfg = RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .build();
        graphlettes.push(GraphletteConfig {
            path: format!("/{verb}/graph"),
            schema_text: gql.to_string(),
            root_config: cfg,
            searcher: Arc::clone(&searcher),
        });
        restlettes.push(RestletteConfig {
            path: format!("/{verb}/api"),
            schema_json: serde_json::from_str(json)
                .unwrap_or_else(|_| panic!("invalid {verb} JSON schema")),
            repository: repo,
        });
        sources.push(Arc::new(RepositoryTail::new(verb, searcher)));
    }

    // ===== Noun meshes (read-only) + their workers =====
    let mut workers: Vec<Worker> = Vec::new();

    // Each entry: (noun, graphql, RootConfig, projector)
    let (farm_repo, farm_searcher) = mesh(&mongo_uri, &db_name, "farm", &auth).await?;
    let (coop_repo, coop_searcher) = mesh(&mongo_uri, &db_name, "coop", &auth).await?;
    let (hen_repo, hen_searcher) = mesh(&mongo_uri, &db_name, "hen", &auth).await?;
    let (container_repo, container_searcher) =
        mesh(&mongo_uri, &db_name, "container", &auth).await?;
    let (consumer_repo, consumer_searcher) = mesh(&mongo_uri, &db_name, "consumer", &auth).await?;
    let (hp_repo, hp_searcher) = mesh(&mongo_uri, &db_name, "hen_productivity", &auth).await?;
    let (ci_repo, ci_searcher) = mesh(&mongo_uri, &db_name, "container_inventory", &auth).await?;
    let (fo_repo, fo_searcher) = mesh(&mongo_uri, &db_name, "farm_output", &auth).await?;

    graphlettes.push(GraphletteConfig {
        path: "/farm/graph".to_string(),
        schema_text: FARM_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
            .internal_vector_resolver("coops", None, "getByFarm", "/coop/graph")
            .internal_vector_resolver("farmOutput", None, "getByFarm", "/farm_output/graph")
            .build(),
        searcher: farm_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/coop/graph".to_string(),
        schema_text: COOP_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
            .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
            .internal_vector_resolver("hens", None, "getByCoop", "/hen/graph")
            .build(),
        searcher: coop_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/hen/graph".to_string(),
        schema_text: HEN_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByCoop", r#"{"payload.coop_id": "{{id}}"}"#)
            .internal_singleton_resolver("coop", Some("coop_id"), "getById", "/coop/graph")
            .internal_vector_resolver("productivity", None, "getByHen", "/hen_productivity/graph")
            .build(),
        searcher: hen_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/container/graph".to_string(),
        schema_text: CONTAINER_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
            .internal_vector_resolver("inventory", None, "getByContainer", "/container_inventory/graph")
            .build(),
        searcher: container_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/consumer/graph".to_string(),
        schema_text: CONSUMER_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByZone", r#"{"payload.zone": "{{zone}}"}"#)
            .build(),
        searcher: consumer_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/hen_productivity/graph".to_string(),
        schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByHen", r#"{"payload.hen_id": "{{id}}"}"#)
            .internal_singleton_resolver("hen", Some("hen_id"), "getById", "/hen/graph")
            .build(),
        searcher: hp_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/container_inventory/graph".to_string(),
        schema_text: CONTAINER_INVENTORY_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByContainer", r#"{"payload.container_id": "{{id}}"}"#)
            .internal_singleton_resolver("container", Some("container_id"), "getById", "/container/graph")
            .build(),
        searcher: ci_searcher,
    });
    graphlettes.push(GraphletteConfig {
        path: "/farm_output/graph".to_string(),
        schema_text: FARM_OUTPUT_GRAPHQL.to_string(),
        root_config: RootConfig::builder()
            .singleton("getById", r#"{"id": "{{id}}"}"#)
            .vector("getAll", "{}")
            .vector("getByFarm", r#"{"payload.farm_id": "{{id}}"}"#)
            .internal_singleton_resolver("farm", Some("farm_id"), "getById", "/farm/graph")
            .build(),
        searcher: fo_searcher,
    });

    // One worker per noun. No noun restlettes exist — workers are the only writers.
    let noun_workers: Vec<(Box<dyn Projector>, Arc<dyn Repository>)> = vec![
        (Box::new(BuildProjector::farm()), farm_repo),
        (Box::new(BuildProjector::coop()), coop_repo),
        (Box::new(HenProjector::new()), hen_repo),
        (Box::new(BuildProjector::container()), container_repo),
        (Box::new(BuildProjector::consumer()), consumer_repo),
        (Box::new(HenProductivityProjector::new()), hp_repo),
        (Box::new(ContainerInventoryProjector::new()), ci_repo),
        (Box::new(FarmOutputProjector::new()), fo_repo),
    ];
    for (projector, repo) in noun_workers {
        workers.push(Worker::new(broker.clone(), projector, repo));
    }

    // ===== Run: CDC connector + workers in the background, HTTP in the foreground =====
    tokio::spawn(run_connector(
        broker.clone(),
        sources,
        Duration::from_millis(500),
    ));
    for w in workers {
        tokio::spawn(w.run_forever(Duration::from_millis(500)));
    }

    let config = ServerConfig { port, graphlettes, restlettes };
    run(config).await
}

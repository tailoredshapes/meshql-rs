use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::farm;
use meshql_cert::CertWorld;
use meshql_core::{GraphletteConfig, NoAuth, RestletteConfig, RootConfig, ServerConfig};
use meshql_server::build_app;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

// Server A schema: Farm with coops resolved via HTTP to Server B
const FARM_GRAPHQL: &str = r#"
type Farm {
    id: ID
    name: String
    coops: [Coop]
}
type Coop {
    id: ID
    name: String
}
type Query {
    getFarm(id: ID, at: Int): Farm
    getFarms(name: String, at: Int): [Farm]
}
"#;

// Server B schema: Coop with farm resolved via HTTP to Server A
const COOP_GRAPHQL: &str = r#"
type Coop {
    id: ID
    farmId: String
    name: String
    farm: Farm
}
type Farm { id: ID name: String }
type Query {
    getCoop(id: ID, at: Int): Coop
    getCoops(name: String, at: Int): [Coop]
    getCoopsByFarm(id: ID, at: Int): [Coop]
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

/// Start two servers: Server A (farm) and Server B (coop).
/// Server A's farm has an HTTP vector_resolver pointing at Server B for coops.
/// Server B's coop has an HTTP singleton_resolver pointing at Server A for farm.
async fn build_cross_service_servers() -> (String, String) {
    let _auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);

    // Create pools for each entity on each server
    let farm_pool = make_pool().await;
    let coop_pool = make_pool().await;

    let farm_repo = Arc::new(
        SqliteRepository::new_with_pool(farm_pool.clone())
            .await
            .unwrap(),
    );
    let coop_repo = Arc::new(
        SqliteRepository::new_with_pool(coop_pool.clone())
            .await
            .unwrap(),
    );

    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(farm_pool).await.unwrap());
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(coop_pool).await.unwrap());

    // Bind both listeners first to know the ports
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = format!(
        "http://127.0.0.1:{}",
        listener_a.local_addr().unwrap().port()
    );
    let addr_b = format!(
        "http://127.0.0.1:{}",
        listener_b.local_addr().unwrap().port()
    );

    // Server A config: farm with HTTP resolver pointing at Server B for coops
    let farm_config = RootConfig::builder()
        .singleton("getFarm", r#"{"id": "{{id}}"}"#)
        .vector("getFarms", r#"{"payload.name": "{{name}}"}"#)
        .vector_resolver(
            "coops",
            None,
            "getCoopsByFarm",
            &format!("{addr_b}/coop/graph"),
        )
        .build();

    let server_a_config = ServerConfig {
        port: 0,
        graphlettes: vec![GraphletteConfig {
            path: "/farm/graph".into(),
            schema_text: FARM_GRAPHQL.into(),
            root_config: farm_config,
            searcher: farm_searcher,
        }],
        restlettes: vec![RestletteConfig {
            path: "/farm/api".into(),
            schema_json: serde_json::json!({}),
            repository: farm_repo,
        }],
    };

    // Server B config: coop with HTTP resolver pointing at Server A for farm
    let coop_config = RootConfig::builder()
        .singleton("getCoop", r#"{"id": "{{id}}"}"#)
        .vector("getCoops", r#"{"payload.name": "{{name}}"}"#)
        .vector("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)
        .singleton_resolver(
            "farm",
            Some("farmId"),
            "getFarm",
            &format!("{addr_a}/farm/graph"),
        )
        .build();

    let server_b_config = ServerConfig {
        port: 0,
        graphlettes: vec![GraphletteConfig {
            path: "/coop/graph".into(),
            schema_text: COOP_GRAPHQL.into(),
            root_config: coop_config,
            searcher: coop_searcher,
        }],
        restlettes: vec![RestletteConfig {
            path: "/coop/api".into(),
            schema_json: serde_json::json!({}),
            repository: coop_repo,
        }],
    };

    let app_a = build_app(server_a_config).await.unwrap();
    let app_b = build_app(server_b_config).await.unwrap();

    tokio::spawn(async move {
        axum::serve(listener_a, app_a).await.unwrap();
    });
    tokio::spawn(async move {
        axum::serve(listener_b, app_b).await.unwrap();
    });

    (addr_a, addr_b)
}

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            Box::pin(async move {
                let (addr_a, addr_b) = build_cross_service_servers().await;
                world.server_addr = Some(addr_a);
                world.server_b_addr = Some(addr_b);
                world.ids.clear();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/cross_service.feature")
        .await;
}

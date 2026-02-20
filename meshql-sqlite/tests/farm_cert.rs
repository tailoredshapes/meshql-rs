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

const FARM_GRAPHQL: &str = r#"
type Farm {
    id: ID
    name: String
    address: String
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

const COOP_GRAPHQL: &str = r#"
type Coop {
    id: ID
    farmId: String
    name: String
    farm: Farm
    hens: [Hen]
}
type Farm { id: ID name: String }
type Hen { id: ID name: String }
type Query {
    getCoop(id: ID, at: Int): Coop
    getCoops(name: String, at: Int): [Coop]
    getCoopsByFarm(id: ID, at: Int): [Coop]
}
"#;

const HEN_GRAPHQL: &str = r#"
type Hen {
    id: ID
    coopId: String
    name: String
    eggs: Int
    coop: Coop
}
type Coop { id: ID name: String }
type Query {
    getHen(id: ID, at: Int): Hen
    getHens(name: String, at: Int): [Hen]
    getHensByCoop(id: ID, at: Int): [Hen]
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

async fn build_farm_server() -> String {
    let _auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);

    // Each entity type gets its own pool (separate in-memory SQLite DB)
    let farm_pool = make_pool().await;
    let coop_pool = make_pool().await;
    let hen_pool = make_pool().await;

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
    let hen_repo = Arc::new(
        SqliteRepository::new_with_pool(hen_pool.clone())
            .await
            .unwrap(),
    );

    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(farm_pool).await.unwrap());
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(coop_pool).await.unwrap());
    let hen_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(hen_pool).await.unwrap());

    let farm_config = RootConfig::builder()
        .singleton("getFarm", r#"{"id": "{{id}}"}"#)
        .vector("getFarms", r#"{"payload.name": "{{name}}"}"#)
        .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getCoop", r#"{"id": "{{id}}"}"#)
        .vector("getCoops", r#"{"payload.name": "{{name}}"}"#)
        .vector("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)
        .singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
        .vector_resolver("hens", None, "getHensByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getHen", r#"{"id": "{{id}}"}"#)
        .vector("getHens", r#"{"payload.name": "{{name}}"}"#)
        .vector("getHensByCoop", r#"{"payload.coopId": "{{id}}"}"#)
        .singleton_resolver("coop", Some("coopId"), "getCoop", "/coop/graph")
        .build();

    let server_config = ServerConfig {
        port: 0,
        graphlettes: vec![
            GraphletteConfig {
                path: "/farm/graph".into(),
                schema_text: FARM_GRAPHQL.into(),
                root_config: farm_config,
                searcher: farm_searcher,
            },
            GraphletteConfig {
                path: "/coop/graph".into(),
                schema_text: COOP_GRAPHQL.into(),
                root_config: coop_config,
                searcher: coop_searcher,
            },
            GraphletteConfig {
                path: "/hen/graph".into(),
                schema_text: HEN_GRAPHQL.into(),
                root_config: hen_config,
                searcher: hen_searcher,
            },
        ],
        restlettes: vec![
            RestletteConfig {
                path: "/farm/api".into(),
                schema_json: serde_json::json!({}),
                repository: farm_repo,
            },
            RestletteConfig {
                path: "/coop/api".into(),
                schema_json: serde_json::json!({}),
                repository: coop_repo,
            },
            RestletteConfig {
                path: "/hen/api".into(),
                schema_json: serde_json::json!({}),
                repository: hen_repo,
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
                let addr = build_farm_server().await;
                world.server_addr = Some(addr);
                world.ids.clear();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/farm.feature")
        .await;
}

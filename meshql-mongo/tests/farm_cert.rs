use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::farm;
use meshql_cert::CertWorld;
use meshql_core::{GraphletteConfig, NoAuth, RestletteConfig, RootConfig, ServerConfig};
use meshql_mongo::{MongoRepository, MongoSearcher};
use meshql_server::build_app;
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

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

async fn build_farm_server(mongo_uri: &str) -> String {
    let db = format!("farm_{}", uuid::Uuid::new_v4().simple());
    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);

    let farm_repo = Arc::new(
        MongoRepository::new(mongo_uri, &db, "farms", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let coop_repo = Arc::new(
        MongoRepository::new(mongo_uri, &db, "coops", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let hen_repo = Arc::new(
        MongoRepository::new(mongo_uri, &db, "hens", Arc::clone(&auth))
            .await
            .unwrap(),
    );

    let farm_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(mongo_uri, &db, "farms", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let coop_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(mongo_uri, &db, "coops", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let hen_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(mongo_uri, &db, "hens", Arc::clone(&auth))
            .await
            .unwrap(),
    );

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
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");

    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            let uri = mongo_uri.clone();
            Box::pin(async move {
                let addr = build_farm_server(&uri).await;
                world.server_addr = Some(addr);
                world.ids.clear();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/farm.feature")
        .await;

    drop(container);
}

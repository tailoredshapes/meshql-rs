//! Proves lay_report's REST payload shape is {henId, eggs, timeOfDay} —
//! the breaking schema migration in the farm-event-sourcing-retrofit spec.
//! Uses the same spawn-a-real-server-and-hit-it-with-reqwest convention as
//! meshql-restlette/tests/header_cert.rs and meshql-mongo/tests/farm_cert.rs.

use meshql_core::NoAuth;
use meshql_mongo::MongoRepository;
use meshql_restlette::build_restlette_router;
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

async fn spawn_lay_report_server(mongo_uri: &str) -> String {
    let db = format!("lay_report_schema_{}", uuid::Uuid::new_v4().simple());
    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);
    let repo = Arc::new(
        MongoRepository::new(mongo_uri, &db, "lay_reports", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let router = build_restlette_router("/lay_report/api", repo, auth);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn accepts_hen_id_eggs_time_of_day_shape() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let addr = spawn_lay_report_server(&mongo_uri).await;

    let resp = reqwest::Client::new()
        .post(format!("{addr}/lay_report/api"))
        .json(&serde_json::json!({
            "henId": "hen-1",
            "eggs": 2,
            "timeOfDay": "2026-07-22T08:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["henId"], "hen-1");
    assert_eq!(body["eggs"], 2);
    assert_eq!(body["timeOfDay"], "2026-07-22T08:00:00Z");
    // The old shape's fields must not be echoed back.
    assert!(body.get("date").is_none());
    assert!(body.get("count").is_none());
}

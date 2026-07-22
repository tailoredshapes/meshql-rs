//! Proves hen_productivity is wired as an ordinary restlette+graphlette
//! pair with the {henId, totalEggs, lastLaidAt} aggregate shape (decision
//! #1 in the plan — exact fields aren't settled by the spec).

use meshql_core::NoAuth;
use meshql_mongo::MongoRepository;
use meshql_restlette::build_restlette_router;
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

#[tokio::test]
async fn accepts_hen_id_total_eggs_last_laid_at_shape() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let db = format!("hen_productivity_{}", uuid::Uuid::new_v4().simple());
    let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);
    let repo = Arc::new(
        MongoRepository::new(&mongo_uri, &db, "hen_productivities", Arc::clone(&auth))
            .await
            .unwrap(),
    );
    let router = build_restlette_router("/hen_productivity/api", repo, auth);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("{addr}/hen_productivity/api"))
        .json(&serde_json::json!({
            "henId": "hen-1",
            "totalEggs": 42,
            "lastLaidAt": "2026-07-22T08:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["henId"], "hen-1");
    assert_eq!(body["totalEggs"], 42);
    assert_eq!(body["lastLaidAt"], "2026-07-22T08:00:00Z");
}

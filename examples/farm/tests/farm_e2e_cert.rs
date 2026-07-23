//! Proves the actual production wiring in `farm::build` — not a hand-rolled
//! duplicate — round-trips data through REST writes and GraphQL reads.
//!
//! This closes a real gap: `meshql-mongo/tests/farm_cert.rs` (and the shared
//! `farm.feature` it runs) already prove the *pattern* is correct with its
//! own copy of the RootConfig, but never exercises `examples/farm`'s own
//! code, and never calls the plain `getFarms`/`getCoops`/`getHens` vector
//! queries directly (only `getFarm`/federated `coops`/`hens` fields, which
//! go through different query templates). A previous version of this crate's
//! own `getFarms`/`getCoops`/`getHens`/`getCoopsByFarm`/`getHensByCoop`
//! templates filtered on top-level fields (`{"name": "{{name}}"}`) instead
//! of `payload`-nested ones (`{"payload.name": "{{name}}"}`), so every
//! payload document, which is always stored as `{payload: {name, ...}}`,
//! silently matched nothing. Nothing caught it until a human noticed the
//! symptom manually.

use farm::build;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

async fn spawn_farm_server(mongo_uri: &str, db_name: &str) -> String {
    let config = build(mongo_uri, db_name).await.unwrap();
    let app = meshql_server::build_app(config).await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn graphql(client: &reqwest::Client, addr: &str, path: &str, query: &str) -> serde_json::Value {
    let resp = client
        .post(format!("{addr}{path}"))
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GraphQL request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let errors = body.get("errors");
    assert!(
        errors.is_none() || errors == Some(&serde_json::Value::Null),
        "GraphQL errors: {:?}",
        errors
    );
    body["data"].clone()
}

#[tokio::test]
async fn farm_written_via_restlette_is_visible_via_graphlette() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let db_name = format!("farm_e2e_{}", uuid::Uuid::new_v4().simple());
    let addr = spawn_farm_server(&mongo_uri, &db_name).await;
    let client = reqwest::Client::new();

    // Write a farm via its restlette.
    let resp = client
        .post(format!("{addr}/farm/api"))
        .json(&serde_json::json!({ "name": "Emerdale" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Read it back via the plain list query — the code path that was broken.
    let data = graphql(
        &client,
        &addr,
        "/farm/graph",
        r#"{ getFarms(name: "Emerdale") { name } }"#,
    )
    .await;
    let farms = data["getFarms"].as_array().unwrap();
    assert!(
        farms.iter().any(|f| f["name"] == "Emerdale"),
        "expected getFarms(name: \"Emerdale\") to return the farm just created via REST, got {farms:?}"
    );
}

#[tokio::test]
async fn coop_written_via_restlette_is_visible_via_graphlette_by_name_and_by_farm() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let db_name = format!("farm_e2e_{}", uuid::Uuid::new_v4().simple());
    let addr = spawn_farm_server(&mongo_uri, &db_name).await;
    let client = reqwest::Client::new();

    let farm_resp = client
        .post(format!("{addr}/farm/api"))
        .json(&serde_json::json!({ "name": "Emerdale" }))
        .send()
        .await
        .unwrap();
    assert_eq!(farm_resp.status(), 201);
    let farm_data = graphql(
        &client,
        &addr,
        "/farm/graph",
        r#"{ getFarms(name: "Emerdale") { id } }"#,
    )
    .await;
    let farm_id = farm_data["getFarms"][0]["id"].as_str().unwrap().to_string();

    let coop_resp = client
        .post(format!("{addr}/coop/api"))
        .json(&serde_json::json!({ "name": "Red Barn", "farmId": farm_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(coop_resp.status(), 201);

    let by_name = graphql(
        &client,
        &addr,
        "/coop/graph",
        r#"{ getCoops(name: "Red Barn") { name } }"#,
    )
    .await;
    assert!(
        by_name["getCoops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Red Barn"),
        "expected getCoops(name: \"Red Barn\") to return the coop just created via REST, got {by_name:?}"
    );

    let by_farm = graphql(
        &client,
        &addr,
        "/coop/graph",
        &format!(r#"{{ getCoopsByFarm(id: "{farm_id}") {{ name }} }}"#),
    )
    .await;
    assert!(
        by_farm["getCoopsByFarm"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Red Barn"),
        "expected getCoopsByFarm(id: farm_id) to return the coop just created via REST, got {by_farm:?}"
    );
}

#[tokio::test]
async fn hen_written_via_restlette_is_visible_via_graphlette_by_name_and_by_coop() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let db_name = format!("farm_e2e_{}", uuid::Uuid::new_v4().simple());
    let addr = spawn_farm_server(&mongo_uri, &db_name).await;
    let client = reqwest::Client::new();

    let farm_resp = client
        .post(format!("{addr}/farm/api"))
        .json(&serde_json::json!({ "name": "Emerdale" }))
        .send()
        .await
        .unwrap();
    assert_eq!(farm_resp.status(), 201);
    let farm_data = graphql(
        &client,
        &addr,
        "/farm/graph",
        r#"{ getFarms(name: "Emerdale") { id } }"#,
    )
    .await;
    let farm_id = farm_data["getFarms"][0]["id"].as_str().unwrap().to_string();

    let coop_resp = client
        .post(format!("{addr}/coop/api"))
        .json(&serde_json::json!({ "name": "Red Barn", "farmId": farm_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(coop_resp.status(), 201);
    let coop_data = graphql(
        &client,
        &addr,
        "/coop/graph",
        r#"{ getCoops(name: "Red Barn") { id } }"#,
    )
    .await;
    let coop_id = coop_data["getCoops"][0]["id"].as_str().unwrap().to_string();

    let hen_resp = client
        .post(format!("{addr}/hen/api"))
        .json(&serde_json::json!({ "name": "Chuck", "coopId": coop_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(hen_resp.status(), 201);

    let by_name = graphql(
        &client,
        &addr,
        "/hen/graph",
        r#"{ getHens(name: "Chuck") { name } }"#,
    )
    .await;
    assert!(
        by_name["getHens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "Chuck"),
        "expected getHens(name: \"Chuck\") to return the hen just created via REST, got {by_name:?}"
    );

    let by_coop = graphql(
        &client,
        &addr,
        "/hen/graph",
        &format!(r#"{{ getHensByCoop(id: "{coop_id}") {{ name }} }}"#),
    )
    .await;
    assert!(
        by_coop["getHensByCoop"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "Chuck"),
        "expected getHensByCoop(id: coop_id) to return the hen just created via REST, got {by_coop:?}"
    );
}

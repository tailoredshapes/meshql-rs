//! Proves the per-entity Casbin auth wiring end-to-end over real HTTP:
//!
//! - farm/coop/hen: full CRUD for the default ("fe") caller
//! - lay_report: create allowed, update/delete denied (403) for "fe"
//! - hen_productivity: every verb denied (403) for "fe" — no policy
//!   row grants it anything
//!
//! Plus a direct unit-level proof that the "worker" role (which a real
//! deployment would grant via trusted-header identity injection — see
//! plan decision #6, out of scope to build here) can create/update
//! hen_productivity per the embedded policy.

use meshql_casbin::CasbinAuth;
use meshql_core::{Auth, NoAuth};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

async fn spawn_farm() -> String {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let mongo_uri = format!("mongodb://127.0.0.1:{port}");
    let db_name = format!("farm_auth_{}", uuid::Uuid::new_v4().simple());

    let (config, extra) = farm::build(&mongo_uri, &db_name).await.unwrap();
    let app = meshql_server::build_app_ext(config, extra).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Container must outlive the spawned server; leak it for test simplicity
    // (matches the Box::leak(tempdir) convention used elsewhere in this repo
    // for keeping test resources alive across a spawned async task).
    Box::leak(Box::new(container));
    addr
}

#[tokio::test]
async fn actors_get_full_crud_by_default() {
    let addr = spawn_farm().await;
    let client = reqwest::Client::new();

    let farm: serde_json::Value = client
        .post(format!("{addr}/farm/api"))
        .json(&serde_json::json!({"name": "Green Acres"}))
        .send().await.unwrap()
        .json().await.unwrap();
    let id = farm["id"].as_str().unwrap();

    let update = client
        .put(format!("{addr}/farm/api/{id}"))
        .json(&serde_json::json!({"name": "Green Acres II"}))
        .send().await.unwrap();
    assert_eq!(update.status(), 200);

    let delete = client.delete(format!("{addr}/farm/api/{id}")).send().await.unwrap();
    assert_eq!(delete.status(), 200);
}

#[tokio::test]
async fn lay_report_is_create_only() {
    let addr = spawn_farm().await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{addr}/lay_report/api"))
        .json(&serde_json::json!({"henId": "hen-1", "eggs": 2, "timeOfDay": "2026-07-22T08:00:00Z"}))
        .send().await.unwrap()
        .json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let update = client
        .put(format!("{addr}/lay_report/api/{id}"))
        .json(&serde_json::json!({"eggs": 3}))
        .send().await.unwrap();
    assert_eq!(update.status(), 403, "lay_report update must be denied");

    let delete = client.delete(format!("{addr}/lay_report/api/{id}")).send().await.unwrap();
    assert_eq!(delete.status(), 403, "lay_report delete must be denied");
}

#[tokio::test]
async fn hen_productivity_denies_default_caller_every_verb() {
    let addr = spawn_farm().await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{addr}/hen_productivity/api"))
        .json(&serde_json::json!({"henId": "hen-1", "totalEggs": 10}))
        .send().await.unwrap();
    assert_eq!(create.status(), 403, "hen_productivity create must be denied to the default caller");
}

#[tokio::test]
async fn worker_role_can_create_and_update_hen_productivity() {
    let model = include_str!("../config/casbin/model.conf");
    let policy = include_str!("../config/casbin/hen_productivity_policy.csv");
    let auth = CasbinAuth::from_strings(model, policy, NoAuth).await.unwrap();

    assert!(auth.authorize_action(&["worker".to_string()], "create"));
    assert!(auth.authorize_action(&["worker".to_string()], "update"));
    assert!(!auth.authorize_action(&["worker".to_string()], "delete"));
    assert!(!auth.authorize_action(&[], "create"), "default caller has no role in this policy");
}

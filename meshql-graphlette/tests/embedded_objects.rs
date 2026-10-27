//! A payload's nested objects resolve through object-typed fields that have
//! no resolver, as they do in meshobj and meshql-java.

use meshql_core::{Repository, RootConfig, Stash};
use meshql_graphlette::{build_schema, ResolverRegistry};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

const TRIP_GRAPHQL: &str = r#"
type Window {
    earliest: String
    latest: String
}
type Stop {
    name: String
    window: Window
}
type Trip {
    id: ID
    departure: Window
    stops: [Stop]
    tags: [String]
    missing: Window
}
type Query {
    getTrip(id: ID, at: Float): Trip
}
"#;

#[tokio::test]
async fn nested_objects_and_lists_of_objects_resolve_from_the_payload() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::from_str("sqlite::memory:").unwrap())
        .await
        .unwrap();
    let repo = SqliteRepository::new_with_pool(pool.clone()).await.unwrap();
    let searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());

    let payload: Stash = serde_json::from_value(json!({
        "departure": { "earliest": "09:00", "latest": "10:00" },
        "stops": [
            { "name": "a", "window": { "earliest": "11:00", "latest": "12:00" } },
            { "name": "b", "window": { "earliest": "13:00", "latest": "14:00" } }
        ],
        "tags": ["x", "y"]
    }))
    .unwrap();
    repo.create(
        meshql_core::Envelope::new("trip-1", payload, vec![]),
        &meshql_core::TokenSession::new(Vec::new()),
    )
    .await
    .unwrap();

    let root_config = RootConfig::builder()
        .singleton("getTrip", r#"{"id": "{{id}}"}"#)
        .build();
    let schema = build_schema(
        TRIP_GRAPHQL,
        &root_config,
        searcher,
        &ResolverRegistry::new(),
    )
    .expect("schema should build");

    let query = r#"{ getTrip(id: "trip-1") { departure { earliest latest } stops { name window { latest } } tags missing { earliest } } }"#;
    let session: Arc<dyn meshql_core::Session> = meshql_core::token_session(&["*".to_string()]);
    let res = schema
        .execute(async_graphql::Request::new(query).data(session))
        .await;
    assert!(res.errors.is_empty(), "{:?}", res.errors);
    let data = serde_json::to_value(res.data).unwrap();
    assert_eq!(
        data,
        json!({ "getTrip": {
            "departure": { "earliest": "09:00", "latest": "10:00" },
            "stops": [
                { "name": "a", "window": { "latest": "12:00" } },
                { "name": "b", "window": { "latest": "14:00" } }
            ],
            "tags": ["x", "y"],
            "missing": null
        }})
    );
}

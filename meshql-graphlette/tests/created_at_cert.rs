use meshql_core::{Repository, RootConfig, Stash};
use meshql_graphlette::{build_schema, ResolverRegistry};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

const WIDGET_GRAPHQL: &str = r#"
type Widget {
    id: ID
    name: String
    createdAt: String
}
type Query {
    getWidget(id: ID, at: Float): Widget
}
"#;

/// Schemas that don't opt in don't get the field at all — GraphQL simply
/// can't select an undeclared field, so there's nothing to assert beyond
/// "the opt-in schema above is what makes createdAt selectable."
#[tokio::test]
async fn opted_in_schema_resolves_created_at_as_rfc3339() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let repo = SqliteRepository::new_with_pool(pool.clone()).await.unwrap();
    let searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());

    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!("sprocket"));
    let created = repo
        .create(
            meshql_core::Envelope::new("widget-1", payload, vec![]),
            &meshql_core::TokenSession::new(Vec::new()),
        )
        .await
        .unwrap();

    let root_config = RootConfig::builder()
        .singleton("getWidget", r#"{"id": "{{id}}"}"#)
        .build();
    let schema = build_schema(
        WIDGET_GRAPHQL,
        &root_config,
        searcher,
        &ResolverRegistry::new(),
    )
    .expect("schema should build");

    let query = format!(
        r#"{{ getWidget(id: "{}") {{ name createdAt }} }}"#,
        created.id
    );
    // The session is named explicitly. There is no unset session: a graphlette
    // resolver with none fails closed, so a test that drives the schema
    // directly has to say who is asking, exactly as the HTTP route does.
    let session: std::sync::Arc<dyn meshql_core::Session> =
        meshql_core::token_session(&["*".to_string()]);
    let resp = schema
        .execute(async_graphql::Request::new(query).data(session))
        .await;
    assert!(resp.errors.is_empty(), "GraphQL errors: {:?}", resp.errors);

    let data = serde_json::to_value(resp.data).unwrap();
    let created_at = data["getWidget"]["createdAt"]
        .as_str()
        .expect("createdAt must be present and a string when the schema opts in");
    chrono::DateTime::parse_from_rfc3339(created_at)
        .expect("createdAt must be a valid RFC3339 timestamp");
}

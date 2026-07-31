//! Harness for the end-to-end authorization certification.
//!
//! The other certification suites authenticate with `star()` — a caller
//! entitled to everything. Against such a caller a working authorization
//! filter and a completely inert one return the same rows, so those suites
//! cannot see an authorization bug at all. This harness stands up a *real*
//! server — restlette for writes, graphlette for reads — behind edge
//! middleware that injects a caller identity exactly the way a deployment
//! does, and the certification drives it with two callers holding distinct,
//! non-wildcard credentials.
//!
//! The defects this is aimed at live *above* the Searcher: in the wiring that
//! carries identity from an HTTP request into the envelope and back out
//! again. Nothing below the HTTP surface is stubbed.

use axum::middleware::{self, Next};
use axum::Router;
use meshql_core::{
    Auth, AuthContext, GraphletteConfig, Repository, RestletteConfig, RootConfig, Searcher,
    ServerConfig, Stash, StashKeyAuth,
};
use std::sync::Arc;

/// The trusted header the edge uses to assert who the caller is. Everything
/// past the edge trusts it — authentication is the edge's job, authorization
/// is the framework's.
pub const IDENTITY_HEADER: &str = "x-meshql-user";

/// Stash key the edge writes the identity under, and the key the server's
/// `Auth` reads it back out of.
pub const IDENTITY_KEY: &str = "user";

/// REST collection the certification writes through.
pub const RESTLETTE_PATH: &str = "/widget/api";

/// GraphQL surface the certification reads through.
pub const GRAPHLETTE_PATH: &str = "/widget/graph";

pub const WIDGET_GRAPHQL: &str = r#"
type Widget {
    id: ID
    name: String
    kind: String
}
type Query {
    getById(id: ID, at: Float): Widget
    getByKind(kind: String, at: Float): [Widget]
}
"#;

/// The `Auth` the certified server runs on: identity comes out of the
/// request-scoped stash the edge populated. Deliberately *not* `NoAuth` —
/// `NoAuth` resolves every caller to `["*"]`, which is the blind spot this
/// whole suite exists to close.
pub fn edge_auth() -> Arc<dyn Auth> {
    Arc::new(StashKeyAuth::new(IDENTITY_KEY))
}

/// Tokens the configured `Auth` resolves for a caller — the same computation
/// the restlette performs when stamping an envelope, so a test can assert the
/// stored tokens against it rather than against a hand-written literal.
pub fn tokens_for(caller: &str) -> Vec<String> {
    edge_auth().get_auth_token(&stash_for(caller))
}

/// The stash the edge middleware would build for a caller. `None` identity
/// (an anonymous caller) yields an empty stash.
pub fn stash_for(caller: &str) -> Stash {
    let mut stash = Stash::new();
    if let Some(id) = identity_of(caller) {
        stash.insert(IDENTITY_KEY.to_string(), serde_json::Value::String(id));
    }
    stash
}

/// Map a certification caller name onto the identity the edge asserts.
///
/// `anonymous` sends no header at all: presenting no credentials is
/// deliberately permitted (you are inside the trust boundary, or auth is
/// off), and that semantic is certified here, not removed.
pub fn identity_of(caller: &str) -> Option<String> {
    match caller {
        "anonymous" => None,
        // The wildcard caller — a system/internal reader. Kept in the suite so
        // its behaviour stays certified, but never used as the *only* caller
        // in a scenario.
        "root" => Some("*".to_string()),
        other => Some(other.to_string()),
    }
}

/// Edge middleware: lift the trusted identity header into an `AuthContext`.
/// This is the production shape — see `meshql-changes/tests/sse_integration.rs`.
async fn edge_identity(mut req: axum::extract::Request, next: Next) -> axum::response::Response {
    let mut stash = Stash::new();
    if let Some(user) = req
        .headers()
        .get(IDENTITY_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        stash.insert(
            IDENTITY_KEY.to_string(),
            serde_json::Value::String(user.to_string()),
        );
    }
    req.extensions_mut().insert(AuthContext::new(stash));
    next.run(req).await
}

/// The queries the certified server exposes.
///
/// Public so that an adapter which derives physical structures from query
/// configuration — `meshql-dynamo` derives its secondary indexes — can certify
/// against *this* config rather than a transcription of it. A transcription is
/// exactly the drift such a derivation exists to remove.
pub fn root_config() -> RootConfig {
    RootConfig::builder()
        .singleton("getById", r#"{"id": "{{id}}"}"#)
        .vector("getByKind", r#"{"payload.kind": "{{kind}}"}"#)
        .build()
}

/// Stand up the certified server on an ephemeral port and return its base URL.
///
/// One entity ("widget"), one restlette, one graphlette — the smallest surface
/// that still exercises the whole identity path: request header → `Auth` →
/// envelope tokens → storage → searcher → response.
pub async fn start_server(repo: Arc<dyn Repository>, searcher: Arc<dyn Searcher>) -> String {
    let root_config = root_config();

    let server_config = ServerConfig {
        port: 0,
        graphlettes: vec![GraphletteConfig {
            path: GRAPHLETTE_PATH.into(),
            schema_text: WIDGET_GRAPHQL.into(),
            root_config,
            searcher,
        }],
        restlettes: vec![RestletteConfig {
            path: RESTLETTE_PATH.into(),
            schema_json: serde_json::json!({}),
            repository: repo,
        }],
    };

    let app = meshql_server::build_app_with_auth(server_config, edge_auth(), Router::new())
        .await
        .expect("build authz cert app")
        .layer(middleware::from_fn(edge_identity));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://127.0.0.1:{}", addr.port())
}

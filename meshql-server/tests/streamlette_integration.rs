//! End-to-end for `build_app_with_streams`: a real sqlite store, a real
//! `SearcherTail`, a real axum server, and a real SSE client over HTTP.
//!
//! Lives in `meshql-server` (not `meshql-changes`) because it drives
//! `build_app_with_streams`; putting it the other way round would need a
//! `meshql-server` dev-dep on a crate that already dev-deps this one.

use axum::middleware::{self, Next};
use axum::Router;
use meshql_changes::{SearcherTail, StreamSource, StreamletteConfig};
use meshql_core::{
    Auth, AuthContext, Envelope, Repository, RestletteConfig, ServerConfig, Stash, StashKeyAuth,
};
use meshql_server::build_app_with_streams;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Trusted-header identity, as production edge middleware would inject.
async fn edge_identity(mut req: axum::extract::Request, next: Next) -> axum::response::Response {
    let mut stash = Stash::new();
    if let Some(user) = req.headers().get("x-user").and_then(|v| v.to_str().ok()) {
        stash.insert("user".to_string(), json!(user));
    }
    req.extensions_mut().insert(AuthContext::new(stash));
    next.run(req).await
}

struct Server {
    base: String,
    repo: Arc<SqliteRepository>,
}

/// Boot a deployment with one restlette and one `Tail`-sourced streamlette
/// over the SAME sqlite store, under `auth`.
async fn start(auth: Arc<dyn Auth>) -> Server {
    // max_connections(1): each `sqlite::memory:` connection is its own DB,
    // and the spawned pump polls concurrently with test-task writes.
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher = Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());

    let config = ServerConfig {
        port: 0,
        graphlettes: vec![],
        restlettes: vec![RestletteConfig {
            path: "/hen/api".to_string(),
            schema_json: json!({}),
            repository: repo.clone(),
        }],
    };

    let streamlette = StreamletteConfig {
        path: "/hen/stream".to_string(),
        entity: "hen".to_string(),
        source: StreamSource::Tail {
            source: Arc::new(SearcherTail::new("hen", searcher, repo.clone())),
            poll_interval: Duration::from_millis(20),
        },
        hub_capacity: 64,
    };

    let app: Router = build_app_with_streams(config, Router::new(), vec![streamlette], auth)
        .await
        .unwrap()
        .layer(middleware::from_fn(edge_identity));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Server {
        base: format!("http://{addr}"),
        repo,
    }
}

fn payload(name: &str) -> Stash {
    let mut s = Stash::new();
    s.insert("name".to_string(), json!(name));
    s
}

/// Read the SSE body until a complete frame satisfying `pred` arrives, and
/// return everything read so far. Cannot read to completion — the handler
/// sets `.keep_alive()`, so the body never ends — hence the deadline.
async fn wire_until(
    resp: reqwest::Response,
    pred: impl Fn(&str) -> bool,
) -> Result<String, String> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| format!("timeout; frames so far:\n{buf}"))?
            .ok_or_else(|| format!("stream ended; frames so far:\n{buf}"))?
            .map_err(|e| e.to_string())?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if pred(&buf) {
            return Ok(buf);
        }
    }
}

fn frame_data(wire: &str, event: &str) -> serde_json::Value {
    let frame = wire
        .split("\n\n")
        .find(|f| {
            f.lines().any(|l| {
                l.trim() == format!("event:{event}") || l.trim() == format!("event: {event}")
            })
        })
        .unwrap_or_else(|| panic!("no `{event}` frame in:\n{wire}"));
    let data = frame
        .lines()
        .find_map(|l| l.trim_end().strip_prefix("data:"))
        .unwrap_or_else(|| panic!("no data line on the `{event}` frame in:\n{wire}"))
        .trim_start();
    serde_json::from_str(data)
        .unwrap_or_else(|e| panic!("`{event}` data is not JSON ({e}): {data}"))
}

/// The milestone assertion: a deployment booted through `meshql-server`
/// serves a real per-entity SSE stream — `ready` first, then a `change`
/// frame for a write that lands after the connection is open.
#[tokio::test]
async fn a_mounted_streamlette_streams_a_write_over_http() {
    let server = start(Arc::new(meshql_core::NoAuth)).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/hen/stream", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    let env = server
        .repo
        .create(
            Envelope::new("hen-1", payload("henrietta"), vec![]),
            &meshql_core::TokenSession::new(vec!["*".to_string()]),
        )
        .await
        .unwrap();

    let wire = wire_until(resp, |b| b.contains("hen-1")).await.unwrap();

    let ready_at = wire
        .find("event:ready")
        .or_else(|| wire.find("event: ready"));
    let change_at = wire
        .find("event:change")
        .or_else(|| wire.find("event: change"));
    assert!(ready_at.is_some(), "no ready frame in:\n{wire}");
    assert!(
        ready_at < change_at,
        "ready must precede the first change frame, got:\n{wire}"
    );

    // A `Tail` source cannot resume, and the ready frame must say so.
    let ready = frame_data(&wire, "ready");
    assert_eq!(ready["resume"], false);
    assert_eq!(ready["cursor"], serde_json::Value::Null);

    let change = frame_data(&wire, "change");
    assert_eq!(change["entity"], "hen");
    assert_eq!(change["id"], "hen-1");
    assert_eq!(change["deleted"], false);
    assert_eq!(change["created_at"], env.created_at.timestamp_millis());
}

/// Regression guard for the delegation inside `build_app_with_streams`.
///
/// If it delegated to `build_app_ext` instead of `build_app_with_auth`, the
/// streamlette would get the caller's real `Auth` while every restlette and
/// graphlette silently ran on `NoAuth`, whose session authorizes every read —
/// so bob would read alice's record and this test fails. It passes only when
/// the real `Auth` reached the lettes.
#[tokio::test]
async fn streamlettes_do_not_downgrade_the_lettes_to_noauth() {
    let server = start(Arc::new(StashKeyAuth::new("user"))).await;
    let client = reqwest::Client::new();

    server
        .repo
        .create(
            Envelope::new("secret-hen", payload("classified"), vec![]),
            &meshql_core::TokenSession::new(vec!["alice".to_string()]),
        )
        .await
        .unwrap();

    // Alice holds the token the envelope is tagged with.
    let alice = client
        .get(format!("{}/hen/api/secret-hen", server.base))
        .header("x-user", "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(
        alice.status(),
        200,
        "alice's own record must still be readable"
    );

    // Bob does not. Under a leaked NoAuth this would be a 200.
    let bob = client
        .get(format!("{}/hen/api/secret-hen", server.base))
        .header("x-user", "bob")
        .send()
        .await
        .unwrap();
    assert_eq!(
        bob.status(),
        404,
        "the restlette ran on NoAuth: build_app_with_streams must delegate to \
         build_app_with_auth, not build_app_ext"
    );
}

/// The same `Auth` also gates the stream itself — bob must not be notified
/// about an envelope he cannot read.
#[tokio::test]
async fn a_mounted_streamlette_filters_by_the_same_auth_as_the_lettes() {
    let server = start(Arc::new(StashKeyAuth::new("user"))).await;
    let client = reqwest::Client::new();

    let bob = client
        .get(format!("{}/hen/stream", server.base))
        .header("x-user", "bob")
        .send()
        .await
        .unwrap();

    server
        .repo
        .create(
            Envelope::new("secret-hen", payload("classified"), vec![]),
            &meshql_core::TokenSession::new(vec!["alice".to_string()]),
        )
        .await
        .unwrap();
    // A public marker so the test terminates on a real frame rather than a
    // timeout, proving the stream was live throughout.
    server
        .repo
        .create(
            Envelope::new("public-hen", payload("open"), vec![]),
            &meshql_core::TokenSession::new(vec![]),
        )
        .await
        .unwrap();

    let wire = wire_until(bob, |b| b.contains("public-hen")).await.unwrap();
    assert!(
        !wire.contains("secret-hen"),
        "alice's envelope leaked into bob's stream:\n{wire}"
    );
}

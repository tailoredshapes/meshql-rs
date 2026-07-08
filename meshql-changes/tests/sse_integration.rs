//! End-to-end: real sqlite storage, real tail, real axum server, real SSE
//! over HTTP. Auth path mirrors production: edge middleware populates
//! AuthContext from a trusted header; StashKeyAuth extracts identity.

use axum::middleware::{self, Next};
use axum::Router;
use meshql_changes::{changes_router, run_tails, ChangeHub, SearcherTail};
use meshql_core::{Auth, AuthContext, Envelope, Repository, Stash, StashKeyAuth};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

async fn edge_identity(mut req: axum::extract::Request, next: Next) -> axum::response::Response {
    // Trusted-header identity, as production edge middleware would inject.
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

async fn start(auth: Arc<dyn Auth>) -> Server {
    // max_connections(1): each sqlite::memory: connection is its own DB,
    // and the spawned tail polls concurrently with test-task writes.
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

    let hub = ChangeHub::new(64);
    let tail = Arc::new(SearcherTail::new("hen", searcher, repo.clone()));
    tokio::spawn(run_tails(hub.clone(), vec![tail], Duration::from_millis(20)));

    let app: Router =
        changes_router("/changes", hub, auth).layer(middleware::from_fn(edge_identity));
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

/// Read from the SSE body until a COMPLETE `data:` line satisfying `pred`
/// arrives, or time out. Only scans newline-terminated lines — a chunk
/// boundary can split a line mid-JSON, so the unterminated remainder stays
/// in `buf` until its newline arrives.
async fn await_data_line(
    resp: reqwest::Response,
    pred: impl Fn(&str) -> bool,
) -> Result<String, String> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| format!("timeout; buffer so far: {buf}"))?
            .ok_or_else(|| format!("stream ended; buffer: {buf}"))?
            .map_err(|e| e.to_string())?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            if let Some(data) = line.trim_end().strip_prefix("data: ") {
                if pred(data) {
                    return Ok(data.to_string());
                }
            }
        }
    }
}

#[tokio::test]
async fn create_and_delete_notifications_arrive_over_http() {
    let server = start(Arc::new(meshql_core::NoAuth)).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/changes", server.base))
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

    // Create
    let env = server
        .repo
        .create(
            Envelope::new("hen-1", payload("henrietta"), vec![]),
            &["*".to_string()],
        )
        .await
        .unwrap();
    let data = await_data_line(resp, |d| d.contains("hen-1")).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["entity"], "hen");
    assert_eq!(v["deleted"], false);
    assert_eq!(v["created_at"], env.created_at.timestamp_millis());

    // Delete → deleted:true notification on a fresh connection
    let resp2 = client
        .get(format!("{}/changes", server.base))
        .send()
        .await
        .unwrap();
    server
        .repo
        .remove("hen-1", &["*".to_string()])
        .await
        .unwrap();
    let data = await_data_line(resp2, |d| d.contains("hen-1") && d.contains("true"))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["deleted"], true);
}

#[tokio::test]
async fn subscribers_only_see_envelopes_their_tokens_allow() {
    let server = start(Arc::new(StashKeyAuth::new("user"))).await;
    let client = reqwest::Client::new();

    let alice = client
        .get(format!("{}/changes", server.base))
        .header("x-user", "alice")
        .send()
        .await
        .unwrap();
    let bob = client
        .get(format!("{}/changes", server.base))
        .header("x-user", "bob")
        .send()
        .await
        .unwrap();

    // alice-only envelope, then a public marker envelope.
    // Repository::create tags the envelope with its `tokens` argument.
    server
        .repo
        .create(
            Envelope::new("secret-hen", payload("classified"), vec![]),
            &["alice".to_string()],
        )
        .await
        .unwrap();
    server
        .repo
        .create(
            Envelope::new("public-hen", payload("open"), vec![]),
            &[],
        )
        .await
        .unwrap();

    // Alice sees the secret envelope.
    await_data_line(alice, |d| d.contains("secret-hen"))
        .await
        .unwrap();

    // Bob's FIRST hen event must be the public one — the secret was filtered.
    let bob_first = await_data_line(bob, |d| d.contains("-hen")).await.unwrap();
    assert!(
        bob_first.contains("public-hen"),
        "bob's first event should be public-hen, got: {bob_first}"
    );
}

#[tokio::test]
async fn entities_param_filters_the_stream() {
    let server = start(Arc::new(meshql_core::NoAuth)).await;
    let client = reqwest::Client::new();
    // Subscribe to a different entity than the one the tail feeds.
    let resp = client
        .get(format!("{}/changes?entities=farm", server.base))
        .send()
        .await
        .unwrap();

    server
        .repo
        .create(
            Envelope::new("hen-x", payload("x"), vec![]),
            &["*".to_string()],
        )
        .await
        .unwrap();

    // No hen event should arrive; expect timeout.
    let res = await_data_line(resp, |d| d.contains("hen-x")).await;
    assert!(res.is_err(), "hen event leaked through entities=farm filter");
}

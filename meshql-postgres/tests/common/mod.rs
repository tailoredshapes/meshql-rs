//! One Postgres container per test binary, shared by every test in it.

use std::sync::{Arc, OnceLock, Weak};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Mutex;

/// A running Postgres, and the URL to reach it.
pub struct PostgresNode {
    pub url: String,
    _container: ContainerAsync<Postgres>,
}

/// Held as a `Weak`, so the container is dropped once the last test holding it
/// finishes — and dropped *inside that test's runtime*, which `ContainerAsync`
/// requires: its `Drop` reaches for `Handle::current()`. A container parked in
/// a `static` would never be dropped at all, and testcontainers has no reaper
/// to collect it afterwards (its watchdog is feature-gated off), so it would
/// outlive the process.
static NODE: OnceLock<Mutex<Weak<PostgresNode>>> = OnceLock::new();

/// This binary's Postgres, started on first use.
///
/// Every test names its own table, so a single server serves all of them
/// instead of one container each — which is what forced the suite down to a
/// handful of test threads and made it spend nearly all its time starting
/// databases rather than exercising them.
///
/// Hold the returned `Arc` for the length of the test. Dropping it early lets
/// the container go while other tests are still using it.
pub async fn shared_postgres() -> Arc<PostgresNode> {
    let mut slot = NODE.get_or_init(|| Mutex::new(Weak::new())).lock().await;
    if let Some(running) = slot.upgrade() {
        return running;
    }

    // Every test opens its own repository and searcher pool against this one
    // server, and sqlx pools grow to 10 connections apiece — comfortably past
    // the stock limit of 100 once the whole binary runs at once.
    let container = Postgres::default()
        .with_cmd(["postgres", "-c", "max_connections=500"])
        .start()
        .await
        .expect("start postgres");
    // Resolve the port here and keep it: a later lookup would go back through
    // the Docker client built in whichever test's runtime started the
    // container, and that runtime is gone the moment that test returns.
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres host port");

    let node = Arc::new(PostgresNode {
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        _container: container,
    });
    *slot = Arc::downgrade(&node);
    node
}

/// A table name no other test will touch.
pub fn fresh_table() -> String {
    format!("env_{}", uuid::Uuid::new_v4().simple())
}

//! One Mongo container per test binary, shared by every test in it.

use std::sync::{Arc, OnceLock, Weak};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::mongo::Mongo;
use tokio::sync::Mutex;

/// A running Mongo, and the URI to reach it.
pub struct MongoNode {
    pub uri: String,
    _container: ContainerAsync<Mongo>,
}

/// Held as a `Weak`, so the container is dropped once the last test holding it
/// finishes — and dropped *inside that test's runtime*, which `ContainerAsync`
/// requires: its `Drop` reaches for `Handle::current()`. A container parked in
/// a `static` would never be dropped at all, and testcontainers has no reaper
/// to collect it afterwards (its watchdog is feature-gated off), so it would
/// outlive the process.
static NODE: OnceLock<Mutex<Weak<MongoNode>>> = OnceLock::new();

/// This binary's Mongo, started on first use.
///
/// Every test names its own collection, so a single server serves all of them.
/// They used to get one container each: twenty simultaneous `docker run`s, each
/// mongod sizing its WiredTiger cache against the whole host, and Docker
/// failing about half of them with `409 can not get logs from container which
/// is dead or marked for removal`. Passing then took `--test-threads=3`, which
/// serialized the tests themselves and not just the startup they were really
/// contending over.
///
/// Hold the returned `Arc` for the length of the test. Dropping it early lets
/// the container go while other tests are still using it — they would then pay
/// to start another one.
pub async fn shared_mongo() -> Arc<MongoNode> {
    let mut slot = NODE.get_or_init(|| Mutex::new(Weak::new())).lock().await;
    if let Some(running) = slot.upgrade() {
        return running;
    }

    let container = Mongo::default().start().await.expect("start mongo");
    // Resolve the port here and keep it: a later lookup would go back through
    // the Docker client built in whichever test's runtime started the
    // container, and that runtime is gone the moment that test returns.
    let port = container
        .get_host_port_ipv4(27017)
        .await
        .expect("mongo host port");

    let node = Arc::new(MongoNode {
        uri: format!("mongodb://127.0.0.1:{port}"),
        _container: container,
    });
    *slot = Arc::downgrade(&node);
    node
}

/// A collection name no other test will touch.
pub fn fresh_collection() -> String {
    format!("test_{}", uuid::Uuid::new_v4().simple())
}

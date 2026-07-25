//! One MySQL container per test binary, shared by every test in it.

use std::sync::{Arc, OnceLock, Weak};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::mysql::Mysql;
use tokio::sync::Mutex;

/// A running MySQL, and the URL to reach it.
pub struct MysqlNode {
    pub url: String,
    _container: ContainerAsync<Mysql>,
}

/// Held as a `Weak`, so the container is dropped once the last test holding it
/// finishes — and dropped *inside that test's runtime*, which `ContainerAsync`
/// requires: its `Drop` reaches for `Handle::current()`. A container parked in
/// a `static` would never be dropped at all, and testcontainers has no reaper
/// to collect it afterwards (its watchdog is feature-gated off), so it would
/// outlive the process.
static NODE: OnceLock<Mutex<Weak<MysqlNode>>> = OnceLock::new();

/// This binary's MySQL, started on first use.
///
/// Every test names its own table, so a single server serves all of them
/// instead of one container each. MySQL is the slowest of the three to boot, so
/// it paid that cost twenty times over: the searcher cert took about five
/// minutes, nearly all of it waiting on `mysqld` rather than exercising it.
///
/// Hold the returned `Arc` for the length of the test. Dropping it early lets
/// the container go while other tests are still using it.
pub async fn shared_mysql() -> Arc<MysqlNode> {
    let mut slot = NODE.get_or_init(|| Mutex::new(Weak::new())).lock().await;
    if let Some(running) = slot.upgrade() {
        return running;
    }

    // Every test opens its own repository and searcher pool against this one
    // server, and sqlx pools grow to 10 connections apiece — past the stock
    // limit once the whole binary runs at once.
    let container = Mysql::default()
        .with_cmd(["mysqld", "--max-connections=500"])
        .start()
        .await
        .expect("start mysql");
    // Resolve the port here and keep it: a later lookup would go back through
    // the Docker client built in whichever test's runtime started the
    // container, and that runtime is gone the moment that test returns.
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql host port");

    let node = Arc::new(MysqlNode {
        // testcontainers-modules mysql defaults: root with empty password, db = "test"
        url: format!("mysql://root:@127.0.0.1:{port}/test"),
        _container: container,
    });
    *slot = Arc::downgrade(&node);
    node
}

/// A table name no other test will touch.
pub fn fresh_table() -> String {
    format!("env_{}", uuid::Uuid::new_v4().simple())
}

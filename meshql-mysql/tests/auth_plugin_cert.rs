//! The auth plugin decides, and nothing else does — MySQL adapter.
//! See `meshql-cert/tests/features/auth_plugin.feature`.
//!
//! Unlike `authz_cert`, the before-hook stands up storage only. The server
//! comes up in the `Given` step, because each scenario names its own plugin —
//! and they are plugins no adapter can second-guess, which is the whole point
//! of this suite: an adapter passes only if every surface actually asks.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_mysql::{MysqlRepository, MysqlSearcher};
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mysql::Mysql;

#[tokio::main]
async fn main() {
    let container = Mysql::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(3306).await.unwrap();
    let url = format!("mysql://root:@127.0.0.1:{port}/test");

    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            let url = url.clone();
            Box::pin(async move {
                // A table per scenario, so scenarios cannot see each other's rows.
                let table = format!("plugin_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                let repo = Arc::new(MysqlRepository::new_with_table(&url, &table).await.unwrap());
                let searcher = Arc::new(MysqlSearcher::new_with_table(&url, &table).await.unwrap());

                world.set_repo(repo);
                world.set_searcher(searcher);
                world.reset_authz();
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/auth_plugin.feature")
        .await;

    drop(container);
}

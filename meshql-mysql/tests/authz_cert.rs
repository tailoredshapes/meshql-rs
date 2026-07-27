//! End-to-end authorization certification, MySQL adapter.
//! See `meshql-cert/tests/features/authz.feature`.

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
                let table = format!("authz_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                let repo = Arc::new(MysqlRepository::new_with_table(&url, &table).await.unwrap());
                let searcher = Arc::new(MysqlSearcher::new_with_table(&url, &table).await.unwrap());

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;

    drop(container);
}

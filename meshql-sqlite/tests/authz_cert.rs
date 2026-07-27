//! End-to-end authorization certification, SQLite adapter.
//! See `meshql-cert/tests/features/authz.feature`.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(|_feature, _rule, _scenario, world| {
            Box::pin(async move {
                // max_connections(1): every `sqlite::memory:` connection is its
                // own database, so one connection is what makes the repository
                // and the searcher see the same rows.
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(
                        SqliteConnectOptions::from_str("sqlite::memory:")
                            .unwrap()
                            .create_if_missing(true),
                    )
                    .await
                    .unwrap();

                let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
                let searcher = Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;
}

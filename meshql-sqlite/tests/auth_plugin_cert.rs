//! The auth plugin decides, and nothing else does — SQLite adapter.
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
}

//! The egg economy certification on SQLite: the contract's
//! `egg_economy.feature` and this repository's graph feature over the
//! fourteen-entity deployment in `meshql_cert::egg_economy`. Each scenario gets
//! a fresh in-memory database per entity.
use meshql_cert::egg_economy::{self, Store};
#[allow(unused_imports)]
use meshql_cert::steps;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

async fn make_store() -> Store {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    Store {
        repo: Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap()),
        searcher: Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap()),
    }
}

async fn make_stores() -> HashMap<String, Store> {
    let mut stores = HashMap::new();
    for entity in egg_economy::entities() {
        stores.insert(entity.to_string(), make_store().await);
    }
    stores
}

#[tokio::main]
async fn main() {
    egg_economy::run(make_stores).await;
}

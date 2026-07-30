//! Shared fixtures for the meshql-dynamo certification suites.
//!
//! Every test gets its **own uniquely-named table**. Several certs assert exact
//! result counts (`test_searcher_find_all_by_type` wants exactly 2,
//! `test_searcher_result_carries_id_and_created_at` wants exactly 2), so a
//! shared table would make the suites fail whenever they ran concurrently —
//! which, since `cargo test` is threaded by default, is always.
//!
//! Tables are dropped by `cleanup()` on the happy path. A panicking test leaks
//! its table, deliberately: teardown in a `Drop` cannot await, and making a
//! failure noisier than the assertion that caused it is worse than leaving a
//! table behind in DynamoDB Local.

#![allow(dead_code)]

use aws_sdk_dynamodb::Client;
use meshql_dynamo::{DynamoRepository, DynamoSearcher};

/// Where DynamoDB Local lives. Overridable so the suites can be pointed at a
/// container on another port without a recompile.
pub fn endpoint() -> String {
    std::env::var("MESHQL_DYNAMO_ENDPOINT").unwrap_or_else(|_| "http://localhost:8123".into())
}

pub fn fresh_table_name() -> String {
    format!("meshql_dynamo_cert_{}", uuid::Uuid::new_v4().simple())
}

/// One client, dummy credentials and region baked in, so a developer with no
/// AWS profile can run the suites.
pub async fn client() -> Client {
    meshql_dynamo::make_client(Some(&endpoint())).await
}

pub struct RepoFixture {
    pub client: Client,
    pub table: String,
    pub repo: DynamoRepository,
}

impl RepoFixture {
    pub async fn new() -> Self {
        let client = client().await;
        let table = fresh_table_name();
        let repo = DynamoRepository::new_with_client(client.clone(), &table)
            .await
            .expect("create the cert table");
        Self {
            client,
            table,
            repo,
        }
    }

    /// A fixture with the repository-authorization seed already written.
    pub async fn seeded_repo_auth() -> Self {
        let fixture = Self::new().await;
        meshql_core::testing::seed_repository_auth_data(&fixture.repo).await;
        fixture
    }

    pub async fn cleanup(self) {
        let _ = meshql_dynamo::drop_table(&self.client, &self.table).await;
    }
}

pub struct SearcherFixture {
    pub client: Client,
    pub table: String,
    pub repo: DynamoRepository,
    pub searcher: DynamoSearcher,
}

impl SearcherFixture {
    /// Repository and searcher over one client and one table — the searcher has
    /// no write path, so the seed goes in through the repository.
    pub async fn new() -> Self {
        let client = client().await;
        let table = fresh_table_name();
        let repo = DynamoRepository::new_with_client(client.clone(), &table)
            .await
            .expect("create the cert table");
        let searcher = DynamoSearcher::new_with_client(client.clone(), &table)
            .await
            .expect("searcher over the same table");
        Self {
            client,
            table,
            repo,
            searcher,
        }
    }

    /// Every searcher seed, matching `meshql-sqlite/tests/searcher_cert.rs`.
    pub async fn seeded() -> Self {
        let fixture = Self::new().await;
        let cert = meshql_core::testing::seed_searcher_data(&fixture.repo);
        cert.await;
        meshql_core::testing::seed_searcher_auth_data(&fixture.repo).await;
        meshql_core::testing::seed_searcher_ordering_data(&fixture.repo).await;
        meshql_core::testing::seed_searcher_result_shape_data(&fixture.repo).await;
        fixture
    }

    pub async fn cleanup(self) {
        let _ = meshql_dynamo::drop_table(&self.client, &self.table).await;
    }
}

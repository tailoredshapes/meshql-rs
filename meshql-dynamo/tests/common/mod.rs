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
//!
//! # Every fixture comes in two
//!
//! [`Indexing::Off`] is the plain adapter: every non-`id` search is a `Scan`.
//! [`Indexing::On`] derives global secondary indexes from [`cert_config`] — the
//! templates the certification suites actually use — and serves those searches
//! two-phase from an index instead.
//!
//! The suites run **both**, case for case, through [`cert_case`]. That is the
//! whole point: indexing is a change to what a search *costs* and it must be no
//! change at all to what a search *means*. A cert that only ever ran against
//! one of the two paths would certify half an adapter, and the half it did not
//! run is the half where a superseded version can leak back into a result set.

#![allow(dead_code)]

use aws_sdk_dynamodb::Client;
use meshql_core::RootConfig;
use meshql_dynamo::{DynamoCollection, DynamoRepository, DynamoSearcher};

/// Whether a fixture's table carries derived indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexing {
    Off,
    On,
}

/// The query templates the certification suites use, as the `RootConfig` a
/// deployment would hand to a graphlette.
///
/// The indexed fixtures derive their index set from *this*, by exactly the call
/// a deployment makes — `DynamoCollection::open(.., &config)` — rather than
/// naming the fields. If a cert starts filtering on a field this config does not
/// mention, the indexed run fails loudly on the guard, which is the behaviour
/// under test.
pub fn cert_config() -> RootConfig {
    RootConfig::builder()
        .singleton("byId", r#"{"id": "{{id}}"}"#)
        .vector("byName", r#"{"payload.name": "{{name}}"}"#)
        .vector("byType", r#"{"payload.type": "{{type}}"}"#)
        .vector(
            "byTypeAndName",
            r#"{"payload.type": "{{type}}", "payload.name": "{{name}}"}"#,
        )
        .vector("getAll", r#"{}"#)
        .build()
}

/// Run one certification function against **both** the unindexed and the
/// indexed adapter, as two separate tests.
///
/// ```ignore
/// cert_case!(should_find_by_name, SearcherFixture, cert::test_searcher_find_by_name);
/// ```
///
/// yields `should_find_by_name::unindexed` and `should_find_by_name::indexed`,
/// so a divergence names which path broke.
#[macro_export]
macro_rules! cert_case {
    ($name:ident, $fixture:ident, $cert:path) => {
        mod $name {
            use super::*;
            #[tokio::test]
            async fn unindexed() {
                let f = $fixture::seeded(Indexing::Off).await;
                $cert(f.subject()).await;
                f.cleanup().await;
            }
            #[tokio::test]
            async fn indexed() {
                let f = $fixture::seeded(Indexing::On).await;
                $cert(f.subject()).await;
                f.cleanup().await;
            }
        }
    };
}

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

/// Repository and searcher over one client and one table, built the way a
/// deployment builds them: through [`DynamoCollection`] when indexed, so the two
/// halves cannot hold different plans.
async fn collection(table: &str, indexing: Indexing) -> (Client, DynamoRepository, DynamoSearcher) {
    let client = client().await;
    match indexing {
        Indexing::Off => {
            let repo = DynamoRepository::new_with_client(client.clone(), table)
                .await
                .expect("create the cert table");
            let searcher = DynamoSearcher::new_with_client(client.clone(), table)
                .await
                .expect("searcher over the same table");
            (client, repo, searcher)
        }
        Indexing::On => {
            let (repo, searcher) =
                DynamoCollection::open_with_client(client.clone(), table, &cert_config())
                    .await
                    .expect("create the indexed cert table")
                    .split();
            (client, repo, searcher)
        }
    }
}

pub struct RepoFixture {
    pub client: Client,
    pub table: String,
    pub repo: DynamoRepository,
}

impl RepoFixture {
    pub async fn new(indexing: Indexing) -> Self {
        let table = fresh_table_name();
        let (client, repo, _) = collection(&table, indexing).await;
        Self {
            client,
            table,
            repo,
        }
    }

    /// What [`cert_case`] hands the certification function.
    pub fn subject(&self) -> &DynamoRepository {
        &self.repo
    }

    /// The base repository certs need no seed; the fixture is the subject.
    pub async fn seeded(indexing: Indexing) -> Self {
        Self::new(indexing).await
    }

    /// A fixture with the repository-authorization seed already written.
    pub async fn seeded_repo_auth(indexing: Indexing) -> Self {
        let fixture = Self::new(indexing).await;
        meshql_core::testing::seed_repository_auth_data(&fixture.repo).await;
        fixture
    }

    pub async fn cleanup(self) {
        let _ = meshql_dynamo::drop_table(&self.client, &self.table).await;
    }
}

/// [`RepoFixture`] pre-seeded for the repository-authorization certs, so
/// [`cert_case`] can drive them too.
pub struct RepoAuthFixture(RepoFixture);

impl RepoAuthFixture {
    pub async fn seeded(indexing: Indexing) -> Self {
        Self(RepoFixture::seeded_repo_auth(indexing).await)
    }

    pub fn subject(&self) -> &DynamoRepository {
        &self.0.repo
    }

    pub async fn cleanup(self) {
        self.0.cleanup().await
    }
}

pub struct SearcherFixture {
    pub client: Client,
    pub table: String,
    pub repo: DynamoRepository,
    pub searcher: DynamoSearcher,
}

impl SearcherFixture {
    /// The searcher has no write path, so the seed goes in through the
    /// repository — which, when indexed, is the half that writes the promoted
    /// attributes the searcher then reads. A fixture that seeded through an
    /// unindexed repository would leave every index empty and every indexed
    /// cert passing vacuously.
    pub async fn new(indexing: Indexing) -> Self {
        let table = fresh_table_name();
        let (client, repo, searcher) = collection(&table, indexing).await;
        Self {
            client,
            table,
            repo,
            searcher,
        }
    }

    pub fn subject(&self) -> &DynamoSearcher {
        &self.searcher
    }

    /// Every searcher seed, matching `meshql-sqlite/tests/searcher_cert.rs`.
    pub async fn seeded(indexing: Indexing) -> Self {
        let fixture = Self::new(indexing).await;
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

//! One table, one configuration, one index plan — a repository and a searcher
//! that cannot disagree about what is indexed.
//!
//! # The mistake this type removes
//!
//! An indexed table has two halves. The searcher reads an index; the repository
//! writes the promoted attribute the index is built on. If the repository is
//! built without the plan, every write lands with no `ix_` attribute, the index
//! stays empty, and every search that uses it returns **nothing** — no error, no
//! slow query, just an empty list where records ought to be. That is a worse
//! failure than the scan indexing replaced: a scan is merely expensive.
//!
//! It is also an easy mistake, because in a real `lib.rs` the repository and the
//! searcher are built ten lines apart, per entity, four or five entities deep.
//!
//! So this type takes the `RootConfig` once and builds both from it:
//!
//! ```no_run
//! # async fn example() -> meshql_core::Result<()> {
//! # let coop_config = meshql_core::RootConfig::builder()
//! #     .singleton("getCoop", r#"{"id": "{{id}}"}"#)
//! #     .vector("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)
//! #     .build();
//! use meshql_dynamo::DynamoCollection;
//!
//! let coops = DynamoCollection::open(None, "coops", &coop_config).await?;
//! // `coops.repository` promotes exactly what `coops.searcher` indexes,
//! // because there is one plan and it came from `coop_config`.
//! # Ok(())
//! # }
//! ```
//!
//! Disagreement is still *detected* if a caller builds the halves separately —
//! [`crate::store::ensure_indexed_table`] refuses a table whose indexes do not
//! match the handle's plan — but detection is the second line of defence.
//! Construction is the first.

use std::sync::Arc;

use aws_sdk_dynamodb::Client;
use meshql_core::{Repository, Result, RootConfig, Searcher};

use crate::index::IndexPlan;
use crate::metering::CapacityMeter;
use crate::{store, DynamoRepository, DynamoSearcher};

/// A `Repository` and a `Searcher` over one DynamoDB table, sharing one client
/// and one [`IndexPlan`].
pub struct DynamoCollection {
    pub repository: DynamoRepository,
    pub searcher: DynamoSearcher,
    plan: IndexPlan,
}

impl DynamoCollection {
    /// Open `table`, provisioning the indexes `config`'s query templates imply.
    ///
    /// `endpoint: None` → real AWS from the ambient config; `Some(url)` →
    /// DynamoDB Local.
    ///
    /// Fails at startup, rather than degrading to a scan, when a template names
    /// a field that cannot be indexed or when the derived set exceeds
    /// DynamoDB's 20-index limit. See [`crate::index`].
    pub async fn open(endpoint: Option<&str>, table: &str, config: &RootConfig) -> Result<Self> {
        let client = store::make_client(endpoint).await;
        Self::open_with_client(client, table, config).await
    }

    /// [`Self::open`] over a client you already have — a VPC endpoint, say, or
    /// one shared with another collection.
    pub async fn open_with_client(
        client: Client,
        table: &str,
        config: &RootConfig,
    ) -> Result<Self> {
        Self::with_plan(client, table, IndexPlan::derive(config)?).await
    }

    /// Open `table` for several configurations over one collection — the merged
    /// index set of all of them.
    pub async fn open_for_all<'a, I>(
        endpoint: Option<&str>,
        table: &str,
        configs: I,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = &'a RootConfig>,
    {
        let plan = IndexPlan::derive_all(configs)?;
        let client = store::make_client(endpoint).await;
        Self::with_plan(client, table, plan).await
    }

    /// Open over an explicit plan. Prefer [`Self::open`]; a hand-written plan is
    /// the thing that drifts.
    pub async fn with_plan(client: Client, table: &str, plan: IndexPlan) -> Result<Self> {
        // The repository first: it is the half that provisions nothing a
        // searcher would not, and if provisioning is going to fail it should
        // fail before a second handle has opened the table.
        let repository = DynamoRepository::with_plan(client.clone(), table, plan.clone()).await?;
        let searcher = DynamoSearcher::with_plan(client, table, plan.clone()).await?;
        Ok(Self {
            repository,
            searcher,
            plan,
        })
    }

    /// Account both halves against one meter, so a report covers the whole
    /// collection's bill. See [`crate::metering`].
    pub fn with_meter(self, meter: Arc<CapacityMeter>) -> Self {
        Self {
            repository: self.repository.with_meter(Arc::clone(&meter)),
            searcher: self.searcher.with_meter(meter),
            plan: self.plan,
        }
    }

    /// Split the `Scan` paths — `getAll` and `list` — across `segments`
    /// concurrent workers. Latency only, and only worth it on a large export;
    /// see [`DynamoSearcher::with_scan_segments`].
    pub fn with_scan_segments(self, segments: i32) -> Self {
        Self {
            repository: self.repository,
            searcher: self.searcher.with_scan_segments(segments),
            plan: self.plan,
        }
    }

    /// The indexes this collection provisioned.
    pub fn plan(&self) -> &IndexPlan {
        &self.plan
    }

    /// The two halves, as the trait objects `ServerConfig` wants.
    pub fn into_arcs(self) -> (Arc<dyn Repository>, Arc<dyn Searcher>) {
        (Arc::new(self.repository), Arc::new(self.searcher))
    }

    /// The two halves, concrete.
    pub fn split(self) -> (DynamoRepository, DynamoSearcher) {
        (self.repository, self.searcher)
    }
}

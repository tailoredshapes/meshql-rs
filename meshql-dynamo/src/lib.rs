//! meshql-dynamo — `Repository` + `Searcher` over Amazon DynamoDB.
//!
//! One table per collection, keyed `(pk = envelope id, sk = created_at nanos +
//! uuid)`. The sort key is what does the work: because it is zero-padded to a
//! fixed width, lexicographic order *is* temporal order, so "the latest version
//! of this id at-or-before this instant" is one `query` with
//! `scan_index_forward(false).limit(1)` — a single round trip, which is better
//! than any of the SQL adapters manage.
//!
//! What that key model cannot do on its own is answer an *arbitrary-attribute*
//! predicate. Latest-version-per-id plus a temporal cutoff plus equality on a
//! payload field nobody declared in advance has no expression in DynamoDB's key
//! model, so an unindexed non-`id` search is a full table `scan` with the
//! version resolution and the predicate applied in Rust — `O(every version ever
//! written)`, which at a million versions is a **45-second** request and
//! $0.0156 a call. See `searcher.rs` for why the predicate cannot be pushed into
//! a `FilterExpression` even as an optimisation.
//!
//! # Nobody declares an arbitrary-attribute predicate
//!
//! meshql query templates are fixed strings in source-controlled configuration,
//! so the complete set of fields a deployment will ever filter on is derivable
//! from the same `RootConfig` that generates the queries — before the first
//! request, and with no possibility of drifting away from them. Hand that config
//! to a [`DynamoCollection`] and the indexes follow from it:
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
//! # Ok(())
//! # }
//! ```
//!
//! `getCoopsByFarm` now costs **6 read units** where it cost **122,254** — the
//! foreign-key search stops being a function of table size at all. What it does
//! not fix is `getAll` / `list`, which is irreducible and is [`index`]'s and
//! `docs/cost-model-dynamodb.md` §8's subject.
//!
//! The unindexed constructors remain, and remain honest about what they are:
//!
//! ```no_run
//! # async fn example() -> meshql_core::Result<()> {
//! use meshql_dynamo::{DynamoRepository, DynamoSearcher};
//!
//! // Real AWS, from the ambient config. Every non-`id` search is a Scan.
//! let repo = DynamoRepository::new(None, "farms").await?;
//! // DynamoDB Local.
//! let searcher = DynamoSearcher::new(Some("http://localhost:8123"), "farms").await?;
//! # Ok(())
//! # }
//! ```
//!
//! What any of it costs is not a matter of opinion: [`metering`] is an opt-in
//! capacity meter that reports what DynamoDB actually billed, and
//! `docs/cost-model-dynamodb.md` is the model it validated.

pub mod collection;
pub mod convert;
pub mod index;
pub mod matcher;
pub mod metering;
pub mod repository;
pub mod searcher;
pub mod store;

pub use collection::DynamoCollection;
pub use index::{IndexPlan, MAX_GLOBAL_SECONDARY_INDEXES};
pub use metering::{CapacityMeter, CapacityReport, Op, OpStats, Rates};
pub use repository::DynamoRepository;
pub use searcher::DynamoSearcher;
pub use store::{drop_table, ensure_indexed_table, ensure_table, make_client, migrate_indexes};

//! meshql-dynamo — `Repository` + `Searcher` over Amazon DynamoDB.
//!
//! One table per collection, keyed `(pk = envelope id, sk = created_at nanos +
//! uuid)`. The sort key is what does the work: because it is zero-padded to a
//! fixed width, lexicographic order *is* temporal order, so "the latest version
//! of this id at-or-before this instant" is one `query` with
//! `scan_index_forward(false).limit(1)` — a single round trip, which is better
//! than any of the SQL adapters manage.
//!
//! What that key model cannot do is answer an *arbitrary-attribute* predicate.
//! Latest-version-per-id plus a temporal cutoff plus equality on a payload field
//! nobody declared in advance has no expression in DynamoDB's key model, so
//! every non-`id` search is a full table `scan` with the version resolution and
//! the predicate applied in Rust. See `searcher.rs` for why the predicate cannot
//! be pushed into a `FilterExpression` even as an optimisation, and the crate's
//! notes on GSIs for what a fix would look like.
//!
//! ```no_run
//! # async fn example() -> meshql_core::Result<()> {
//! use meshql_dynamo::{DynamoRepository, DynamoSearcher};
//!
//! // Real AWS, from the ambient config.
//! let repo = DynamoRepository::new(None, "farms").await?;
//! // DynamoDB Local.
//! let searcher = DynamoSearcher::new(Some("http://localhost:8123"), "farms").await?;
//! # Ok(())
//! # }
//! ```

pub mod convert;
pub mod matcher;
pub mod repository;
pub mod searcher;
pub mod store;

pub use repository::DynamoRepository;
pub use searcher::DynamoSearcher;
pub use store::{drop_table, ensure_table, make_client};

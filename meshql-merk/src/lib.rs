//! # meshql-merk
//!
//! A **create-only** `Repository` over [`merk-cloud`](https://github.com/tailoredshapes/merk-cloud):
//! an event log whose partitions are objects in an S3 Express One Zone
//! directory bucket, appended with compare-and-swap on the tail.
//!
//! ## Why this crate exists rather than reusing `meshql-merkql`
//!
//! `merk-cloud`'s own README says the migration from merkql is a dependency
//! rename, and that is true of the *log* surface. It is not true of the meshql
//! adapter, because `meshql-merkql`'s value is a `Searcher` this crate must
//! forbid.
//!
//! `MerkqlSearcher::scan_latest` opens a fresh consumer group with a new UUID
//! id, `OffsetReset::Earliest`, and polls until the batch is empty — a full scan
//! of the entire topic from offset zero **on every search call**. So does
//! `MerkqlRepository::read_all_envelopes`, which backs both `read` and `list`.
//! On merkql's local filesystem that is a page-cached disk read and merely
//! wasteful. On object storage it is a `head`, sometimes a `list`, and then a
//! ranged read of every segment: **one GraphQL query downloads the whole log**,
//! at a cost that grows without bound as the log grows.
//!
//! ## What this crate does instead
//!
//! Reads are not slow here. They are **structurally absent**.
//!
//! [`MerkRepository`] holds an [`AppendOnlyLog`], which holds a
//! `merk_object::producer::Producer`. A `Producer`'s entire public surface is
//! `send` and `send_batch`. It exposes no broker, no topic, no partition and no
//! consumer, so there is no path from this crate's types to anything that can
//! read the log. `read`, `list`, `read_many`, `remove` and `remove_many` return
//! an explicit error, and a future implementer cannot quietly turn one of them
//! into a scan without first adding a field that a reviewer can see. That is the
//! point: a comment saying "do not scan" is one careless commit from being
//! false, and a slow-but-correct read path is how this becomes production
//! behaviour by accident.
//!
//! There is deliberately **no `Searcher` implementation in this crate at all.**
//! Projections are read from an indexed store (`meshql-dynamo`); the log is
//! written to and consumed by offset range, which is what a log is for.
//!
//! ## What else is here
//!
//! - [`provision`] — explicit `create_topic` calls with partition counts read
//!   from a `topics.toml`, because `BrokerConfig::new()` defaults to
//!   `default_partitions: 1` with `auto_create_topics: true`, so the first
//!   `send` to an unprovisioned topic silently creates it with one partition and
//!   nobody chose that. Partition count is immutable.
//! - [`consumer`] — a consumer wrapper that cannot be configured wrongly. See
//!   its module docs for the two defects it exists to prevent.
//! - [`notify`] — the producer-side wake-up message, which is the only way a
//!   consumer on AWS ever learns anything happened, because directory buckets
//!   emit no S3 Event Notifications.

pub mod consumer;
pub mod conversion;
pub mod log;
pub mod notify;
pub mod provision;
pub mod repository;

pub use consumer::{group_id_for, SafeConsumer};
pub use log::AppendOnlyLog;
pub use notify::Notification;
pub use provision::{provision, TopicPlan, TopicSpec};
pub use repository::MerkRepository;

/// The certified surface bound to S3 Express One Zone.
pub mod aws {
    use merk_aws::S3Backend;

    pub type AppendOnlyLog = crate::log::AppendOnlyLog<S3Backend>;
    pub type MerkRepository = crate::repository::MerkRepository<S3Backend>;
    pub type SafeConsumer = crate::consumer::SafeConsumer<S3Backend>;
    pub use merk_aws::broker::{Broker, BrokerConfig, BrokerRef};
}

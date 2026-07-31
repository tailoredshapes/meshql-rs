//! `merkql-connect` — change-data-capture from a meshql envelope store onto a
//! merkql topic.
//!
//! # This is a separate deployable, not a library a service links in
//!
//! `merkql-connect` is a **standalone process**, deployed next to the database
//! the way Debezium is. It is not part of `meshql-server`, does not depend on
//! `meshql-restlette`, and is never wired up by a product's `main.rs`. A
//! deployment runs its meshql service and, separately, runs this connector
//! pointed at the same store:
//!
//! ```text
//! meshql service (restlettes) ──writes──> Postgres / Mongo / SQLite
//!                                                │
//!                                                │ CDC (this process)
//!                                                ▼
//!                                         merkql-connect ── sole writer ──> merkql store
//!                                                                                │
//!                                                                 many reader processes
//!                                                                                ▼
//!                                                           workers ──> projections (via restlette)
//! ```
//!
//! ## Why the separate process is the *right* shape, not merely Debezium cosplay
//!
//! merkql is **single-writer per process**: `Partition::next_offset` is
//! in-memory and advanced only by in-process appends, so two writer processes
//! silently destroy each other's records. Running the CDC as its own
//! deployable *resolves* that constraint instead of fighting it. The meshql
//! service writes to Postgres/Mongo/SQLite and never touches merkql at all;
//! this connector is the only process that appends; workers are readers, which
//! merkql supports without limit. One writer, many readers — exactly the
//! topology merkql is built for.
//!
//! [`TopicWriter`] makes that structural rather than conventional: claiming a
//! topic takes an exclusive advisory lock on a file next to the offset store,
//! so a second connector aimed at the same topic fails to start instead of
//! quietly corrupting the log.
//!
//! # Why CDC and not a `post_create` hook
//!
//! - **No dual write.** The application writes exactly once — to the store.
//!   The event is *derived* from the committed write, so there is no window in
//!   which the row exists but the event was lost. A restlette `post_create`
//!   hook is a dual write with no shared transaction: crash between the DB
//!   commit and the publish and the event never fires, the log is a lie, and
//!   every projection folded from it is silently wrong. CDC guarantees
//!   at-least-once delivery *after* commit, and that guarantee belongs to the
//!   datastore/CDC layer, not to the mesh.
//! - **Correct order and time.** Records carry the store's commit order and
//!   the store's timestamp, not the application's wall-clock during a request,
//!   so replay and temporal reads agree with what actually happened.
//!
//! # The contract every source honours
//!
//! - **At-least-once after commit.** Duplicates are acceptable — folds are
//!   idempotent over a whole log — and **silent gaps are not**. Every design
//!   decision below resolves in favour of re-delivering rather than skipping.
//! - **Snapshot-then-stream** (Debezium's model). On a cold start, or when a
//!   stored position is no longer usable and the configured snapshot mode
//!   allows it, the connector captures the streaming position *first*, emits
//!   the rows that already exist as `op: r`, then switches to streaming from
//!   the captured position. A new consumer therefore gets full history with no
//!   gap, and "materialise a projection from the beginning of time" is an
//!   ordinary operation rather than a special case.
//! - **An unusable position is never a silent skip.** See [`SnapshotMode`] for
//!   the policy — it either re-snapshots or refuses to start.
//! - **Positions are committed periodically, after the records they cover.**
//!   A crash between an append and an offset commit replays; it never skips.
//!
//! # The record on the topic
//!
//! [`ChangeRecord`] is Debezium's change-event envelope: `before`, `after`,
//! `source`, `op`, `ts_ms`. meshql envelopes are append-only, so `before` is
//! always `null` and `op` is `r` (snapshot read) or `c` (create); `u` and `d`
//! are carried by the type but never produced by these sources. The `source`
//! block carries the **native** position — a SQLite rowid, a Mongo resume
//! token, a Postgres LSN — so a consumer can dedupe and reason about ordering
//! against the store's own numbering rather than trusting ours.
//!
//! **merkql's producer key is hardcoded to the Envelope id** (see
//! `meshql-merkql/src/repository.rs`), and this connector keys the same way
//! deliberately, so a CDC topic and a merkql-as-primary-store topic partition
//! identically. Partitioning is therefore by *envelope id*, not by aggregate.
//! Use `num_partitions = 1` and get total order per topic; see the
//! `meshql-patterns` skill's `domain-design.md`.
//!
//! # Not for merkql-as-primary-store
//!
//! If an event mesh's `Repository` is already `meshql-merkql`'s
//! `MerkqlRepository`, the restlette `POST` *is* the topic append and there is
//! nothing to bridge. This connector exists for the other topology, where the
//! store is a real database.

pub mod config;
pub mod offsets;
pub mod record;
pub mod sink;
pub mod source;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "mongo")]
pub mod mongo;

#[cfg(feature = "postgres")]
pub mod postgres;

pub mod cert;

pub use config::{ConnectorConfig, SourceConfig};
pub use offsets::OffsetStore;
pub use record::{ChangeRecord, Op, Snapshot, SourceInfo};
pub use sink::{run_connector, TopicWriter};
pub use source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};

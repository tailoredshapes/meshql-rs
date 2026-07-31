//! The queue side: the sink seam, the merkql single-writer guard, and the
//! connector loop.
//!
//! # Why the sink is a trait and not `merkql`
//!
//! "The queue" is not merkql. It is **whichever persistent queue a deployment
//! configured** — merkql for development and early growth, Kafka (via
//! `meshql-ksql`) in production, merk-cloud on AWS, and a PostgreSQL-backed
//! queue for the medium/large tier. A connector that named merkql in its
//! append path would have to be rewritten once per queue, which is exactly the
//! coupling meshql's adapter guarantee exists to prevent.
//!
//! So the connector appends through [`TopicSink`], and there are two shapes of
//! implementation:
//!
//! - [`TopicWriter`] — merkql direct, keeping the `flock` single-writer guard
//!   and the single-partition check that merkql specifically requires. It puts
//!   the **whole [`ChangeRecord`]** on the topic, preserving today's wire
//!   format exactly.
//! - [`RepositorySink`] — any `meshql_core::Repository`, which is the seam
//!   every other queue already implements. Kafka, merk-cloud, Postgres and
//!   DynamoDB arrive here for free, and a new queue backend is a config change
//!   rather than a connector change.
//!
//! ## The one difference between them, and it is load-bearing
//!
//! `Repository::create` takes an `Envelope`, not a `ChangeRecord`. So a
//! `RepositorySink` appends **`record.after` — the envelope alone** — and the
//! Debezium `source` block (native position, snapshot flag, connector name)
//! stays connector-local rather than riding on the topic.
//!
//! That is the right trade for the reason the ingress connectors exist: an
//! ingress connector **synthesises** the envelope from a foreign record, so
//! everything the domain needs is already inside the payload it built. It is
//! *not* free for the database CDC sources, where a consumer that wants to
//! distinguish backfill (`op: r`) from live traffic (`op: c`) can only do so
//! on a merkql sink. A source that needs that distinction downstream must
//! materialise it into the envelope payload rather than relying on the
//! Debezium block surviving the append.

use crate::offsets::OffsetStore;
use crate::record::ChangeRecord;
use crate::source::{CdcError, CommitSource, Resume, SnapshotMode};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use merkql::broker::{Broker, BrokerRef};
use merkql::record::ProducerRecord;
use meshql_core::Repository;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One configured queue, appended to.
///
/// The connector loop knows nothing about which queue it is feeding. See the
/// module docs for why this is a trait and what the two implementations differ
/// on.
#[async_trait]
pub trait TopicSink: Send + Sync {
    /// Append one change record. Must not return until the record is durable
    /// on the queue, because the caller commits the source position
    /// immediately afterwards and a premature return turns a crash into a
    /// permanent gap.
    async fn append(&self, record: &ChangeRecord) -> Result<()>;

    /// The topic being written, for log lines and errors.
    fn topic(&self) -> &str;

    /// Which queue backend this is — `"merkql"`, `"repository"`. Logged at
    /// startup so an operator can see which sink a config actually selected.
    fn backend(&self) -> &'static str;
}

/// Exclusive write access to one merkql topic.
///
/// # The constraint this makes structural
///
/// merkql is **single-writer per process**. `Partition::next_offset` is an
/// in-memory counter advanced only by in-process appends, so two writer
/// processes each believe they own the next offset and silently overwrite each
/// other's records. Nothing in merkql detects this; the log simply ends up
/// missing records, which is indistinguishable downstream from "those writes
/// never happened."
///
/// A comment saying "don't run two of these" has not been enough — this has
/// been rediscovered repeatedly. So claiming a topic takes an **exclusive
/// advisory lock (`flock`) on a lock file** beside the offset store, held for
/// the writer's lifetime. A second connector aimed at the same topic and state
/// directory fails at startup with a clear error rather than corrupting the
/// log. The lock is per open file description, so this catches a second
/// process *and* a second writer inside one process.
///
/// The only way to append through this module is [`TopicWriter::append`], and
/// the only way to get a `TopicWriter` is [`TopicWriter::claim`]. There is no
/// free function that produces to a topic.
pub struct TopicWriter {
    topic: String,
    broker: BrokerRef,
    /// Held for the lifetime of the writer. Dropping the file releases the
    /// advisory lock, so this field is load-bearing despite being unread.
    _lock: std::fs::File,
    lock_path: PathBuf,
}

impl TopicWriter {
    /// Claim exclusive write access to `topic`.
    ///
    /// `state_dir` is the connector's own directory — the same one holding the
    /// offset file — so the lock lives with the state it protects rather than
    /// inside the merkql store, which merkql itself manages.
    pub fn claim(broker: BrokerRef, topic: &str, state_dir: &Path) -> Result<Self> {
        use fs2::FileExt;

        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating connector state dir {}", state_dir.display()))?;
        let lock_path = state_dir.join(format!("{topic}.writer.lock"));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening writer lock {}", lock_path.display()))?;

        if lock.try_lock_exclusive().is_err() {
            bail!(
                "another merkql-connect writer already holds {}: merkql is single-writer \
                 per process, so a second writer on topic '{topic}' would silently destroy \
                 records. Stop the other connector, or point this one at a different topic.",
                lock_path.display()
            );
        }

        // One partition, always. merkql routes by hashing the producer key,
        // and the key is the Envelope id — unique per record — so raising the
        // partition count scatters one aggregate's records across partitions
        // with no ordering between them, which is strictly worse than one
        // partition's total order. `create_topic` is idempotent.
        broker
            .create_topic(topic, 1)
            .with_context(|| format!("creating merkql topic '{topic}'"))?;

        let partitions = broker
            .topic(topic)
            .map(|t| t.num_partitions())
            .ok_or_else(|| anyhow::anyhow!("merkql topic '{topic}' missing after create_topic"))?;
        if partitions != 1 {
            // `create_topic` returns early on an existing topic WITHOUT
            // checking its partition count, so a topic provisioned elsewhere
            // with more partitions reaches here looking healthy.
            bail!(
                "merkql topic '{topic}' has {partitions} partitions; merkql-connect requires \
                 a single-partition topic, because the producer key is the Envelope id and \
                 multi-partition routing would scatter an aggregate's records with no \
                 ordering between them"
            );
        }

        Ok(Self {
            topic: topic.to_string(),
            broker,
            _lock: lock,
            lock_path,
        })
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Append one change record. Returns the merkql offset it landed on.
    pub fn append(&self, record: &ChangeRecord) -> Result<u64> {
        let value = serde_json::to_string(record).context("serializing change record")?;
        let produced = Broker::producer(&self.broker)
            .send(&ProducerRecord::new(&self.topic, record.key(), value))
            .map_err(|e| anyhow::anyhow!("appending to merkql topic '{}': {e}", self.topic))?;
        Ok(produced.offset)
    }
}

#[async_trait]
impl TopicSink for TopicWriter {
    /// Puts the **whole** `ChangeRecord` on the topic — Debezium block and
    /// all — which is the wire format this connector has always produced.
    async fn append(&self, record: &ChangeRecord) -> Result<()> {
        TopicWriter::append(self, record).map(|_offset| ())
    }

    fn topic(&self) -> &str {
        &self.topic
    }

    fn backend(&self) -> &'static str {
        "merkql"
    }
}

/// Any `meshql_core::Repository` as a queue.
///
/// This is how every non-merkql queue is reached: Kafka via
/// `meshql_ksql::KsqlRepository`, merk-cloud via `meshql_merk::MerkRepository`,
/// Postgres via `meshql_postgres::PostgresRepository`. The connector binary
/// does not depend on any of them — the caller constructs the repository and
/// hands it over, so adding a queue backend never touches this file.
///
/// # What lands on the topic
///
/// `record.after` — the envelope — and nothing else. See the module docs for
/// why, and for what a source must do if it needs the Debezium metadata
/// downstream.
///
/// A record with no `after` is a Debezium delete, which meshql's append-only
/// sources never emit (a deletion is a new envelope with `deleted: true`). If
/// one ever arrives it is an error rather than a silent drop, because a
/// dropped record is the gap this whole crate is built to prevent.
pub struct RepositorySink {
    topic: String,
    repository: Arc<dyn Repository>,
}

impl RepositorySink {
    pub fn new(topic: impl Into<String>, repository: Arc<dyn Repository>) -> Self {
        Self {
            topic: topic.into(),
            repository,
        }
    }
}

#[async_trait]
impl TopicSink for RepositorySink {
    async fn append(&self, record: &ChangeRecord) -> Result<()> {
        let envelope = record.after.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "change record for topic '{}' has no `after` envelope; a Repository sink has \
                 nothing to append. meshql sources are append-only and never emit a Debezium \
                 delete, so this is a source bug — failing rather than dropping the record.",
                self.topic
            )
        })?;

        // The envelope's own tokens are the authority. Passing them back as
        // the caller credentials is what a restlette POST does, and it keeps
        // an adapter that filters on write from rejecting the connector's
        // append.
        let tokens = envelope.authorized_tokens.clone();
        self.repository
            .create(envelope.clone(), &tokens)
            .await
            .with_context(|| format!("appending to queue topic '{}'", self.topic))?;
        Ok(())
    }

    fn topic(&self) -> &str {
        &self.topic
    }

    fn backend(&self) -> &'static str {
        "repository"
    }
}

/// Run one source into one topic, forever.
///
/// The loop's whole job is the ordering rule: **append, then commit the
/// position.** Everything else is policy about how to start.
pub async fn run_connector(
    source: &dyn CommitSource,
    writer: &dyn TopicSink,
    offsets: &mut OffsetStore,
    mode: SnapshotMode,
) -> Result<()> {
    let mut stream = open_stream(source, offsets, mode).await?;

    while let Some(item) = stream.next().await {
        let record = match item {
            Ok(r) => r,
            // A position that goes bad mid-stream (a slot dropped under us, an
            // oplog roll-over during a stall) gets the same policy as one that
            // was bad at startup — never a silent restart from elsewhere.
            Err(CdcError::UnusablePosition {
                connector,
                position,
                reason,
            }) => {
                if !mode.recovers_from_unusable_position() {
                    bail!(
                        "{connector}: position {position:?} became unusable ({reason}) and \
                         snapshot_mode is not `when_needed`, so recovering would mean silently \
                         choosing a new start point. Refusing. Set snapshot_mode = \
                         \"when_needed\" to re-snapshot instead."
                    );
                }
                eprintln!(
                    "[merkql-connect {connector}] position {position:?} unusable ({reason}); \
                     re-snapshotting per snapshot_mode = when_needed"
                );
                stream = source.changes(Resume::Cold, mode).await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        // 1. Append first. If this fails the position is not committed, so the
        //    record is re-delivered on the next start.
        writer.append(&record).await?;

        // 2. Only then stage the position, and commit it periodically.
        if let Some(position) = record.position() {
            offsets.stage(position, record.source.snapshot.in_progress());
            // 3. And only once a commit has actually hit the disk may the
            //    source be told to let go of what it was holding. Doing this
            //    on `stage` rather than on a real commit would hand a
            //    PostgreSQL slot permission to recycle WAL for records that
            //    are not durable anywhere.
            if let Some(committed) = offsets.maybe_commit()? {
                source.durable_through(&committed).await?;
            }
        }
    }

    // The stream ended: commit whatever is staged so a restart resumes from
    // the last record actually appended rather than replaying the interval.
    if let Some(committed) = offsets.commit_now()? {
        source.durable_through(&committed).await?;
    }
    Ok(())
}

/// Open the feed, applying the snapshot / unusable-position policy.
async fn open_stream(
    source: &dyn CommitSource,
    offsets: &OffsetStore,
    mode: SnapshotMode,
) -> Result<crate::ChangeStream> {
    let from = offsets.resume();
    match source.changes(from.clone(), mode).await {
        Ok(stream) => Ok(stream),
        Err(CdcError::UnusablePosition {
            connector,
            position,
            reason,
        }) => {
            if !mode.recovers_from_unusable_position() {
                bail!(
                    "{connector}: stored position {position:?} is unusable ({reason}). \
                     snapshot_mode is not `when_needed`, and starting from anywhere else \
                     would silently skip every record between {position:?} and the new \
                     start point. Refusing to start. Set snapshot_mode = \"when_needed\" \
                     to re-snapshot from history instead."
                );
            }
            eprintln!(
                "[merkql-connect {connector}] stored position {position:?} unusable \
                 ({reason}); re-snapshotting per snapshot_mode = when_needed"
            );
            Ok(source.changes(Resume::Cold, mode).await?)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Op, Snapshot, SourceInfo};
    use crate::source::ChangeStream;
    use async_trait::async_trait;
    use merkql::broker::BrokerConfig;
    use merkql::consumer::{ConsumerConfig, OffsetReset};
    use meshql_core::Envelope;
    use std::sync::Arc;
    use std::time::Duration;

    fn broker(dir: &Path) -> BrokerRef {
        Broker::open(BrokerConfig::new(dir.join("merkql"))).unwrap()
    }

    fn record(id: &str, position: &str, snapshot: Snapshot) -> ChangeRecord {
        ChangeRecord::new(
            if snapshot.is_snapshot() {
                Op::Read
            } else {
                Op::Create
            },
            Envelope::new(id, meshql_core::Stash::new(), vec![]),
            SourceInfo {
                connector: "test".into(),
                entity: "envelopes".into(),
                ts_ms: 1,
                position: Some(position.into()),
                snapshot,
            },
        )
    }

    fn read_topic(broker: &BrokerRef, topic: &str) -> Vec<ChangeRecord> {
        let mut consumer = Broker::consumer(
            broker,
            ConsumerConfig {
                group_id: uuid::Uuid::new_v4().to_string(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&[topic]).unwrap();
        consumer
            .poll(Duration::from_millis(0))
            .unwrap()
            .iter()
            .map(|r| serde_json::from_str(&r.value).unwrap())
            .collect()
    }

    // ── The single-writer guard ─────────────────────────────────────────

    #[test]
    fn a_second_writer_on_the_same_topic_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let first = TopicWriter::claim(broker.clone(), "hen", dir.path())
            .expect("the first writer claims the topic");

        let err = match TopicWriter::claim(broker.clone(), "hen", dir.path()) {
            Ok(_) => panic!("a second writer must be refused — merkql is single-writer"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("single-writer"),
            "the error must name the constraint, got: {err}"
        );

        // And the guard releases, so a restart works. Without this the
        // assertion above would also pass for a writer that can never be
        // claimed twice in a process lifetime, even sequentially.
        drop(first);
        TopicWriter::claim(broker, "hen", dir.path())
            .expect("the lock must release on drop so a restart can claim it");
    }

    /// The lock is per *open file description*, which is why it catches a
    /// second process and not merely a second `TopicWriter` value in this one.
    /// Asserted by locking the same path through an independent handle — the
    /// exact thing a second process does.
    #[test]
    fn the_writer_lock_is_held_against_an_independent_file_handle() {
        use fs2::FileExt;

        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let writer = TopicWriter::claim(broker, "hen", dir.path()).unwrap();

        let other = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(writer.lock_path())
            .unwrap();
        assert!(
            other.try_lock_exclusive().is_err(),
            "an independent handle on the lock file must be blocked — otherwise the \
             guard does not survive being run as a second process"
        );
    }

    #[test]
    fn a_multi_partition_topic_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        broker.create_topic("hen", 3).unwrap();

        let err = match TopicWriter::claim(broker, "hen", dir.path()) {
            Ok(_) => panic!("a multi-partition topic must be refused"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("single-partition"), "got: {err}");
    }

    // ── The connector loop ──────────────────────────────────────────────

    /// A scripted source: yields `records`, then ends.
    struct Scripted {
        records: Vec<Result<ChangeRecord, ()>>,
        cold_only: bool,
    }

    #[async_trait]
    impl CommitSource for Scripted {
        fn connector(&self) -> &'static str {
            "test"
        }
        fn entity(&self) -> &str {
            "envelopes"
        }
        async fn changes(&self, from: Resume, _m: SnapshotMode) -> Result<ChangeStream, CdcError> {
            if self.cold_only {
                if let Resume::At(position) = from {
                    return Err(CdcError::UnusablePosition {
                        connector: "test",
                        position,
                        reason: "this source only accepts a cold start".into(),
                    });
                }
            }
            let items: Vec<Result<ChangeRecord, CdcError>> = self
                .records
                .iter()
                .map(|r| match r {
                    Ok(rec) => Ok(rec.clone()),
                    Err(()) => Err(CdcError::UnusablePosition {
                        connector: "test",
                        position: "gone".into(),
                        reason: "scripted".into(),
                    }),
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(items)))
        }
    }

    fn offsets(dir: &Path) -> OffsetStore {
        OffsetStore::open(
            dir.join("offsets.json"),
            "test",
            "envelopes",
            Duration::from_millis(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn records_land_on_the_topic_and_the_position_is_committed() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let writer = TopicWriter::claim(broker.clone(), "hen", dir.path()).unwrap();
        let mut store = offsets(dir.path());

        let source = Scripted {
            records: vec![
                Ok(record("a", "1", Snapshot::False)),
                Ok(record("b", "2", Snapshot::False)),
            ],
            cold_only: false,
        };
        run_connector(&source, &writer, &mut store, SnapshotMode::Initial)
            .await
            .unwrap();

        let on_topic = read_topic(&broker, "hen");
        assert_eq!(
            on_topic
                .iter()
                .map(|r| r.key().unwrap())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(store.resume(), Resume::At("2".into()));
    }

    /// A snapshot-in-progress position must not be resumable as a streaming
    /// position, even after a clean shutdown mid-snapshot.
    /// An interrupted snapshot offers its position back as `Snapshotting`, and
    /// never as `At`. A source decides what to do with it; the loop's job is
    /// only to keep the two kinds of position distinguishable.
    #[tokio::test]
    async fn an_interrupted_snapshot_resumes_as_snapshotting() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let writer = TopicWriter::claim(broker, "hen", dir.path()).unwrap();
        let mut store = offsets(dir.path());

        let source = Scripted {
            records: vec![Ok(record("a", "1", Snapshot::True))],
            cold_only: false,
        };
        run_connector(&source, &writer, &mut store, SnapshotMode::Initial)
            .await
            .unwrap();

        assert_eq!(store.resume(), Resume::Snapshotting("1".to_string()));
        // A source that cannot continue a partial snapshot still gets the
        // pre-existing restart, unchanged.
        assert_eq!(store.resume().without_snapshot_resume(), Resume::Cold);
    }

    /// `initial` and `never` must REFUSE TO START on an unusable position.
    /// Starting anyway from a fresh live position is the silent skip.
    #[tokio::test]
    async fn an_unusable_position_stops_a_connector_that_may_not_re_snapshot() {
        for mode in [SnapshotMode::Initial, SnapshotMode::Never] {
            let dir = tempfile::tempdir().unwrap();
            let broker = broker(dir.path());
            let writer = TopicWriter::claim(broker, "hen", dir.path()).unwrap();
            let mut store = offsets(dir.path());
            store.stage("99", false);
            store.commit_now().unwrap();

            let source = Scripted {
                records: vec![Ok(record("a", "1", Snapshot::False))],
                cold_only: true,
            };
            let err = run_connector(&source, &writer, &mut store, mode)
                .await
                .expect_err("must refuse to start rather than skip");
            assert!(
                err.to_string().contains("Refusing to start"),
                "mode {mode:?} got: {err}"
            );
        }
    }

    /// `when_needed` recovers by re-snapshotting — the whole reason the mode
    /// exists.
    #[tokio::test]
    async fn when_needed_recovers_an_unusable_position_by_re_snapshotting() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let writer = TopicWriter::claim(broker.clone(), "hen", dir.path()).unwrap();
        let mut store = offsets(dir.path());
        store.stage("99", false);
        store.commit_now().unwrap();

        let source = Scripted {
            records: vec![Ok(record("a", "1", Snapshot::True))],
            cold_only: true,
        };
        run_connector(&source, &writer, &mut store, SnapshotMode::WhenNeeded)
            .await
            .expect("when_needed must recover");

        assert_eq!(
            read_topic(&broker, "hen")
                .iter()
                .map(|r| r.key().unwrap())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
    }

    /// The append-then-commit ordering, observed from the outside: a source
    /// that fails *after* yielding a record must leave the position at the
    /// last record actually appended, never past it.
    #[tokio::test]
    async fn a_mid_stream_failure_leaves_the_position_no_further_than_the_last_append() {
        let dir = tempfile::tempdir().unwrap();
        let broker = broker(dir.path());
        let writer = TopicWriter::claim(broker.clone(), "hen", dir.path()).unwrap();
        let mut store = offsets(dir.path());

        let source = Scripted {
            records: vec![Ok(record("a", "1", Snapshot::False)), Err(())],
            cold_only: false,
        };
        // `initial` cannot recover, so the error propagates.
        assert!(
            run_connector(&source, &writer, &mut store, SnapshotMode::Initial)
                .await
                .is_err()
        );

        // "a" was appended, so resuming from its position is correct and
        // loses nothing. Anything beyond it would skip.
        let appended = read_topic(&broker, "hen");
        assert_eq!(appended.len(), 1);
        let reopened = OffsetStore::open(
            dir.path().join("offsets.json"),
            "test",
            "envelopes",
            Duration::from_millis(0),
        )
        .unwrap();
        assert!(
            matches!(reopened.resume(), Resume::Cold | Resume::At(_)),
            "a durable position must never name a record that was not appended"
        );
        if let Resume::At(p) = reopened.resume() {
            assert_eq!(p, "1", "the position must not run ahead of the appends");
        }
    }

    #[tokio::test]
    async fn arc_sources_work_through_the_trait_object() {
        // The binary holds sources as trait objects; pin that this compiles.
        let source: Arc<dyn CommitSource> = Arc::new(Scripted {
            records: vec![],
            cold_only: false,
        });
        assert_eq!(source.connector(), "test");
    }

    /// A `Repository` that records what it was asked to append.
    struct RecordingRepo {
        seen: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl Repository for RecordingRepo {
        async fn create(
            &self,
            envelope: Envelope,
            tokens: &[String],
        ) -> meshql_core::Result<Envelope> {
            self.seen
                .lock()
                .unwrap()
                .push((envelope.id.clone(), tokens.to_vec()));
            Ok(envelope)
        }
        async fn read(
            &self,
            _: &str,
            _: &[String],
            _: Option<chrono::DateTime<chrono::Utc>>,
        ) -> meshql_core::Result<Option<Envelope>> {
            unreachable!("a sink never reads")
        }
        async fn list(&self, _: &[String]) -> meshql_core::Result<Vec<Envelope>> {
            unreachable!("a sink never reads")
        }
        async fn remove(&self, _: &str, _: &[String]) -> meshql_core::Result<bool> {
            unreachable!("a sink never removes")
        }
        async fn create_many(
            &self,
            _: Vec<Envelope>,
            _: &[String],
        ) -> meshql_core::Result<Vec<Envelope>> {
            unreachable!("the loop appends one at a time")
        }
        async fn read_many(
            &self,
            _: &[String],
            _: &[String],
        ) -> meshql_core::Result<Vec<Envelope>> {
            unreachable!("a sink never reads")
        }
        async fn remove_many(
            &self,
            _: &[String],
            _: &[String],
        ) -> meshql_core::Result<std::collections::HashMap<String, bool>> {
            unreachable!("a sink never removes")
        }
    }

    /// The whole point of the seam: the same connector loop drives a queue
    /// that is not merkql, with no merkql types involved.
    #[tokio::test]
    async fn a_repository_sink_appends_the_envelope_through_the_repository() {
        let repo = Arc::new(RecordingRepo {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let sink = RepositorySink::new("lay_report", Arc::clone(&repo) as Arc<dyn Repository>);

        assert_eq!(sink.backend(), "repository");
        assert_eq!(TopicSink::topic(&sink), "lay_report");

        sink.append(&record("hen-1", "1", Snapshot::False))
            .await
            .unwrap();

        let seen = repo.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "hen-1");
    }

    /// The envelope's own tokens are passed back as the caller credentials, so
    /// an adapter that filters on write does not reject the connector.
    #[tokio::test]
    async fn a_repository_sink_presents_the_envelopes_own_tokens() {
        let repo = Arc::new(RecordingRepo {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let sink = RepositorySink::new("lay_report", Arc::clone(&repo) as Arc<dyn Repository>);

        let mut rec = record("hen-1", "1", Snapshot::False);
        rec.after.as_mut().unwrap().authorized_tokens = vec!["farm-1".to_string()];
        sink.append(&rec).await.unwrap();

        assert_eq!(repo.seen.lock().unwrap()[0].1, vec!["farm-1".to_string()]);
    }

    /// A record with no `after` must be an error, never a silent drop — a
    /// dropped record is exactly the permanent gap this crate exists to
    /// prevent.
    #[tokio::test]
    async fn a_repository_sink_refuses_a_record_with_no_envelope() {
        let repo = Arc::new(RecordingRepo {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let sink = RepositorySink::new("lay_report", repo as Arc<dyn Repository>);

        let mut rec = record("hen-1", "1", Snapshot::False);
        rec.after = None;
        let err = sink.append(&rec).await.unwrap_err();
        assert!(
            err.to_string().contains("no `after` envelope"),
            "got: {err}"
        );
    }
}

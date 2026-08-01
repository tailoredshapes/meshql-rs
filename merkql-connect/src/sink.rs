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

    /// Append several records, returning only once **all** of them are durable.
    ///
    /// # Why this exists
    ///
    /// The database sources emit one record per committed row, so a batch is
    /// rarely more than one and this earns nothing. An ingress connector's
    /// *initial load* is the opposite case: an SAP ODP delta initialisation is
    /// a full read of the dataset, and paying a network round trip per row
    /// turns a few minutes of transfer into hours of latency.
    ///
    /// # Why a batch does not weaken the ordering rule
    ///
    /// The caller's rule is append-then-commit, and it survives unchanged: the
    /// whole batch is durable before any position derived from it is staged.
    /// A batch that fails halfway is *already* safe, because the position does
    /// not commit and the entire batch replays — duplicates, which folds
    /// absorb, and never a gap.
    ///
    /// So an implementation needs **durability, not atomicity**. It must not
    /// return `Ok` unless every record is durable, and a partial write must be
    /// reported as an error rather than a short success — but it does not need
    /// a transaction, and the default below is correct precisely because a
    /// partial loop failure is indistinguishable from any other failure here.
    ///
    /// The default appends one at a time, so a sink that has nothing better to
    /// offer inherits today's behaviour exactly.
    async fn append_batch(&self, records: &[ChangeRecord]) -> Result<()> {
        for record in records {
            self.append(record).await?;
        }
        Ok(())
    }

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

    /// Goes through merkql's `send_batch`.
    ///
    /// Be clear about what this does and does not buy. `send_batch` is itself a
    /// loop over the same per-record append — it does **not** amortise the
    /// segment write. What it amortises is the wake-up: one notification for
    /// the batch instead of one per record. On a local filesystem that is a
    /// modest win, not a transformative one, and the honest reason to route
    /// through it is that the alternative — this sink alone ignoring the batch
    /// API merkql offers — would have to be justified instead.
    async fn append_batch(&self, records: &[ChangeRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let producer_records = records
            .iter()
            .map(|record| {
                let value = serde_json::to_string(record).context("serializing change record")?;
                Ok(ProducerRecord::new(&self.topic, record.key(), value))
            })
            .collect::<Result<Vec<_>>>()?;

        Broker::producer(&self.broker)
            .send_batch(&producer_records)
            .map_err(|e| {
                anyhow::anyhow!(
                    "appending {} records to merkql topic '{}': {e}",
                    producer_records.len(),
                    self.topic
                )
            })?;
        Ok(())
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

    /// Write one same-token run and clear it. See [`TopicSink::append_batch`].
    async fn flush(
        &self,
        run: &mut Vec<meshql_core::Envelope>,
        tokens: &mut Option<Vec<String>>,
    ) -> Result<()> {
        if run.is_empty() {
            return Ok(());
        }
        let envelopes = std::mem::take(run);
        let tokens = tokens.take().unwrap_or_default();
        let count = envelopes.len();
        self.repository
            .create_many(envelopes, &tokens)
            .await
            .with_context(|| {
                format!("appending {count} records to queue topic '{}'", self.topic)
            })?;
        Ok(())
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

    /// Goes through `Repository::create_many`, which is where the real win is.
    ///
    /// Unlike merkql's, this is a genuine batch on the adapters that implement
    /// it as one — a single multi-row `INSERT` or `insert_many` instead of N
    /// round trips, which over a network is the whole cost of a bulk load.
    /// Adapters that still loop internally are no worse off than before.
    ///
    /// All records must share one token set, which they do: the tokens come
    /// from the envelope, and an ingress connector stamps every envelope from
    /// the same configured list. A batch whose envelopes disagree is split
    /// rather than being sent under one envelope's tokens — passing the wrong
    /// caller credentials to an adapter that filters on write would either
    /// reject the append or, worse, store rows nobody can read back.
    async fn append_batch(&self, records: &[ChangeRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        let mut run: Vec<meshql_core::Envelope> = Vec::with_capacity(records.len());
        let mut run_tokens: Option<Vec<String>> = None;

        for record in records {
            let envelope = record.after.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "change record for topic '{}' has no `after` envelope; a Repository sink \
                     has nothing to append. meshql sources are append-only and never emit a \
                     Debezium delete, so this is a source bug — failing rather than dropping \
                     the record.",
                    self.topic
                )
            })?;

            match &run_tokens {
                Some(tokens) if tokens == &envelope.authorized_tokens => {}
                None => run_tokens = Some(envelope.authorized_tokens.clone()),
                Some(_) => {
                    self.flush(&mut run, &mut run_tokens).await?;
                    run_tokens = Some(envelope.authorized_tokens.clone());
                }
            }
            run.push(envelope.clone());
        }

        self.flush(&mut run, &mut run_tokens).await
    }

    fn topic(&self) -> &str {
        &self.topic
    }

    fn backend(&self) -> &'static str {
        "repository"
    }
}

/// The most records appended in one call.
///
/// A ceiling on how much is replayed after a crash, not a throughput knob: the
/// batch is durable before its position commits, so an interrupted batch costs
/// at most this many duplicates. Large enough that a bulk load stops paying a
/// round trip per row, small enough that the replay is uninteresting.
const MAX_BATCH: usize = 512;

/// Run one source into one topic, forever.
///
/// The loop's whole job is the ordering rule: **append, then commit the
/// position.** Everything else is policy about how to start.
///
/// # Batching, and why it changes nothing about correctness
///
/// Records are appended in batches rather than one at a time, because an
/// ingress connector's initial load is a bulk transfer and a round trip per row
/// is its whole cost. The batch is assembled **greedily and without waiting**:
/// the loop blocks for one record, then takes only what is *already* available.
/// A trickling source therefore still produces batches of one and behaves
/// exactly as before — no added latency, no timer, and nothing to configure.
///
/// The ordering rule is unchanged because a batch is durable before any
/// position drawn from it is staged. Only the last position in the batch is
/// staged: earlier ones name records the same append already made durable, and
/// records *after* it carry no position at all — which is the fan-out rule, and
/// means they replay on restart rather than being skipped.
pub async fn run_connector(
    source: &dyn CommitSource,
    writer: &dyn TopicSink,
    offsets: &mut OffsetStore,
    mode: SnapshotMode,
) -> Result<()> {
    use std::task::Poll;

    let mut stream = open_stream(source, offsets, mode).await?;

    loop {
        let mut batch: Vec<ChangeRecord> = Vec::new();
        let mut deferred: Option<CdcError> = None;
        let mut ended = false;

        // Block for the first record. Everything after it is opportunistic.
        match stream.next().await {
            Some(Ok(record)) => batch.push(record),
            Some(Err(e)) => deferred = Some(e),
            None => ended = true,
        }

        // Take whatever else is ready *right now*. `Poll::Pending` ends the
        // batch, so a quiet source is never held back waiting for company.
        while deferred.is_none() && !ended && batch.len() < MAX_BATCH {
            match futures::poll!(stream.next()) {
                Poll::Ready(Some(Ok(record))) => batch.push(record),
                Poll::Ready(Some(Err(e))) => deferred = Some(e),
                Poll::Ready(None) => ended = true,
                Poll::Pending => break,
            }
        }

        // 1. Append first, and the whole batch. If this fails no position is
        //    committed, so every record in it is re-delivered on the next
        //    start.
        if !batch.is_empty() {
            writer.append_batch(&batch).await?;

            // 2. Only then stage, and only the furthest position the batch
            //    reached. Everything before it is durable by the append above.
            if let Some(last) = batch.iter().rev().find(|r| r.position().is_some()) {
                let position = last.position().expect("filtered on `is_some`");
                offsets.stage(position, last.source.snapshot.in_progress());
                // 3. And only once a commit has actually hit the disk may the
                //    source be told to let go of what it was holding. Doing
                //    this on `stage` rather than on a real commit would hand a
                //    PostgreSQL slot permission to recycle WAL for records
                //    that are not durable anywhere.
                if let Some(committed) = offsets.maybe_commit()? {
                    source.durable_through(&committed).await?;
                }
            }
        }

        // An error is handled only after the records that preceded it are
        // durable — otherwise re-opening the stream would discard a batch that
        // had already been read.
        if let Some(error) = deferred {
            match error {
                // A position that goes bad mid-stream (a slot dropped under us,
                // an oplog roll-over during a stall) gets the same policy as
                // one that was bad at startup — never a silent restart from
                // elsewhere.
                CdcError::UnusablePosition {
                    connector,
                    position,
                    reason,
                } => {
                    if !mode.recovers_from_unusable_position() {
                        bail!(
                            "{connector}: position {position:?} became unusable ({reason}) and \
                             snapshot_mode is not `when_needed`, so recovering would mean \
                             silently choosing a new start point. Refusing. Set snapshot_mode = \
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
                e => return Err(e.into()),
            }
        }

        if ended {
            break;
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
            envelopes: Vec<Envelope>,
            tokens: &[String],
        ) -> meshql_core::Result<Vec<Envelope>> {
            let mut seen = self.seen.lock().unwrap();
            for envelope in &envelopes {
                seen.push((envelope.id.clone(), tokens.to_vec()));
            }
            Ok(envelopes)
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

    /// A sink that records the shape of every call, so a test can assert what
    /// was batched rather than merely that the records arrived.
    #[derive(Default)]
    struct BatchSpy {
        calls: std::sync::Mutex<Vec<usize>>,
        fail_on_batch: Option<usize>,
    }

    #[async_trait]
    impl TopicSink for BatchSpy {
        async fn append(&self, _record: &ChangeRecord) -> Result<()> {
            // Deliberately unimplemented: `run_connector` must go through
            // `append_batch`, and a silent fallback to per-record appends is
            // exactly the regression this test guards.
            unreachable!("run_connector must append via append_batch")
        }
        async fn append_batch(&self, records: &[ChangeRecord]) -> Result<()> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(records.len());
            if Some(calls.len() - 1) == self.fail_on_batch {
                bail!("scripted sink failure");
            }
            Ok(())
        }
        fn topic(&self) -> &str {
            "spy"
        }
        fn backend(&self) -> &'static str {
            "spy"
        }
    }

    /// Records already available are appended together, in one call.
    #[tokio::test]
    async fn ready_records_are_appended_as_one_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = offsets(dir.path());
        let spy = BatchSpy::default();

        let source = Scripted {
            records: (1..=5)
                .map(|i| Ok(record(&format!("hen-{i}"), &i.to_string(), Snapshot::False)))
                .collect(),
            cold_only: false,
        };
        run_connector(&source, &spy, &mut store, SnapshotMode::Initial)
            .await
            .unwrap();

        let calls = spy.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![5],
            "five immediately-available records must cost one append, not five"
        );
        // And the furthest position still commits, exactly as before.
        assert_eq!(store.resume(), Resume::At("5".to_string()));
    }

    /// The cap bounds a batch, so a crash replays at most `MAX_BATCH`.
    #[tokio::test]
    async fn a_batch_never_exceeds_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = offsets(dir.path());
        let spy = BatchSpy::default();

        let n = MAX_BATCH + 7;
        let source = Scripted {
            records: (1..=n)
                .map(|i| Ok(record(&format!("hen-{i}"), &i.to_string(), Snapshot::False)))
                .collect(),
            cold_only: false,
        };
        run_connector(&source, &spy, &mut store, SnapshotMode::Initial)
            .await
            .unwrap();

        let calls = spy.calls.lock().unwrap().clone();
        assert_eq!(calls, vec![MAX_BATCH, 7]);
        assert!(calls.iter().all(|n| *n <= MAX_BATCH));
    }

    /// A failed batch commits no position, so every record in it replays.
    /// This is the ordering rule, and batching must not weaken it.
    #[tokio::test]
    async fn a_failed_batch_commits_no_position() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = offsets(dir.path());
        let spy = BatchSpy {
            calls: Default::default(),
            fail_on_batch: Some(0),
        };

        let source = Scripted {
            records: (1..=3)
                .map(|i| Ok(record(&format!("hen-{i}"), &i.to_string(), Snapshot::False)))
                .collect(),
            cold_only: false,
        };
        let err = run_connector(&source, &spy, &mut store, SnapshotMode::Initial)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("scripted sink failure"), "{err}");

        assert_eq!(
            store.resume(),
            Resume::Cold,
            "a batch that failed must leave no position behind — the records replay"
        );
    }

    /// Only the *last* position in a batch is staged. The earlier ones name
    /// records the same append already made durable.
    #[tokio::test]
    async fn a_batch_stages_only_its_furthest_position() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = offsets(dir.path());
        let spy = BatchSpy::default();

        // Interior records carry no position, as the fan-out rule requires;
        // only the cycle's last one does.
        let mut records = vec![
            Ok(record("hen-1", "1", Snapshot::False)),
            Ok(record("hen-2", "2", Snapshot::False)),
        ];
        let mut trailing = record("hen-3", "3", Snapshot::False);
        trailing.source.position = None;
        records.push(Ok(trailing));

        let source = Scripted {
            records,
            cold_only: false,
        };
        run_connector(&source, &spy, &mut store, SnapshotMode::Initial)
            .await
            .unwrap();

        assert_eq!(
            store.resume(),
            Resume::At("2".to_string()),
            "the furthest position with a value wins; the trailing positionless \
             record replays rather than being skipped"
        );
    }

    /// A `RepositorySink` batch is split where the token set changes, so no
    /// envelope is ever written under another envelope's credentials.
    #[tokio::test]
    async fn a_repository_batch_splits_where_tokens_change() {
        let repo = Arc::new(RecordingRepo {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let sink = RepositorySink::new("lay_report", Arc::clone(&repo) as Arc<dyn Repository>);

        let mut a = record("hen-1", "1", Snapshot::False);
        a.after.as_mut().unwrap().authorized_tokens = vec!["farm-a".into()];
        let mut b = record("hen-2", "2", Snapshot::False);
        b.after.as_mut().unwrap().authorized_tokens = vec!["farm-a".into()];
        let mut c = record("hen-3", "3", Snapshot::False);
        c.after.as_mut().unwrap().authorized_tokens = vec!["farm-b".into()];

        sink.append_batch(&[a, b, c]).await.unwrap();

        let seen = repo.seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "every record must be written exactly once");
        assert_eq!(seen[0].1, vec!["farm-a".to_string()]);
        assert_eq!(seen[1].1, vec!["farm-a".to_string()]);
        assert_eq!(
            seen[2].1,
            vec!["farm-b".to_string()],
            "the differing-token record must not inherit the run's tokens"
        );
    }
}

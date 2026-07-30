//! How many store requests a poll actually costs.
//!
//! This matters because it is the counter-pressure against overshooting the
//! partition count: partition count is immutable, so the advice is to overshoot,
//! but every partition is polled by a worker and swept every five minutes, and
//! *idle polling* is named as a leading cause of a bill that climbs on flat
//! volume.
//!
//! Three documents state the idle cost as **one `head`, plus a `list` when the
//! head shows nothing new** — merk-cloud's README ("Peer roll discovery costs a
//! `list`"), the `designing-on-merk-cloud` skill (§2a, §5), and sociallymeshy's
//! `docs/architecture.md` (§2.2, §5.1, §5.5, which prices the sweep at "30 `head`
//! calls plus up to 30 `list` calls"). On S3 that would matter a lot, because
//! LIST is billed with PUT rather than with GET.
//!
//! It is no longer true. `Partition::discover` says so in its own doc comment —
//! *"Deliberately not a listing … the next segment's key is predictable, and
//! asking whether it exists is a single `head`"* — so an idle poll is **two
//! `head`s and no `list` at all**. This test pins that, because it is a cost
//! model three documents depend on and the code moved out from under them.
//!
//! Counting happens against the in-memory backend on purpose: what is being
//! measured is how many times the engine calls the store, which is a property of
//! the engine and identical on S3.

use merk_object::backend::{Appended, Backend, SegmentLimits};
use merk_object::broker::{Broker, BrokerConfig, BrokerRef};
use merk_object::memory::MemoryBackend;
use merk_object::record::ProducerRecord;
use meshql_merk::consumer::{SafeConsumer, Start};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static HEADS: AtomicU64 = AtomicU64::new(0);
static GETS: AtomicU64 = AtomicU64::new(0);
static RANGES: AtomicU64 = AtomicU64::new(0);
static LISTS: AtomicU64 = AtomicU64::new(0);
static PUTS: AtomicU64 = AtomicU64::new(0);
static APPENDS: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    heads: u64,
    gets: u64,
    ranges: u64,
    lists: u64,
    puts: u64,
    appends: u64,
}

fn snapshot() -> Counts {
    Counts {
        heads: HEADS.load(Ordering::Relaxed),
        gets: GETS.load(Ordering::Relaxed),
        ranges: RANGES.load(Ordering::Relaxed),
        lists: LISTS.load(Ordering::Relaxed),
        puts: PUTS.load(Ordering::Relaxed),
        appends: APPENDS.load(Ordering::Relaxed),
    }
}

/// Requests made since `before`.
fn since(before: &Counts) -> Counts {
    let now = snapshot();
    Counts {
        heads: now.heads - before.heads,
        gets: now.gets - before.gets,
        ranges: now.ranges - before.ranges,
        lists: now.lists - before.lists,
        puts: now.puts - before.puts,
        appends: now.appends - before.appends,
    }
}

/// Serialises the whole file, because the counters are process-global and cargo
/// runs tests in parallel by default.
///
/// Found the hard way: without this, `opening_a_broker_costs_one_list_per_process`
/// observed **three** `list`s rather than one — its own, plus one from each of the
/// other two tests' setup running concurrently. It had passed under
/// `--test-threads=1` and passed twice more under the default before failing in a
/// pre-push hook, which is the worst possible failure schedule for a test whose
/// job is to pin a cost model. Every test in this file holds the lock for its
/// entire body, setup included — three short tests, so serialising them is free.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Counting {
    inner: MemoryBackend,
}

impl Backend for Counting {
    fn open(location: &str) -> anyhow::Result<Self> {
        Ok(Counting {
            inner: MemoryBackend::open(location)?,
        })
    }
    fn head(&self, key: &str) -> anyhow::Result<Option<u64>> {
        HEADS.fetch_add(1, Ordering::Relaxed);
        self.inner.head(key)
    }
    fn append_at(&self, key: &str, offset: u64, data: &[u8]) -> anyhow::Result<Appended> {
        APPENDS.fetch_add(1, Ordering::Relaxed);
        self.inner.append_at(key, offset, data)
    }
    fn get_range(&self, key: &str, from: u64, len: u64) -> anyhow::Result<Vec<u8>> {
        RANGES.fetch_add(1, Ordering::Relaxed);
        self.inner.get_range(key, from, len)
    }
    fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        GETS.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key)
    }
    fn put(&self, key: &str, data: &[u8]) -> anyhow::Result<()> {
        PUTS.fetch_add(1, Ordering::Relaxed);
        self.inner.put(key, data)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        LISTS.fetch_add(1, Ordering::Relaxed);
        self.inner.list(prefix)
    }
    fn segment_limits(&self) -> SegmentLimits {
        self.inner.segment_limits()
    }
    fn seal(&self, key: &str) -> anyhow::Result<()> {
        self.inner.seal(key)
    }
}

const TOPIC: &str = "t";

fn open(location: &str) -> BrokerRef<Counting> {
    Broker::<Counting>::open(BrokerConfig::new(location)).unwrap()
}

#[test]
fn an_idle_poll_costs_two_heads_and_no_list() {
    let _exclusive = exclusive();
    let location = "mem://idle-poll";
    let broker = open(location);
    broker.create_topic(TOPIC, 1).unwrap();
    Broker::producer(&broker)
        .send(&ProducerRecord::new(TOPIC, Some("k0".into()), "v0"))
        .unwrap();

    let mut consumer =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 0, Start::Earliest).unwrap();
    let folded = consumer
        .fold(|batch| {
            assert_eq!(batch.len(), 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(folded, 1);

    // Now the partition is caught up. This is the shape of every sweep tick and
    // every spurious wake-up.
    let before = snapshot();
    assert!(consumer.peek().unwrap().is_empty());
    let idle = since(&before);

    assert_eq!(
        idle.lists, 0,
        "an idle poll issued a LIST, which S3 bills at the write rate: {idle:?}"
    );
    assert_eq!(
        idle.heads, 2,
        "an idle poll should be exactly two heads — the active segment, then the \
         predictable next segment key to see whether a peer rolled: {idle:?}"
    );
    assert_eq!(idle.ranges, 0, "nothing was read: {idle:?}");
    assert_eq!(idle.gets, 0, "nothing was read: {idle:?}");
}

/// A productive poll: at most two `head`s and one ranged read, still no `list`.
///
/// Two rather than one when the partition handle has already ingested the growth
/// — `refresh` sees the active segment has not grown *since it last looked*, so
/// it falls through to the peer-roll check, which is the second `head`. Either
/// way the read-class/write-class split is what matters: nothing here is billed
/// as a LIST.
#[test]
fn a_productive_poll_costs_at_most_two_heads_and_one_ranged_read() {
    let _exclusive = exclusive();
    let location = "mem://busy-poll";
    let broker = open(location);
    broker.create_topic(TOPIC, 1).unwrap();
    let producer = Broker::producer(&broker);
    producer
        .send(&ProducerRecord::new(TOPIC, Some("k0".into()), "v0"))
        .unwrap();

    let mut consumer =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 0, Start::Earliest).unwrap();
    consumer.fold(|_| Ok(())).unwrap();

    // A peer appends. Same process here, but the consumer's partition handle is
    // the one the producer used, so force the cheap path by measuring a fresh
    // reader instead.
    producer
        .send(&ProducerRecord::new(TOPIC, Some("k1".into()), "v1"))
        .unwrap();

    let reader = open(location);
    let consumer =
        SafeConsumer::open(Arc::clone(&reader), "other", TOPIC, 0, Start::Latest).unwrap();
    drop(consumer);

    let catching_up =
        SafeConsumer::open(Arc::clone(&reader), "catcher", TOPIC, 0, Start::Earliest).unwrap();
    let before = snapshot();
    let batch = catching_up.peek().unwrap();
    let busy = since(&before);

    assert_eq!(batch.len(), 2);
    assert_eq!(busy.lists, 0, "a productive poll issued a LIST: {busy:?}");
    assert!(
        busy.heads <= 2,
        "a productive poll should need at most the active-segment head and the \
         peer-roll head: {busy:?}"
    );
    assert_eq!(
        busy.ranges, 1,
        "the records came back in one ranged read, not one per record: {busy:?}"
    );
}

/// Opening a broker costs one `list` — the only `list` in the steady state, and
/// it is per process rather than per poll. A Lambda pays it on cold start.
#[test]
fn opening_a_broker_costs_one_list_per_process() {
    let _exclusive = exclusive();
    let location = "mem://broker-open";
    let seed = open(location);
    for topic in ["a", "b", "c"] {
        seed.create_topic(topic, 2).unwrap();
    }
    drop(seed);

    let before = snapshot();
    let reopened = open(location);
    let cost = since(&before);
    assert_eq!(reopened.topic("a").unwrap().partition_ids().len(), 2);

    assert_eq!(
        cost.lists, 1,
        "a broker open should list the topic registry exactly once: {cost:?}"
    );
    assert_eq!(
        cost.gets, 3,
        "one metadata get per topic discovered: {cost:?}"
    );
    assert_eq!(
        cost.heads, 0,
        "opening a broker must not touch partitions — they open lazily: {cost:?}"
    );
}

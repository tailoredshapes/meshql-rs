//! Segment rolling, against a real S3 Express One Zone directory bucket.
//!
//! `merk-cloud`'s status table records `merk-aws` as certified 27/27 against a
//! live directory bucket but **not yet run with segment rolling**. The engine's
//! rolling path is covered in memory (`merk-object/tests/cert_rolling.rs` and the
//! unit tests in `partition.rs`), so what is unproven is specifically the
//! interaction of rolling with the S3 binding: whether sealing writes a sidecar
//! that reads back, whether a fresh object is created at the right key, whether a
//! ranged read spanning two objects returns a dense offset run, and whether the
//! merkle tree survives a resume from a checkpoint written to S3 and hydrates
//! correctly for a proof in a long-sealed segment.
//!
//! Everything about a segment boundary is supposed to be invisible in the
//! contract: offsets stay dense, reads span boundaries, and the root does not
//! depend on how the log was divided. So these tests assert the *absence* of a
//! boundary rather than its presence, having first made certain a boundary is
//! there.
//!
//! ```sh
//! MESHQL_MERK_TEST_LOCATION=s3://mybucket--use1-az4--x-s3/rolling \
//!   cargo test -p meshql-merk --features live --test live_rolling
//!
//! # and the slow one, which crosses the real 9,000-append ceiling:
//! MESHQL_MERK_TEST_LOCATION=... \
//!   cargo test -p meshql-merk --features live --test live_rolling -- --ignored --nocapture
//! ```

use merk_aws::S3Backend;
use merk_object::backend::Backend;
use merk_object::broker::{Broker as GenericBroker, BrokerConfig, BrokerRef};
use merk_object::record::ProducerRecord;
use merk_object::topic::partition_prefix;
use meshql_merk::aws::Broker;

fn location() -> String {
    std::env::var("MESHQL_MERK_TEST_LOCATION")
        .expect("set MESHQL_MERK_TEST_LOCATION to an s3://<directory-bucket>/<prefix> location")
}

fn topic(kind: &str) -> String {
    format!("roll_{kind}_{}", uuid::Uuid::new_v4().simple())
}

/// `max_segment_records` overrides `SegmentLimits::max_records`. `None` leaves
/// the backend's own limits alone, which on S3 means rolling at **9,000
/// appends** (`merk-aws/src/s3.rs` `segment_limits`, overriding the generic
/// 4,000) or 5,000,000 records or 1 GiB, whichever comes first.
fn open(max_segment_records: Option<u32>) -> BrokerRef<S3Backend> {
    let mut config = BrokerConfig::new(location()).with_auto_create_topics(false);
    config.max_segment_records = max_segment_records;
    Broker::open(config).unwrap()
}

fn append(broker: &BrokerRef<S3Backend>, topic: &str, values: impl Iterator<Item = String>) {
    let producer = GenericBroker::producer(broker);
    for (i, value) in values.enumerate() {
        // A fixed key so every record routes to the same partition: rolling is a
        // per-partition property and spreading the records would need 9,000 per
        // partition to provoke one.
        producer
            .send(&ProducerRecord::new(
                topic,
                Some("all-one-partition".into()),
                value,
            ))
            .unwrap_or_else(|e| panic!("append {i} failed: {e:#}"));
    }
}

/// Which partition a fixed key routes to. Rolling is per partition, and the key
/// is fixed above, so every assertion has to look at the same one.
fn routed_partition(broker: &BrokerRef<S3Backend>, topic: &str) -> u32 {
    broker
        .topic(topic)
        .expect("topic exists")
        .route(&Some("all-one-partition".to_string()))
}

struct Shape {
    segments: usize,
    sealed: usize,
    next_offset: u64,
    root: String,
}

fn shape(broker: &BrokerRef<S3Backend>, topic: &str, partition: u32) -> Shape {
    let handle = broker.topic(topic).expect("topic exists");
    let part = handle.partition(partition).expect("partition exists");
    let mut guard = part.write().unwrap();
    guard.refresh().unwrap();
    Shape {
        segments: guard.segment_count(),
        sealed: guard.sealed_segment_count(),
        next_offset: guard.next_offset(),
        root: guard
            .merkle_root()
            .unwrap()
            .expect("a non-empty partition has a root")
            .to_hex(),
    }
}

// ---------------------------------------------------------------------------
// A deliberately provoked roll, cheaply
// ---------------------------------------------------------------------------

#[test]
fn a_provoked_roll_is_invisible_in_the_contract() {
    const TOTAL: u64 = 13;
    const PER_SEGMENT: u32 = 3;

    let topic = topic("forced");
    let broker = open(Some(PER_SEGMENT));
    broker.create_topic(&topic, 1).unwrap();
    append(&broker, &topic, (0..TOTAL).map(|i| format!("v{i}")));

    let partition = routed_partition(&broker, &topic);
    let before = shape(&broker, &topic, partition);

    assert!(
        before.segments > 1,
        "no roll happened at {PER_SEGMENT} records per segment over {TOTAL} records — \
         the rest of this test proves nothing"
    );
    assert_eq!(
        before.sealed,
        before.segments - 1,
        "every segment but the active one must be sealed"
    );
    assert_eq!(
        before.next_offset, TOTAL,
        "offsets are dense across the rolls"
    );

    // The S3 objects themselves: several segments, and a sidecar per sealed one.
    let backend = S3Backend::open(&location()).unwrap();
    let prefix = format!("{}/", partition_prefix(&topic, partition));
    let keys = backend.list(&prefix).unwrap();
    let segments: Vec<&String> = keys
        .iter()
        .filter(|k| k.contains("/seg-") && !k.ends_with(".meta"))
        .collect();
    let sidecars: Vec<&String> = keys.iter().filter(|k| k.ends_with(".meta")).collect();
    assert_eq!(
        segments.len(),
        before.segments,
        "the store holds one object per segment; keys: {keys:?}"
    );
    assert_eq!(
        sidecars.len(),
        before.sealed,
        "each sealed segment wrote a sidecar; keys: {keys:?}"
    );

    // Reads spanning the boundaries.
    {
        let handle = broker.topic(&topic).unwrap();
        let part = handle.partition(partition).unwrap();
        let mut guard = part.write().unwrap();

        let all = guard.read_range(0, TOTAL).unwrap();
        assert_eq!(all.len() as u64, TOTAL);
        for (i, record) in all.iter().enumerate() {
            assert_eq!(record.offset, i as u64, "offset run has a gap at {i}");
            assert_eq!(record.value, format!("v{i}"));
        }

        // A window deliberately straddling two rolls.
        let window = guard.read_range(2, 8).unwrap();
        assert_eq!(window.len(), 6);
        assert_eq!(window[0].value, "v2");
        assert_eq!(window[5].value, "v7");

        // And a single read that lands inside a long-sealed segment, which is the
        // path that has to fetch the sidecar it was opened without.
        assert_eq!(guard.read(1).unwrap().unwrap().value, "v1");
    }

    // Reopen: the checkpoint must reproduce the root exactly, without replaying
    // history, and reads must still work.
    let reopened = open(Some(PER_SEGMENT));
    let after = shape(&reopened, &topic, partition);
    assert_eq!(after.next_offset, before.next_offset);
    assert_eq!(
        after.root, before.root,
        "the checkpoint written to S3 did not reproduce the merkle root"
    );
    assert_eq!(after.segments, before.segments);

    {
        let handle = reopened.topic(&topic).unwrap();
        let part = handle.partition(partition).unwrap();
        let mut guard = part.write().unwrap();
        assert_eq!(guard.read(0).unwrap().unwrap().value, "v0");
        assert_eq!(
            guard.read(TOTAL - 1).unwrap().unwrap().value,
            format!("v{}", TOTAL - 1)
        );
    }
}

/// Proofs are the part a checkpoint deliberately omits: it carries pending roots
/// and a record count, not merkle *nodes*. So the first proof after a resume has
/// to replay the log to regenerate them, and this is the test that the replay
/// produces nodes that verify — including for an offset in a segment sealed long
/// ago.
#[test]
fn proofs_survive_a_roll_and_a_resume() {
    const TOTAL: u64 = 13;
    const PER_SEGMENT: u32 = 3;

    let topic = topic("proof");
    let broker = open(Some(PER_SEGMENT));
    broker.create_topic(&topic, 1).unwrap();
    append(&broker, &topic, (0..TOTAL).map(|i| format!("v{i}")));
    let partition = routed_partition(&broker, &topic);
    assert!(
        shape(&broker, &topic, partition).segments > 1,
        "a roll happened"
    );
    drop(broker);

    // A cold open, which resumes from the newest checkpoint and holds no nodes.
    let reopened = open(Some(PER_SEGMENT));
    let handle = reopened.topic(&topic).unwrap();
    let part = handle.partition(partition).unwrap();
    let mut guard = part.write().unwrap();
    guard.refresh().unwrap();

    for offset in 0..TOTAL {
        let proof = guard
            .proof(offset)
            .unwrap()
            .unwrap_or_else(|| panic!("no proof for offset {offset}"));
        assert!(
            guard.verify_proof(&proof).unwrap(),
            "proof for offset {offset} does not verify after a roll and a resume"
        );
    }
    assert!(
        guard.proof(TOTAL).unwrap().is_none(),
        "a proof was issued for an offset past the tail"
    );
}

/// Two writers racing a partition that is at its rolling threshold. This is the
/// case a memory test cannot make convincing, because the whole point is that the
/// store arbitrates: the loser catches up on the winner's records, discovers it
/// is now sitting on a segment somebody else has sealed, and must roll rather
/// than append past the seal. Records written past a seal are silently
/// unreachable, since a sealed segment's length comes from the next segment's
/// base offset.
#[test]
fn two_writers_across_a_roll_boundary_lose_nothing() {
    const PER_SEGMENT: u32 = 4;
    const EACH: u64 = 12;

    let topic = topic("race");
    let setup = open(Some(PER_SEGMENT));
    setup.create_topic(&topic, 1).unwrap();
    drop(setup);

    std::thread::scope(|scope| {
        for writer in 0..2u32 {
            let topic = topic.clone();
            scope.spawn(move || {
                // Its own broker, so its own view of the tail.
                let broker = open(Some(PER_SEGMENT));
                append(&broker, &topic, (0..EACH).map(|i| format!("w{writer}-{i}")));
            });
        }
    });

    let reader = open(Some(PER_SEGMENT));
    let partition = routed_partition(&reader, &topic);
    let shape = shape(&reader, &topic, partition);
    let total = EACH * 2;

    assert_eq!(
        shape.next_offset, total,
        "records were lost or duplicated across a contended roll"
    );
    assert!(shape.segments > 1, "the run crossed at least one roll");

    let handle = reader.topic(&topic).unwrap();
    let part = handle.partition(partition).unwrap();
    let mut guard = part.write().unwrap();
    let all = guard.read_range(0, total).unwrap();
    assert_eq!(
        all.len() as u64,
        total,
        "a record fell into a sealed segment"
    );
    for (i, record) in all.iter().enumerate() {
        assert_eq!(record.offset, i as u64, "offset gap at {i}");
    }

    let mut values: Vec<String> = all.iter().map(|r| r.value.clone()).collect();
    values.sort();
    let mut expected: Vec<String> = (0..2)
        .flat_map(|w| (0..EACH).map(move |i| format!("w{w}-{i}")))
        .collect();
    expected.sort();
    assert_eq!(values, expected, "every writer's records are all present");
}

// ---------------------------------------------------------------------------
// The real ceiling
// ---------------------------------------------------------------------------

/// The threshold that actually fires in production. `merk-aws` overrides the
/// generic 4,000 appends with **9,000** — S3's appendable-object ceiling is
/// 10,000 parts, and reaching it makes a partition permanently unwritable — so
/// this appends 9,001 records one at a time with the limits left alone. Only
/// `max_appends` can trigger a roll at that volume: `max_records` is 5,000,000
/// and `max_bytes` is 1 GiB.
///
/// `#[ignore]` because it is 9,001 sequential round trips: measured **358 s at
/// 25 appends/s from a workstation**, and about 75 s in region, for roughly a
/// cent of requests. Run it deliberately rather than meeting the first roll in
/// production.
///
/// Resumable: set `MESHQL_MERK_CEILING_TOPIC` to a topic a previous run already
/// filled and the appends are skipped, so a wrong assertion can be corrected
/// without paying for 9,001 more round trips.
#[test]
#[ignore = "9,001 sequential appends; run explicitly with --ignored"]
fn the_append_ceiling_rolls_at_nine_thousand() {
    const TOTAL: u64 = 9_001;

    let existing = std::env::var("MESHQL_MERK_CEILING_TOPIC").ok();
    let topic = existing.clone().unwrap_or_else(|| topic("ceiling"));
    let broker = open(None);
    broker.create_topic(&topic, 1).unwrap();
    let partition = routed_partition(&broker, &topic);

    let already = shape(&broker, &topic, partition).next_offset;
    if already < TOTAL {
        assert!(
            existing.is_none(),
            "MESHQL_MERK_CEILING_TOPIC={topic} holds only {already} records, not {TOTAL}"
        );
        let started = std::time::Instant::now();
        append(&broker, &topic, (0..TOTAL).map(|i| format!("v{i}")));
        let elapsed = started.elapsed();
        eprintln!(
            "topic {topic}: {TOTAL} appends in {:.1}s ({:.1} appends/s)",
            elapsed.as_secs_f64(),
            TOTAL as f64 / elapsed.as_secs_f64(),
        );
    } else {
        eprintln!("topic {topic}: reusing {already} existing records");
    }

    let shape = shape(&broker, &topic, partition);
    eprintln!(
        "{} segments, {} sealed, next_offset {}",
        shape.segments, shape.sealed, shape.next_offset
    );

    assert_eq!(
        shape.segments, 2,
        "9,001 single appends must cross exactly one 9,000-append boundary"
    );
    assert_eq!(shape.sealed, 1);
    assert_eq!(
        shape.next_offset, TOTAL,
        "offsets are dense across the roll"
    );

    // The boundary is where it is claimed to be: the sealed segment holds
    // exactly 9,000 records, so the second object starts at offset 9,000.
    let backend = S3Backend::open(&location()).unwrap();
    let prefix = format!("{}/", partition_prefix(&topic, partition));
    let mut segment_bases: Vec<u64> = backend
        .list(&prefix)
        .unwrap()
        .iter()
        .filter(|k| !k.ends_with(".meta"))
        .filter_map(|k| k.rsplit("/seg-").next().and_then(|n| n.parse().ok()))
        .collect();
    segment_bases.sort_unstable();
    assert_eq!(
        segment_bases,
        vec![0, 9_000],
        "the roll did not land on the 9,000-append ceiling"
    );

    // And the boundary is invisible to a reader. The window has to stay inside
    // the log: offsets run 0..=9_000, so 9_001 is the exclusive end and asking
    // past it gets clamped rather than erroring.
    let handle = broker.topic(&topic).unwrap();
    let part = handle.partition(partition).unwrap();
    let mut guard = part.write().unwrap();

    let straddling = guard.read_range(8_997, TOTAL).unwrap();
    assert_eq!(
        straddling.len(),
        4,
        "a window straddling the 9,000 boundary came back short"
    );
    for (i, record) in straddling.iter().enumerate() {
        let offset = 8_997 + i as u64;
        assert_eq!(record.offset, offset, "offset gap across the boundary");
        assert_eq!(record.value, format!("v{offset}"));
    }

    // Single reads either side of the boundary, and one deep inside the sealed
    // segment — which is the path that has to fetch a sidecar describing 9,000
    // frames.
    for offset in [0u64, 4_500, 8_999, 9_000] {
        let record = guard
            .read(offset)
            .unwrap()
            .unwrap_or_else(|| panic!("offset {offset} is missing"));
        assert_eq!(record.value, format!("v{offset}"));
    }

    // Merkle proofs across the boundary. A checkpoint carries roots and a record
    // count but no nodes, so the first proof replays both segments to regenerate
    // them; that the replay produces verifying nodes is the property.
    for offset in [0u64, 8_999, 9_000] {
        let proof = guard
            .proof(offset)
            .unwrap()
            .unwrap_or_else(|| panic!("no proof for offset {offset}"));
        assert!(
            guard.verify_proof(&proof).unwrap(),
            "proof for offset {offset} does not verify across a 9,000-append roll"
        );
    }
}

//! The two consumer-offset defects, reproduced, and then shown not to arise
//! through `SafeConsumer`.
//!
//! These tests are the evidence behind `src/consumer.rs`'s module docs. They run
//! against the in-memory backend, because both defects live in the engine
//! (`merk-object`) rather than in the S3 binding, so a cloud account would add
//! cost and nothing else.
//!
//! Deliberately not a bug report against merk-cloud: defect 2 is documented
//! upstream. Defect 1 is not, and the reason to pin it here rather than there is
//! that the fix is a design rule this crate has to follow either way — one that
//! stays necessary until `Backend` grows a conditional put.

use merk_object::backend::Backend;
use merk_object::broker::{Broker as GenericBroker, BrokerConfig, BrokerRef};
use merk_object::consumer::{ConsumerConfig, OffsetReset};
use merk_object::group::{ConsumerGroup, TopicPartition};
use merk_object::mem::broker::Broker;
use merk_object::memory::MemoryBackend;
use merk_object::record::ProducerRecord;
use meshql_merk::consumer::{SafeConsumer, Start};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const TOPIC: &str = "t";

fn open(location: &str) -> BrokerRef<MemoryBackend> {
    Broker::open(BrokerConfig::new(location)).unwrap()
}

fn seed(broker: &BrokerRef<MemoryBackend>, n: usize) {
    broker.create_topic(TOPIC, 1).unwrap();
    let producer = GenericBroker::producer(broker);
    for i in 0..n {
        producer
            .send(&ProducerRecord::new(
                TOPIC,
                Some(format!("k{i}")),
                format!("v{i}"),
            ))
            .unwrap();
    }
}

fn tp() -> TopicPartition {
    TopicPartition {
        topic: TOPIC.to_string(),
        partition: 0,
    }
}

// ---------------------------------------------------------------------------
// Defect 1: auto_commit + a failing handler = permanent silent loss
// ---------------------------------------------------------------------------

/// `poll` sets each position to `tail` before returning, `close` commits when
/// `auto_commit` is set, and `Drop` calls `close`. A handler that returns `Err`
/// therefore unwinds through a commit of offsets for records it never processed.
///
/// This is at-most-once delivery. It is not replay, it is loss, and nothing logs
/// anything.
#[test]
fn auto_commit_plus_a_failing_handler_loses_the_batch_permanently() {
    let broker = open("mem://defect-auto-commit");
    seed(&broker, 3);

    // A worker that polls, tries to fold, and fails.
    let fold_result: Result<(), &str> = {
        let mut consumer = GenericBroker::consumer(
            &broker,
            ConsumerConfig {
                group_id: "loses-data".into(),
                auto_commit: true,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&[TOPIC]).unwrap();
        let batch = consumer.poll(Duration::from_millis(10)).unwrap();
        assert_eq!(batch.len(), 3, "all three records were read");

        // The projection write fails. The consumer is dropped by the `?`-shaped
        // early return a real worker would take.
        Err("projection write failed")
    };
    assert!(fold_result.is_err());

    // The group is now committed past records that were never folded.
    assert_eq!(
        broker.committed_offset("loses-data", &tp()).unwrap(),
        Some(3),
        "Drop committed the offsets of a batch that was never folded"
    );

    // And a fresh worker on the same group sees nothing to do, forever.
    let mut recovery = GenericBroker::consumer(
        &broker,
        ConsumerConfig {
            group_id: "loses-data".into(),
            auto_commit: false,
            offset_reset: OffsetReset::Earliest,
        },
    );
    recovery.subscribe(&[TOPIC]).unwrap();
    assert!(
        recovery.poll(Duration::from_millis(10)).unwrap().is_empty(),
        "the three records are unreachable: at-most-once, silent, permanent"
    );
}

/// The same worker, written with `SafeConsumer`: the cursor advances only after
/// the fold returns `Ok`, so a failure costs a retry rather than three events.
#[test]
fn safe_consumer_does_not_advance_past_a_failing_fold() {
    let broker = open("mem://safe-failing-fold");
    seed(&broker, 3);

    let mut consumer =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 0, Start::Earliest).unwrap();

    let failed = consumer.fold(|batch| {
        assert_eq!(batch.len(), 3);
        Err(meshql_core::MeshqlError::Storage(
            "projection write failed".into(),
        ))
    });
    assert!(failed.is_err());
    assert_eq!(consumer.position(), 0, "the cursor did not move");
    assert_eq!(
        broker.committed_offset(consumer.group_id(), &tp()).unwrap(),
        None,
        "nothing was committed"
    );

    // The retry sees exactly the same records.
    let mut seen = Vec::new();
    let folded = consumer
        .fold(|batch| {
            seen.extend(batch.iter().map(|r| r.value.clone()));
            Ok(())
        })
        .unwrap();
    assert_eq!(folded, 3);
    assert_eq!(seen, vec!["v0", "v1", "v2"]);
    assert_eq!(
        broker.committed_offset(consumer.group_id(), &tp()).unwrap(),
        Some(3)
    );
}

/// `peek` is the other half of the same property: reading must not be an
/// irreversible act. `Consumer::poll` makes it one.
#[test]
fn peek_is_repeatable() {
    let broker = open("mem://safe-peek");
    seed(&broker, 2);

    let consumer =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 0, Start::Earliest).unwrap();
    let first = consumer.peek().unwrap();
    let second = consumer.peek().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(
        first.iter().map(|r| r.offset).collect::<Vec<_>>(),
        second.iter().map(|r| r.offset).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Defect 2: commits are last-write-wins, so they can rewind
// ---------------------------------------------------------------------------

/// `ConsumerGroup::commit` serialises the whole offsets map and calls
/// `Backend::put`, which is an unconditional overwrite — the trait has no
/// conditional put. The map was loaded once at `open` and is never re-read, so
/// the forward-only guard protects one process's view and not two.
///
/// Two processes committing *different partitions of the same group* therefore
/// clobber each other. This is the documented limitation, confirmed rather than
/// assumed.
#[test]
fn two_committers_on_one_group_clobber_each_other() {
    let location = "mem://defect-lww";
    let backend = || Arc::new(MemoryBackend::open(location).unwrap());

    // Two processes, each loading the group's offsets before the other commits.
    let mut first = ConsumerGroup::open(backend(), "shared").unwrap();
    let mut second = ConsumerGroup::open(backend(), "shared").unwrap();

    let p0 = TopicPartition {
        topic: TOPIC.into(),
        partition: 0,
    };
    let p1 = TopicPartition {
        topic: TOPIC.into(),
        partition: 1,
    };

    first.commit(&HashMap::from([(p0.clone(), 10)])).unwrap();
    second.commit(&HashMap::from([(p1.clone(), 20)])).unwrap();

    let reloaded = ConsumerGroup::open(backend(), "shared").unwrap();
    assert_eq!(
        reloaded.committed_offset(&p1),
        Some(20),
        "the last writer's partition survived"
    );
    assert_eq!(
        reloaded.committed_offset(&p0),
        None,
        "and it erased the other partition's offset entirely"
    );
}

/// The consequence, stated precisely, because it decides whether this is a
/// correctness problem or a cost problem: a clobber can only push an offset
/// **backwards**. Every value a process writes is either one it read from the
/// store or one it advanced itself, and both are at most the true committed
/// maximum. So the failure mode is replay — at-least-once degradation and billed
/// requests — never a skip.
#[test]
fn a_clobber_can_rewind_but_never_skip() {
    let location = "mem://defect-lww-direction";
    let backend = || Arc::new(MemoryBackend::open(location).unwrap());
    let p0 = TopicPartition {
        topic: TOPIC.into(),
        partition: 0,
    };

    // Both processes load the group while it is empty. `behind` is now holding a
    // map that will never learn about anything `ahead` commits.
    let mut behind = ConsumerGroup::open(backend(), "shared").unwrap();
    let mut ahead = ConsumerGroup::open(backend(), "shared").unwrap();

    ahead.commit(&HashMap::from([(p0.clone(), 50)])).unwrap();
    behind.commit(&HashMap::from([(p0.clone(), 10)])).unwrap();

    let reloaded = ConsumerGroup::open(backend(), "shared").unwrap();
    let observed = reloaded.committed_offset(&p0).unwrap_or(0);

    assert_eq!(
        observed, 10,
        "the stale writer rewound the group from 50 to 10"
    );
    assert!(
        observed <= 50,
        "an offset moved forwards past what any process had folded: {observed}. \
         That would be a skip, which is loss; a rewind is only replay."
    );
}

/// The mitigation. A group id per `(topic, partition)` gives every offsets
/// object exactly one writer, so there is nothing to clobber.
#[test]
fn a_group_per_partition_makes_the_clobber_impossible() {
    let broker = open("mem://safe-group-per-partition");
    broker.create_topic(TOPIC, 2).unwrap();
    let producer = GenericBroker::producer(&broker);
    // Unique keys, so both partitions get records.
    for i in 0..40 {
        producer
            .send(&ProducerRecord::new(
                TOPIC,
                Some(format!("k{i}")),
                format!("v{i}"),
            ))
            .unwrap();
    }

    let mut zero =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 0, Start::Earliest).unwrap();
    let mut one =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 1, Start::Earliest).unwrap();
    assert_ne!(zero.group_id(), one.group_id());

    let folded_zero = zero.fold(|_| Ok(())).unwrap();
    let folded_one = one.fold(|_| Ok(())).unwrap();
    assert_eq!(folded_zero + folded_one, 40, "every record folded once");

    // Both survive, in either commit order, because they are different objects.
    assert_eq!(
        broker
            .committed_offset(
                zero.group_id(),
                &TopicPartition {
                    topic: TOPIC.into(),
                    partition: 0
                }
            )
            .unwrap(),
        Some(folded_zero as u64)
    );
    assert_eq!(
        broker
            .committed_offset(
                one.group_id(),
                &TopicPartition {
                    topic: TOPIC.into(),
                    partition: 1
                }
            )
            .unwrap(),
        Some(folded_one as u64)
    );
}

/// And one partition per invocation, which `Consumer` cannot express at all: its
/// `subscribe` takes topics, so a worker using it folds every partition.
#[test]
fn a_safe_consumer_reads_one_partition_and_the_raw_consumer_reads_all() {
    let broker = open("mem://one-partition");
    broker.create_topic(TOPIC, 4).unwrap();
    let producer = GenericBroker::producer(&broker);
    for i in 0..40 {
        producer
            .send(&ProducerRecord::new(
                TOPIC,
                Some(format!("k{i}")),
                format!("v{i}"),
            ))
            .unwrap();
    }

    let mut scoped =
        SafeConsumer::open(Arc::clone(&broker), "projector", TOPIC, 2, Start::Earliest).unwrap();
    let mut partitions = std::collections::HashSet::new();
    scoped
        .fold(|batch| {
            for record in batch {
                partitions.insert(record.partition);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        partitions,
        std::collections::HashSet::from([2]),
        "a SafeConsumer reads exactly the partition it was opened on"
    );

    let mut whole_topic = GenericBroker::consumer(
        &broker,
        ConsumerConfig {
            group_id: "reads-everything".into(),
            auto_commit: false,
            offset_reset: OffsetReset::Earliest,
        },
    );
    whole_topic.subscribe(&[TOPIC]).unwrap();
    let seen: std::collections::HashSet<u32> = whole_topic
        .poll(Duration::from_millis(10))
        .unwrap()
        .iter()
        .map(|r| r.partition)
        .collect();
    assert!(
        seen.len() > 1,
        "Consumer::subscribe takes topics, not partitions, so it read {seen:?}"
    );
}

//! The create path against a **real** S3 Express One Zone directory bucket.
//!
//! There is no emulator for this. Append-at-offset exists only on Express One
//! Zone and LocalStack does not implement it, so the choice is a real bucket or
//! no coverage of the wire binding at all. A bucket costs pennies and five
//! minutes.
//!
//! ```sh
//! aws s3api create-bucket --bucket mybucket--use1-az4--x-s3 --region us-east-1 \
//!   --create-bucket-configuration \
//!   '{"Location":{"Type":"AvailabilityZone","Name":"use1-az4"},
//!     "Bucket":{"Type":"Directory","DataRedundancy":"SingleAvailabilityZone"}}'
//!
//! MESHQL_MERK_TEST_LOCATION=s3://mybucket--use1-az4--x-s3/cert \
//!   cargo test -p meshql-merk --features live --test live_create_cert
//! ```

use merk_aws::S3Backend;
use merk_object::broker::{BrokerConfig, BrokerRef};
use meshql_core::{Envelope, Repository, Stash};
use meshql_merk::aws::Broker;
use meshql_merk::consumer::{SafeConsumer, Start};
use meshql_merk::conversion::value_to_envelope;
use meshql_merk::{MerkRepository, TopicPlan};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;

const PARTITIONS: u32 = 4;

fn location() -> String {
    std::env::var("MESHQL_MERK_TEST_LOCATION").expect(
        "set MESHQL_MERK_TEST_LOCATION to an s3://<directory-bucket>/<prefix> location; \
         append-at-offset has no emulator",
    )
}

/// A fresh topic per run, so a rerun cannot read the previous run's records and
/// call it a pass.
fn topic() -> String {
    format!("live_create_{}", uuid::Uuid::new_v4().simple())
}

fn open() -> BrokerRef<S3Backend> {
    Broker::open(BrokerConfig::new(location()).with_auto_create_topics(false)).unwrap()
}

fn star() -> Vec<String> {
    vec!["*".to_string()]
}

fn envelope(id: &str, body: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("type".into(), json!("story_created"));
    payload.insert("body".into(), json!(body));
    Envelope::new(id, payload, star())
}

/// Everything on the topic, read the way a worker reads: by offset range, one
/// partition at a time. This is a legitimate log read — it is the search that is
/// forbidden, not the consumption.
fn drain(broker: &BrokerRef<S3Backend>, topic: &str) -> Vec<Envelope> {
    let mut all = Vec::new();
    for partition in 0..PARTITIONS {
        let mut consumer = SafeConsumer::open(
            Arc::clone(broker),
            &format!("live-cert-{}", uuid::Uuid::new_v4().simple()),
            topic,
            partition,
            Start::Earliest,
        )
        .unwrap();
        consumer
            .fold(|batch| {
                for record in batch {
                    all.push(value_to_envelope(&record.value)?);
                }
                Ok(())
            })
            .unwrap();
    }
    all
}

#[tokio::test]
async fn create_lands_durably_in_a_real_directory_bucket() {
    let broker = open();
    let topic = topic();
    let plan = TopicPlan::from_toml_str(&format!(
        "[[topic]]\nname=\"{topic}\"\npartitions={PARTITIONS}\n"
    ))
    .unwrap();
    assert_eq!(
        meshql_merk::provision(&broker, &plan).unwrap(),
        vec![(topic.clone(), PARTITIONS)]
    );

    let repo = MerkRepository::new(&broker, &topic);
    for i in 0..12 {
        let stored = repo
            .create(
                envelope(&format!("e-{i:02}"), &format!("body-{i}")),
                &star(),
            )
            .await
            .unwrap();
        assert_eq!(stored.id, format!("e-{i:02}"));
    }

    // A *new* broker, so nothing is served out of the writer's warm state: what
    // is being asserted is that the bytes are in the store, which is what makes
    // a 201 mean committed.
    let reader = open();
    let read_back = drain(&reader, &topic);
    assert_eq!(read_back.len(), 12, "every append is durable");

    let ids: BTreeSet<String> = read_back.iter().map(|e| e.id.clone()).collect();
    let expected: BTreeSet<String> = (0..12).map(|i| format!("e-{i:02}")).collect();
    assert_eq!(ids, expected);

    for env in &read_back {
        assert_eq!(env.authorized_tokens, star());
        assert!(!env.deleted);
        assert!(env.payload.get("body").is_some());
    }
}

/// Unique keys spread across the partitions, which is what a many-writer gateway
/// needs and what makes more than one partition the right choice here rather than
/// the trap it is on merkql.
#[tokio::test]
async fn unique_keys_spread_across_partitions() {
    let broker = open();
    let topic = topic();
    let plan = TopicPlan::from_toml_str(&format!(
        "[[topic]]\nname=\"{topic}\"\npartitions={PARTITIONS}\n"
    ))
    .unwrap();
    meshql_merk::provision(&broker, &plan).unwrap();

    let repo = MerkRepository::new(&broker, &topic);
    let batch: Vec<Envelope> = (0..40)
        .map(|i| envelope(&uuid::Uuid::new_v4().to_string(), &format!("b{i}")))
        .collect();
    repo.create_many(batch, &star()).await.unwrap();

    let topic_handle = broker.topic(&topic).unwrap();
    let mut occupied = 0;
    let mut total = 0u64;
    for partition in 0..PARTITIONS {
        let part = topic_handle.partition(partition).unwrap();
        let mut guard = part.write().unwrap();
        guard.refresh().unwrap();
        let n = guard.next_offset();
        total += n;
        if n > 0 {
            occupied += 1;
        }
    }
    assert_eq!(total, 40, "no record was lost in the fan-out");
    assert!(
        occupied >= 3,
        "40 unique keys landed on only {occupied} of {PARTITIONS} partitions"
    );
}

#[tokio::test]
async fn reads_are_refused_against_a_real_bucket_too() {
    let broker = open();
    let topic = topic();
    let plan =
        TopicPlan::from_toml_str(&format!("[[topic]]\nname=\"{topic}\"\npartitions=1\n")).unwrap();
    meshql_merk::provision(&broker, &plan).unwrap();
    let repo = MerkRepository::new(&broker, &topic);
    repo.create(envelope("only", "x"), &star()).await.unwrap();

    assert!(repo.read("only", &star(), None).await.is_err());
    assert!(repo.list(&star()).await.is_err());
    assert!(repo
        .read_many(&["only".to_string()], &star())
        .await
        .is_err());
    assert!(repo.remove("only", &star()).await.is_err());
}

//! `create_many` writes the batch with a single unordered `insert_many`. These
//! tests hold it to the contract that makes that safe: everything it reports as
//! written is readable, the order it returns is the order it was given, a
//! failure is an `Err`, and the result is indistinguishable from the same
//! envelopes written one at a time.

mod common;

use bson::{doc, Document};
use common::{fresh_collection, shared_mongo};
use meshql_core::{Envelope, NoAuth, Repository, Stash};
use meshql_mongo::MongoRepository;
use mongodb::options::IndexOptions;
use mongodb::{Collection, IndexModel};
use serde_json::json;
use std::sync::Arc;

const DB: &str = "batch_test_db";

/// A repository, plus a raw handle on the same collection — the repository's
/// own handle is private, and these tests need to count documents and install
/// an index behind its back.
async fn create_repo() -> (MongoRepository, Collection<Document>, impl std::any::Any) {
    let node = shared_mongo().await;
    let name = fresh_collection();
    let repo = MongoRepository::new(&node.uri, DB, &name, Arc::new(NoAuth))
        .await
        .unwrap();
    let raw = mongodb::Client::with_uri_str(&node.uri)
        .await
        .unwrap()
        .database(DB)
        .collection::<Document>(&name);
    (repo, raw, node)
}

fn star() -> Vec<String> {
    vec!["*".to_string()]
}

fn envelope(id: &str, name: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(name));
    Envelope::new(id, payload, star())
}

#[tokio::test]
async fn batch_round_trips_every_envelope_in_input_order() {
    let (repo, raw, _c) = create_repo().await;

    let inputs: Vec<Envelope> = (0..25)
        .map(|i| envelope(&format!("batch-{i:03}"), &format!("name-{i}")))
        .collect();

    let written = repo.create_many(inputs.clone(), &star()).await.unwrap();

    assert_eq!(written.len(), inputs.len());
    for (returned, input) in written.iter().zip(inputs.iter()) {
        assert_eq!(returned.id, input.id, "returned out of input order");
        assert_eq!(returned.payload, input.payload);
    }
    assert_eq!(
        raw.count_documents(doc! {}).await.unwrap(),
        inputs.len() as u64
    );

    for input in &inputs {
        let stored = repo
            .read(&input.id, &star(), None)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{} was reported written but is not readable", input.id));
        assert_eq!(stored.payload, input.payload);
        assert!(!stored.deleted);
        assert_eq!(stored.authorized_tokens, star());
        assert_eq!(
            stored.created_at.timestamp_millis(),
            input.created_at.timestamp_millis()
        );
    }
}

/// The driver splits a run past the server's `maxWriteBatchSize` on its own, so
/// there is no chunk size of ours to test — only that a batch far larger than
/// any single test above still lands whole.
#[tokio::test]
async fn large_batch_lands_whole() {
    let (repo, raw, _c) = create_repo().await;

    let count = 5_000;
    let inputs: Vec<Envelope> = (0..count)
        .map(|i| envelope(&format!("bulk-{i:06}"), &format!("n{i}")))
        .collect();

    let written = repo.create_many(inputs.clone(), &star()).await.unwrap();

    assert_eq!(written.len(), count);
    assert_eq!(raw.count_documents(doc! {}).await.unwrap(), count as u64);
    for i in [0, count / 2, count - 1] {
        assert_eq!(written[i].id, inputs[i].id);
        assert!(repo
            .read(&inputs[i].id, &star(), None)
            .await
            .unwrap()
            .is_some());
    }
}

#[tokio::test]
async fn empty_batch_is_a_no_op() {
    let (repo, raw, _c) = create_repo().await;

    let written = repo.create_many(Vec::new(), &star()).await.unwrap();

    assert!(written.is_empty());
    assert_eq!(raw.count_documents(doc! {}).await.unwrap(), 0);
}

/// A batch the server rejects must come back as `Err` — including under an
/// unordered `insert_many`, which keeps going past the failing document rather
/// than aborting. Reporting it as a success would let the caller commit its
/// position past envelopes that were never stored: a permanent gap.
#[tokio::test]
async fn constraint_violation_is_reported_as_an_error() {
    let (repo, raw, _c) = create_repo().await;

    raw.create_index(
        IndexModel::builder()
            .keys(doc! { "id": 1 })
            .options(IndexOptions::builder().unique(true).build())
            .build(),
    )
    .await
    .unwrap();

    let inputs = vec![
        envelope("dup-a", "first"),
        envelope("dup-b", "second"),
        envelope("dup-a", "collides with the first"),
    ];

    let result = repo.create_many(inputs, &star()).await;

    assert!(
        result.is_err(),
        "a rejected batch was reported as a success: {result:?}"
    );
}

/// The whole point of the batch path: the same envelopes, stored the same way.
#[tokio::test]
async fn batch_and_repeated_single_writes_store_the_same_thing() {
    let node = shared_mongo().await;
    let batched = MongoRepository::new(&node.uri, DB, &fresh_collection(), Arc::new(NoAuth))
        .await
        .unwrap();
    let singly = MongoRepository::new(&node.uri, DB, &fresh_collection(), Arc::new(NoAuth))
        .await
        .unwrap();

    // Tokens deliberately unlike the envelopes' own, so the test would catch a
    // batch path that forgot to overwrite `authorized_tokens` the way `create`
    // does.
    let tokens = vec!["reader".to_string(), "writer".to_string()];
    let inputs: Vec<Envelope> = (0..10)
        .map(|i| envelope(&format!("eq-{i:02}"), &format!("payload-{i}")))
        .collect();

    batched.create_many(inputs.clone(), &tokens).await.unwrap();
    for env in inputs.clone() {
        singly.create(env, &tokens).await.unwrap();
    }

    let mut from_batch = batched.list(&tokens).await.unwrap();
    let mut from_singles = singly.list(&tokens).await.unwrap();
    from_batch.sort_by(|a, b| a.id.cmp(&b.id));
    from_singles.sort_by(|a, b| a.id.cmp(&b.id));

    assert_eq!(from_batch.len(), inputs.len());
    assert_eq!(from_batch.len(), from_singles.len());
    for (b, s) in from_batch.iter().zip(from_singles.iter()) {
        assert_eq!(b.id, s.id);
        assert_eq!(b.payload, s.payload);
        assert_eq!(b.deleted, s.deleted);
        assert_eq!(b.authorized_tokens, s.authorized_tokens);
        assert_eq!(
            b.created_at.timestamp_millis(),
            s.created_at.timestamp_millis()
        );
    }
}

/// `create` generates an id for an envelope that arrives without one; the batch
/// path has to do it too, and has to hand the generated id back.
#[tokio::test]
async fn batch_assigns_ids_to_envelopes_that_arrive_without_one() {
    let (repo, _raw, _c) = create_repo().await;

    let inputs: Vec<Envelope> = (0..3).map(|i| envelope("", &format!("anon-{i}"))).collect();

    let written = repo.create_many(inputs, &star()).await.unwrap();

    for env in &written {
        assert!(!env.id.is_empty(), "batch returned an unassigned id");
        assert!(
            repo.read(&env.id, &star(), None).await.unwrap().is_some(),
            "the id the batch returned does not name a stored document"
        );
    }
}

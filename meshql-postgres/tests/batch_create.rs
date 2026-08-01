//! `create_many` writes the batch in multi-row `INSERT`s. These tests hold it
//! to the contract that makes that safe: everything it reports as written is
//! readable, the order it returns is the order it was given, a failure is an
//! `Err`, and the result is indistinguishable from the same envelopes written
//! one at a time.

mod common;

use common::{fresh_table, shared_postgres};
use meshql_core::{Envelope, Repository, Stash};
use meshql_postgres::{PostgresRepository, MAX_ROWS_PER_INSERT};
use serde_json::json;

async fn create_repo() -> (PostgresRepository, impl std::any::Any) {
    let node = shared_postgres().await;
    let table = fresh_table();
    let repo = PostgresRepository::new_with_table(&node.url, &table)
        .await
        .unwrap();
    (repo, node)
}

fn star() -> Vec<String> {
    vec!["*".to_string()]
}

fn envelope(id: &str, name: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(name));
    Envelope::new(id, payload, star())
}

async fn row_count(repo: &PostgresRepository) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", repo.table))
        .fetch_one(&repo.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_round_trips_every_envelope_in_input_order() {
    let (repo, _c) = create_repo().await;

    let inputs: Vec<Envelope> = (0..25)
        .map(|i| envelope(&format!("batch-{i:03}"), &format!("name-{i}")))
        .collect();

    let written = repo.create_many(inputs.clone(), &star()).await.unwrap();

    assert_eq!(written.len(), inputs.len());
    for (returned, input) in written.iter().zip(inputs.iter()) {
        assert_eq!(returned.id, input.id, "returned out of input order");
        assert_eq!(returned.payload, input.payload);
    }

    // Every one is durable, with its contents intact.
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

/// A batch past `MAX_ROWS_PER_INSERT` has to be split: one statement binding
/// this many values would blow PostgreSQL's 16-bit parameter count and fail
/// outright. Sized from the constant the implementation chunks by, so it keeps
/// straddling the boundary if the column count ever changes.
#[tokio::test]
async fn batch_larger_than_one_statement_is_chunked() {
    let (repo, _c) = create_repo().await;

    let count = MAX_ROWS_PER_INSERT + 7;
    let inputs: Vec<Envelope> = (0..count)
        .map(|i| envelope(&format!("chunk-{i:06}"), &format!("n{i}")))
        .collect();

    let written = repo.create_many(inputs.clone(), &star()).await.unwrap();

    assert_eq!(written.len(), count);
    assert_eq!(row_count(&repo).await, count as i64);

    // Order survives the split, and so do the rows on either side of it.
    for i in [0, MAX_ROWS_PER_INSERT - 1, MAX_ROWS_PER_INSERT, count - 1] {
        assert_eq!(written[i].id, inputs[i].id);
        let stored = repo
            .read(&inputs[i].id, &star(), None)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("row {i} straddling the chunk boundary is missing"));
        assert_eq!(stored.payload, inputs[i].payload);
    }
}

#[tokio::test]
async fn empty_batch_is_a_no_op() {
    let (repo, _c) = create_repo().await;

    let written = repo.create_many(Vec::new(), &star()).await.unwrap();

    assert!(written.is_empty());
    assert_eq!(row_count(&repo).await, 0);
}

/// A batch the database rejects must come back as `Err`. Reporting it as a
/// success would let the caller commit its position past envelopes that were
/// never stored — a permanent gap.
#[tokio::test]
async fn constraint_violation_is_reported_as_an_error() {
    let (repo, _c) = create_repo().await;

    sqlx::query(&format!(
        "CREATE UNIQUE INDEX uniq_{0} ON {0}(id)",
        repo.table
    ))
    .execute(&repo.pool)
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
    let node = shared_postgres().await;
    let batched = PostgresRepository::new_with_table(&node.url, &fresh_table())
        .await
        .unwrap();
    let singly = PostgresRepository::new_with_table(&node.url, &fresh_table())
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
    let (repo, _c) = create_repo().await;

    let inputs: Vec<Envelope> = (0..3).map(|i| envelope("", &format!("anon-{i}"))).collect();

    let written = repo.create_many(inputs, &star()).await.unwrap();

    for env in &written {
        assert!(!env.id.is_empty(), "batch returned an unassigned id");
        let stored = repo.read(&env.id, &star(), None).await.unwrap();
        assert!(
            stored.is_some(),
            "the id the batch returned does not name a stored row"
        );
    }
}

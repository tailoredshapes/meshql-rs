//! The startup guards, exercised against a real table.
//!
//! Each of these refuses to open a table rather than opening one that would
//! serve wrong answers. They are worth their own suite because the failure they
//! prevent is *silence*: an index nobody writes to, or an index that cannot see
//! the history it was added to, both produce searches that return fewer records
//! than exist and no error at all.
//!
//! Requires DynamoDB Local at `MESHQL_DYNAMO_ENDPOINT` (default
//! `http://localhost:8123`).

mod common;

use common::{cert_config, client, fresh_table_name};
use meshql_core::{Envelope, Repository, Searcher, Stash};
use meshql_dynamo::{DynamoCollection, DynamoRepository, DynamoSearcher, IndexPlan};
use serde_json::json;

/// `expect_err` needs `Debug` on the success type and neither handle has it —
/// deliberately, since a `Debug` on a repository would print a client.
fn refusal<T>(result: meshql_core::Result<T>, why: &str) -> String {
    match result {
        Ok(_) => panic!("{why}"),
        Err(e) => e.to_string(),
    }
}

fn envelope(id: &str, kind: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("type".to_string(), json!(kind));
    payload.insert("name".to_string(), json!(id));
    Envelope::new(id, payload, vec!["*".to_string()])
}

/// The failure mode that motivates [`DynamoCollection`]: a repository with no
/// plan writes no promoted attributes, so an indexed searcher over the same
/// table finds nothing and says nothing. Opening is refused instead.
#[tokio::test]
async fn a_plain_repository_refuses_a_table_that_is_indexed() {
    let client = client().await;
    let table = fresh_table_name();

    let collection = DynamoCollection::open_with_client(client.clone(), &table, &cert_config())
        .await
        .expect("the indexed table");
    assert_eq!(collection.plan().len(), 2, "name and type");

    let message = refusal(
        DynamoRepository::new_with_client(client.clone(), &table).await,
        "a repository that promotes nothing must not open an indexed table",
    );
    assert!(
        message.contains("name") && message.contains("type"),
        "name the indexes: {message}"
    );

    // ...and the searcher half is refused for the same reason: a handle whose
    // plan disagrees with the table cannot be trusted about either.
    assert!(DynamoSearcher::new_with_client(client.clone(), &table)
        .await
        .is_err());

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

/// A plan that covers *some* of the table's indexes is still a disagreement.
#[tokio::test]
async fn a_partial_plan_refuses_a_more_indexed_table() {
    let client = client().await;
    let table = fresh_table_name();

    DynamoCollection::open_with_client(client.clone(), &table, &cert_config())
        .await
        .expect("the indexed table");

    let partial = IndexPlan::from_fields(["name"]).unwrap();
    let message = refusal(
        DynamoRepository::with_plan(client.clone(), &table, partial).await,
        "a handle that promotes only half the indexed fields must be refused",
    );
    assert!(message.contains("type"), "{message}");

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

/// An index added to a table that already holds data cannot see that data:
/// promotion happens on write, and the stored versions were written before the
/// field was indexed. Refusing to start is the only outcome that is not a
/// silently incomplete search.
///
/// This is the migration path a live deployment takes when it adds a query, so
/// the test walks the whole of it: refuse, migrate, open, and — the part that
/// matters — **find the historical record**.
#[tokio::test]
async fn indexing_a_populated_table_is_refused_until_it_is_migrated() {
    let client = client().await;
    let table = fresh_table_name();

    // A deployment that did not filter on anything yet.
    let repo = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("the plain table");
    repo.create(envelope("old", "typeA"), &["*".to_string()])
        .await
        .unwrap();

    // Now it adds `byType`. The index would not see "old".
    let message = refusal(
        DynamoCollection::open_with_client(client.clone(), &table, &cert_config()).await,
        "indexing a populated table must be refused, not silently half-done",
    );
    assert!(
        message.contains("migrate_indexes"),
        "the message must say what to run: {message}"
    );
    assert!(
        message.contains("holds data"),
        "the message must say why: {message}"
    );

    // The documented fix.
    let plan = IndexPlan::derive(&cert_config()).unwrap();
    let rewritten = meshql_dynamo::migrate_indexes(&client, &table, &plan)
        .await
        .expect("migration");
    assert_eq!(rewritten, 1, "one stored version to promote");

    // ...and now the historical record is findable through the index it
    // predates. Without the migration this assertion is what would have been
    // silently false.
    let collection = DynamoCollection::open_with_client(client.clone(), &table, &cert_config())
        .await
        .expect("open after migrating");
    let now = chrono::Utc::now().timestamp_millis();
    let found = collection
        .searcher
        .find_all(
            r#"{"payload.type": "typeA"}"#,
            &Stash::new(),
            &["*".to_string()],
            now,
        )
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["old"],
        "the record written before the index existed must be findable after \
         migrating; got {ids:?}"
    );

    // Migrating again rewrites nothing: the promoted attributes are already
    // there, so a restart-and-migrate loop cannot cost a second O(V) pass.
    assert_eq!(
        meshql_dynamo::migrate_indexes(&client, &table, &plan)
            .await
            .unwrap(),
        0
    );

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

/// An *empty* table has no history to miss, so an index can simply be added.
/// This is what makes "create the table on first boot, add a query later before
/// anything is written" work without ceremony.
#[tokio::test]
async fn indexing_an_empty_table_needs_no_migration() {
    let client = client().await;
    let table = fresh_table_name();

    DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("the plain table");

    let collection = DynamoCollection::open_with_client(client.clone(), &table, &cert_config())
        .await
        .expect("an empty table can be indexed in place");

    // And it works: a record written after the index exists is found through it.
    collection
        .repository
        .create(envelope("fresh", "typeA"), &["*".to_string()])
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let found = collection
        .searcher
        .find_all(
            r#"{"payload.type": "typeA"}"#,
            &Stash::new(),
            &["*".to_string()],
            now,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "got {found:?}");

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

/// Opening the same indexed table twice is the ordinary case — every restart,
/// and every second process — and must not try to create the indexes again.
#[tokio::test]
async fn reopening_an_indexed_table_is_idempotent() {
    let client = client().await;
    let table = fresh_table_name();

    for _ in 0..3 {
        DynamoCollection::open_with_client(client.clone(), &table, &cert_config())
            .await
            .expect("reopening an indexed table");
    }

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

/// A GSI that is not this crate's is left alone: a client may have their own
/// index on the table and it is not ours to police.
#[tokio::test]
async fn an_index_this_crate_does_not_manage_is_ignored() {
    use aws_sdk_dynamodb::types::{
        AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType,
        Projection, ProjectionType, ScalarAttributeType,
    };

    let client = client().await;
    let table = fresh_table_name();

    let attr = |name: &str| {
        AttributeDefinition::builder()
            .attribute_name(name)
            .attribute_type(ScalarAttributeType::S)
            .build()
            .unwrap()
    };
    let key = |name: &str, kind: KeyType| {
        KeySchemaElement::builder()
            .attribute_name(name)
            .key_type(kind)
            .build()
            .unwrap()
    };

    client
        .create_table()
        .table_name(&table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key("pk", KeyType::Hash))
        .key_schema(key("sk", KeyType::Range))
        .attribute_definitions(attr("pk"))
        .attribute_definitions(attr("sk"))
        .attribute_definitions(attr("theirs"))
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name("someone-elses-index")
                .key_schema(key("theirs", KeyType::Hash))
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::KeysOnly)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .send()
        .await
        .expect("a table with a foreign index");

    DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("an index we do not manage is none of our business");

    let _ = meshql_dynamo::drop_table(&client, &table).await;
}

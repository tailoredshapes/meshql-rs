//! The create half of the repository certification, plus the assertion that
//! every read path is a hard failure.
//!
//! Run against the in-memory backend, because what is being certified here is
//! the adapter's behaviour, not the S3 binding. `tests/live_create_cert.rs` does
//! the same against a real directory bucket.

use merk_object::broker::{BrokerConfig, BrokerRef};
use merk_object::mem::broker::Broker;
use merk_object::memory::MemoryBackend;
use meshql_core::{Envelope, Repository, Stash};
use meshql_merk::repository::READ_REFUSED;
use meshql_merk::{MerkRepository, TopicPlan};
use serde_json::json;

fn star() -> Vec<String> {
    vec!["*".to_string()]
}

fn open(location: &str) -> BrokerRef<MemoryBackend> {
    Broker::open(BrokerConfig::new(location).with_auto_create_topics(false)).unwrap()
}

fn plan() -> TopicPlan {
    TopicPlan::from_toml_str("[[topic]]\nname=\"story_event\"\npartitions=8\n").unwrap()
}

fn repo(location: &str) -> MerkRepository<MemoryBackend> {
    let (repo, _broker) = repo_and_broker(location);
    repo
}

/// Some tests need the broker as well, to ask the topic where a key routes.
fn repo_and_broker(location: &str) -> (MerkRepository<MemoryBackend>, BrokerRef<MemoryBackend>) {
    let broker = open(location);
    meshql_merk::provision(&broker, &plan()).unwrap();
    let repo = MerkRepository::new(&broker, "story_event");
    (repo, broker)
}

fn envelope(id: &str, body: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("type".into(), json!("story_created"));
    payload.insert("body".into(), json!(body));
    Envelope::new(id, payload, star())
}

#[tokio::test]
async fn create_stores_and_returns_the_envelope() {
    let repo = repo("mem://merk-create");
    let result = repo
        .create(envelope("id-1", "hello"), &star())
        .await
        .unwrap();

    assert_eq!(result.id, "id-1");
    assert!(!result.deleted);
    assert_eq!(result.payload.get("body").unwrap(), &json!("hello"));
}

#[tokio::test]
async fn create_mints_an_id_when_the_caller_supplies_none() {
    let repo = repo("mem://merk-create-id");
    let result = repo.create(envelope("", "anon"), &star()).await.unwrap();
    assert!(!result.id.is_empty());
    assert_eq!(result.id.len(), 36, "a uuid: {}", result.id);
}

#[tokio::test]
async fn create_stamps_the_callers_tokens() {
    let repo = repo("mem://merk-create-tokens");
    let tokens = vec![
        "public".to_string(),
        "account:a_4d2e".to_string(),
        "role:moderator".to_string(),
    ];
    // The envelope arrives carrying something else entirely; the caller's
    // resolved tokens are what gets stored.
    let mut incoming = envelope("id-tok", "x");
    incoming.authorized_tokens = vec!["whatever-the-client-sent".to_string()];

    let stored = repo.create(incoming, &tokens).await.unwrap();
    assert_eq!(stored.authorized_tokens, tokens);
}

#[tokio::test]
async fn create_many_stores_every_envelope() {
    let repo = repo("mem://merk-create-many");
    let batch: Vec<Envelope> = (0..25)
        .map(|i| envelope(&format!("bulk-{i}"), &format!("body-{i}")))
        .collect();

    let stored = repo.create_many(batch, &star()).await.unwrap();
    assert_eq!(stored.len(), 25);
    for (i, env) in stored.iter().enumerate() {
        assert_eq!(env.id, format!("bulk-{i}"));
        assert_eq!(env.authorized_tokens, star());
    }
}

#[tokio::test]
async fn create_many_of_nothing_is_not_an_error() {
    let repo = repo("mem://merk-create-many-empty");
    assert!(repo.create_many(vec![], &star()).await.unwrap().is_empty());
}

/// The gateway's step 7 is append, then notify, then 201 — and the notify needs
/// the partition, which `Repository::create` cannot return.
///
/// The assertion is that the partition the engine assigned is the one
/// `Topic::route` computes for the same key. Not because the gateway should
/// re-derive it — it should not, and `create_located` exists so it does not have
/// to — but because if the two ever disagreed, a gateway that *did* re-derive
/// would send every wake-up to the wrong worker, and nothing would log a word
/// about it.
#[tokio::test]
async fn create_located_reports_the_partition_the_engine_chose() {
    let (repo, broker) = repo_and_broker("mem://merk-located");
    let topic = broker.topic("story_event").unwrap();

    for i in 0..20 {
        let id = format!("e-{i:02}");
        let (env, notification) = repo
            .create_located(envelope(&id, "body"), &star())
            .await
            .unwrap();

        assert_eq!(env.id, id);
        assert_eq!(notification.topic, "story_event");
        assert_eq!(
            notification.partition,
            topic.route(&Some(id.clone())),
            "the engine and Topic::route disagree about where {id} belongs"
        );
        assert!(notification.partition < 8);
    }
}

/// A batch gets one wake-up per partition touched, not one per record — 200
/// records over 8 partitions is 8 messages.
#[tokio::test]
async fn create_many_located_notifies_each_partition_once() {
    let (repo, _broker) = repo_and_broker("mem://merk-located-many");
    let batch: Vec<Envelope> = (0..200)
        .map(|i| envelope(&uuid::Uuid::new_v4().to_string(), &format!("b{i}")))
        .collect();

    let (stored, notifications) = repo.create_many_located(batch, &star()).await.unwrap();
    assert_eq!(stored.len(), 200);

    assert!(
        !notifications.is_empty() && notifications.len() <= 8,
        "expected at most one notification per partition, got {}",
        notifications.len()
    );

    let mut partitions: Vec<u32> = notifications.iter().map(|n| n.partition).collect();
    let before = partitions.len();
    partitions.sort_unstable();
    partitions.dedup();
    assert_eq!(partitions.len(), before, "a partition was notified twice");
    assert!(
        notifications.iter().all(|n| n.topic == "story_event"),
        "a notification named the wrong topic"
    );
}

/// The whole point of the crate. Every read path fails loudly, and says why.
#[tokio::test]
async fn every_read_path_is_refused() {
    let repo = repo("mem://merk-refusals");
    repo.create(envelope("present", "here"), &star())
        .await
        .unwrap();

    let ids = vec!["present".to_string()];

    let errors = [
        (
            "read",
            repo.read("present", &star(), None)
                .await
                .err()
                .map(|e| e.to_string()),
        ),
        (
            "read at:",
            repo.read("present", &star(), Some(chrono::Utc::now()))
                .await
                .err()
                .map(|e| e.to_string()),
        ),
        (
            "list",
            repo.list(&star()).await.err().map(|e| e.to_string()),
        ),
        (
            "read_many",
            repo.read_many(&ids, &star())
                .await
                .err()
                .map(|e| e.to_string()),
        ),
        (
            "remove",
            repo.remove("present", &star())
                .await
                .err()
                .map(|e| e.to_string()),
        ),
        (
            "remove_many",
            repo.remove_many(&ids, &star())
                .await
                .err()
                .map(|e| e.to_string()),
        ),
    ];

    for (path, error) in &errors {
        let message = error
            .as_ref()
            .unwrap_or_else(|| panic!("{path} returned Ok — it must refuse, not scan"));
        assert!(
            message.contains(READ_REFUSED),
            "{path} refused without explaining why: {message}"
        );
    }
}

/// A refusal that is not reachable is not a refusal. `remove` in particular is
/// the one a reviewer might "fix" by making it a tombstone append, so its
/// message says both reasons it is refused.
#[tokio::test]
async fn remove_is_refused_by_name_not_by_returning_false() {
    let repo = repo("mem://merk-remove");
    repo.create(envelope("gone", "x"), &star()).await.unwrap();
    let error = repo.remove("gone", &star()).await.expect_err("must refuse");
    assert!(error.to_string().starts_with("Storage error: remove:"));
}

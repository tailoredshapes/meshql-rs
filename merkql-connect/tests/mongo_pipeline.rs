//! `mongodb → merkql`, end to end, against a real single-node replica set.
//!
//! Change streams do not exist on a standalone `mongod`, so this must be a
//! replica set. The connector is otherwise identical to the SQLite one from a
//! consumer's point of view, which is what the shared certification asserts.

use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use merkql_connect::cert::{self, CertStore};
use merkql_connect::mongo::MongoSource;
use merkql_connect::{
    run_connector, CdcError, ChangeRecord, CommitSource, OffsetStore, Op, Resume, SnapshotMode,
    TopicWriter,
};
use meshql_core::{Envelope, NoAuth, Repository, Stash};
use meshql_mongo::MongoRepository;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::mongo::Mongo;

/// A single-node replica set, initiated and ready for change streams.
///
/// `repl_set()` starts `mongod --replSet rs`, but a replica set is not usable
/// until `replSetInitiate` runs and a primary is elected — until then every
/// `watch()` fails. Doing that here rather than assuming the image does it is
/// the difference between a flaky suite and a deterministic one.
async fn replica_set() -> (ContainerAsync<Mongo>, String) {
    let container = Mongo::repl_set().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true");

    let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
    // Idempotent: an already-initiated set answers AlreadyInitialized.
    let _ = client
        .database("admin")
        .run_command(bson::doc! {
            "replSetInitiate": {
                "_id": "rs",
                "members": [ { "_id": 0, "host": format!("127.0.0.1:{port}") } ]
            }
        })
        .await;

    // Wait for a primary; `hello.isWritablePrimary` is the readiness signal.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(reply) = client
            .database("admin")
            .run_command(bson::doc! { "hello": 1 })
            .await
        {
            if reply.get_bool("isWritablePrimary").unwrap_or(false) {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replica set never elected a primary"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    (container, uri)
}

fn broker(dir: &Path) -> BrokerRef {
    Broker::open(BrokerConfig::new(dir.join("merkql"))).unwrap()
}

fn consume(broker: &BrokerRef, topic: &str) -> Vec<ChangeRecord> {
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

struct MongoCert {
    uri: String,
    database: String,
    collection: String,
    repo: Arc<MongoRepository>,
    _container: Arc<ContainerAsync<Mongo>>,
}

#[async_trait::async_trait]
impl CertStore for MongoCert {
    async fn write(&self, envelope: Envelope) -> anyhow::Result<()> {
        self.repo
            .create(
                envelope,
                &meshql_core::TokenSession::new(vec!["cert".to_string()]),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(())
    }

    async fn source(&self) -> anyhow::Result<Box<dyn CommitSource>> {
        Ok(Box::new(
            MongoSource::open(&self.uri, &self.database, &self.collection, "cert")
                .await
                .map_err(anyhow::Error::from)?,
        ))
    }
}

/// One replica set, one fresh collection per test — a shared collection would
/// let one test's writes satisfy another's assertions.
async fn mongo_cert(container: Arc<ContainerAsync<Mongo>>, uri: &str) -> MongoCert {
    let collection = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let repo = Arc::new(
        MongoRepository::new(uri, "cdc_cert", &collection, Arc::new(NoAuth))
            .await
            .unwrap(),
    );
    // The collection must exist before `watch()`; an insert creates it.
    MongoCert {
        uri: uri.to_string(),
        database: "cdc_cert".to_string(),
        collection,
        repo,
        _container: container,
    }
}

/// One container for the whole file: starting a replica set is slow, and each
/// test isolates itself with its own collection instead.
#[tokio::test]
async fn mongo_cdc_end_to_end_and_certified() {
    let (container, uri) = replica_set().await;
    let container = Arc::new(container);

    // ── the shared certification: the same contract SQLite passes ──
    cert::certify_snapshot_then_stream(&mongo_cert(container.clone(), &uri).await)
        .await
        .expect("snapshot-then-stream");
    cert::certify_positions_are_present_and_distinct(&mongo_cert(container.clone(), &uri).await)
        .await
        .expect("native positions");
    cert::certify_resume_delivers_only_what_follows(&mongo_cert(container.clone(), &uri).await)
        .await
        .expect("resume");
    cert::certify_never_mode_skips_history(&mongo_cert(container.clone(), &uri).await)
        .await
        .expect("snapshot_mode = never");

    // ── the chain: a committed write reaches the merkql topic ──
    let dir = tempfile::tempdir().unwrap();
    let store = mongo_cert(container.clone(), &uri).await;
    let merk = broker(dir.path());
    let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path()).unwrap();
    let source = MongoSource::open(&store.uri, &store.database, &store.collection, "lay_report")
        .await
        .unwrap();
    let mut offsets = OffsetStore::open(
        dir.path().join("o.json"),
        "mongodb",
        "lay_report",
        Duration::from_millis(0),
    )
    .unwrap();

    let connector = tokio::spawn(async move {
        let _ = run_connector(&source, &writer, &mut offsets, SnapshotMode::Initial).await;
    });

    let mut payload = Stash::new();
    payload.insert("hen_id".to_string(), json!("henrietta"));
    payload.insert("eggs".to_string(), json!(3));
    store
        .write(Envelope::new("evt-1", payload, vec!["farm".to_string()]))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let records = loop {
        let records = consume(&merk, "lay_report");
        if !records.is_empty() {
            break records;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the mongo connector never delivered the committed write"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert_eq!(records[0].op, Op::Create);
    assert_eq!(records[0].source.connector, "mongodb");
    assert_eq!(records[0].key().as_deref(), Some("evt-1"));
    assert_eq!(
        records[0].after.as_ref().unwrap().payload["eggs"],
        json!(3),
        "the committed payload must survive onto the topic"
    );
    // The position is the server's resume token, not something we invented.
    let position = records[0].position().expect("a native position");
    assert!(
        position.contains("_data"),
        "the position must be MongoDB's own resume token, got {position}"
    );

    connector.abort();
}

/// A resume token the server will not honour must be reported, never turned
/// into a silent restart at the live tail.
#[tokio::test]
async fn a_bogus_resume_token_is_reported_as_unusable() {
    let (container, uri) = replica_set().await;
    let store = mongo_cert(Arc::new(container), &uri).await;
    store
        .write(Envelope::new("a", Stash::new(), vec![]))
        .await
        .unwrap();
    let source = store.source().await.unwrap();

    // Not a resume token at all.
    let err = match source
        .changes(Resume::At("not-a-token".into()), SnapshotMode::Initial)
        .await
    {
        Ok(_) => panic!("a malformed resume token must not be honoured"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CdcError::UnusablePosition { .. }),
        "must be UnusablePosition, got: {err}"
    );

    // Well-formed but naming a position the oplog has never held.
    let err = match source
        .changes(
            Resume::At(r#"{"_data":"82000000000000000000000000000000000000000000000000"}"#.into()),
            SnapshotMode::Initial,
        )
        .await
    {
        Ok(_) => panic!("a resume token the server refuses must not be honoured"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CdcError::UnusablePosition { .. }),
        "must be UnusablePosition, got: {err}"
    );

    // The control: a cold start on the same source works, so the rejections
    // above cannot be passing because the source is simply broken.
    assert!(source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .is_ok());
}

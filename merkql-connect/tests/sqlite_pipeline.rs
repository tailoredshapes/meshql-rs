//! The whole `sqlite3 → merkql` topology, end to end, with nothing stubbed.
//!
//! ```text
//! POST /lay_report/api        (real restlette, real axum router)
//!   -> SQLite commit          (real file-backed database, WAL mode)
//!   -> merkql-connect         (real inotify feed, real rowid cursor)
//!   -> merkql topic           (real broker, tempdir-backed)
//!   -> consumer               (real merkql consumer)
//!   -> worker fold            (real projector)
//!   -> POST /hen_productivity/api  (real projection restlette)
//!   -> GET  /hen_productivity/api  (proving the projection landed)
//! ```
//!
//! The connector runs as its own task here rather than its own process, which
//! is the one concession the test makes; everything it touches — the database
//! file, the inotify watch, the writer lock, the offset file — is exactly what
//! the deployed binary touches, and the writer-lock test in `sink.rs` covers
//! the second-process case directly.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use merkql_connect::cert::{self, CertStore};
use merkql_connect::sqlite::SqliteSource;
use merkql_connect::{
    run_connector, ChangeRecord, CommitSource, OffsetStore, Op, Resume, SnapshotMode, TopicWriter,
};
use meshql_core::{Envelope, NoAuth, Repository, Stash};
use meshql_restlette::build_restlette_router;
use meshql_sqlite::SqliteRepository;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// A file-backed SQLite pool in WAL mode.
///
/// WAL specifically, and a *file* specifically: the connector watches the
/// database's directory for changes, and an in-memory database has neither a
/// file to watch nor a way for a second connection to see it. This is the
/// deployed shape, not a test convenience.
async fn pool(path: &Path) -> sqlx::SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .unwrap()
}

fn broker(dir: &Path) -> BrokerRef {
    Broker::open(BrokerConfig::new(dir.join("merkql"))).unwrap()
}

/// Read every `ChangeRecord` currently on the topic — a real merkql consumer,
/// which is what a worker uses.
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
        .map(|r| serde_json::from_str(&r.value).expect("a merkql record is a ChangeRecord"))
        .collect()
}

async fn post(
    app: &Router,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

async fn get(app: &Router, path: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Wait for `f` to hold, or fail with what was actually there.
async fn eventually<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ── The chain ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_post_to_a_sqlite_restlette_reaches_a_projection_through_merkql() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lay_report.db");
    let projection_db = dir.path().join("hen_productivity.db");

    // ── the meshql service: an event restlette over SQLite ──
    let event_repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&db).await)
            .await
            .unwrap(),
    );
    let events: Router = build_restlette_router(
        "/lay_report/api",
        event_repo.clone() as Arc<dyn Repository>,
        Arc::new(NoAuth),
    );

    // ── the projection restlette, written only by the worker ──
    let projection_repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&projection_db).await)
            .await
            .unwrap(),
    );
    let projections: Router = build_restlette_router(
        "/hen_productivity/api",
        projection_repo.clone() as Arc<dyn Repository>,
        Arc::new(NoAuth),
    );

    // ── merkql-connect: a separate deployable, here a separate task ──
    let merk = broker(dir.path());
    let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path()).unwrap();
    let source = SqliteSource::open(&db, "envelopes", "lay_report")
        .await
        .expect("the connector opens the same database file the service writes");
    let mut offsets = OffsetStore::open(
        dir.path().join("lay_report.offsets.json"),
        "sqlite",
        "lay_report",
        Duration::from_millis(0),
    )
    .unwrap();

    // ── 1. the only write anyone makes: POST the business event ──
    //
    // The first event is POSTed BEFORE the connector starts and the rest
    // AFTER it has demonstrably finished snapshotting. That ordering is
    // deliberate: spawning the connector and immediately POSTing races its
    // snapshot query, and which phase captures the write then depends on load
    // — this test failed exactly that way under a loaded box. Sequencing it
    // properly turns the race into coverage: event 1 exercises the snapshot,
    // events 2 and 3 exercise the live stream, and the handover between them
    // is what the assertions below pin.
    let (status, _) = post(
        &events,
        "/lay_report/api",
        json!({"hen_id": "henrietta", "eggs": 3}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "the event must commit");

    let connector = tokio::spawn(async move {
        let _ = run_connector(&source, &writer, &mut offsets, SnapshotMode::Initial).await;
    });

    // The snapshot is complete once its final record lands, and it says so:
    // `Snapshot::Last` is the connector's own statement that it has covered
    // everything that existed when it started.
    let snapshot = eventually("the snapshot to complete", || async {
        let records = consume(&merk, "lay_report");
        records
            .iter()
            .any(|r| r.source.snapshot == merkql_connect::Snapshot::Last)
            .then_some(records)
    })
    .await;
    assert_eq!(snapshot.len(), 1);
    assert_eq!(
        snapshot[0].op,
        Op::Read,
        "history that predates the connector must arrive as a snapshot read"
    );

    // ── 2. writes after the handover arrive as live creates ──
    for (hen, eggs) in [("clucky", 2), ("henrietta", 4)] {
        let (status, _) = post(
            &events,
            "/lay_report/api",
            json!({"hen_id": hen, "eggs": eggs}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let records = eventually("three records on the merkql topic", || async {
        let records = consume(&merk, "lay_report");
        (records.len() >= 3).then_some(records)
    })
    .await;
    assert_eq!(
        records.len(),
        3,
        "no gap and no duplicate across the handover"
    );
    assert!(
        records[1..]
            .iter()
            .all(|r| r.op == Op::Create && !r.source.snapshot.is_snapshot()),
        "writes after the snapshot must arrive as live creates: {:?}",
        records
            .iter()
            .map(|r| (r.op, r.source.snapshot))
            .collect::<Vec<_>>()
    );
    // Every record names its native SQLite position.
    assert!(records.iter().all(|r| r.position().is_some()));

    // ── 3. a worker folds the log and writes the projection ──
    // Deliberately folded from the WHOLE log, from offset zero — which is what
    // makes a projection safe to drop and rebuild.
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for record in &records {
        let payload = &record.after.as_ref().unwrap().payload;
        let hen = payload["hen_id"].as_str().unwrap().to_string();
        *totals.entry(hen).or_default() += payload["eggs"].as_i64().unwrap();
    }
    for (hen, eggs) in &totals {
        let (status, _) = post(
            &projections,
            "/hen_productivity/api",
            json!({"hen_id": hen, "total_eggs": eggs}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // ── 4. the projection is visible through its own restlette ──
    let listed = get(&projections, "/hen_productivity/api").await;
    let rows = listed.as_array().unwrap();
    let by_hen: BTreeMap<&str, i64> = rows
        .iter()
        .map(|r| {
            (
                r["hen_id"].as_str().unwrap(),
                r["total_eggs"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        by_hen,
        BTreeMap::from([("henrietta", 7), ("clucky", 2)]),
        "the projection must reflect every event that was POSTed"
    );

    connector.abort();
}

/// A projection built long after the events happened, from the beginning of
/// the log — the replay payoff, and the reason `op: r` exists.
#[tokio::test]
async fn a_connector_started_after_the_writes_snapshots_them_as_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lay_report.db");

    let repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&db).await)
            .await
            .unwrap(),
    );
    let events: Router = build_restlette_router(
        "/lay_report/api",
        repo.clone() as Arc<dyn Repository>,
        Arc::new(NoAuth),
    );
    for eggs in [1, 2] {
        let (status, _) = post(&events, "/lay_report/api", json!({"eggs": eggs})).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // Only now does the connector exist.
    let merk = broker(dir.path());
    let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path()).unwrap();
    let source = SqliteSource::open(&db, "envelopes", "lay_report")
        .await
        .unwrap();
    let mut offsets = OffsetStore::open(
        dir.path().join("o.json"),
        "sqlite",
        "lay_report",
        Duration::from_millis(0),
    )
    .unwrap();
    let connector = tokio::spawn(async move {
        let _ = run_connector(&source, &writer, &mut offsets, SnapshotMode::Initial).await;
    });

    let records = eventually("the snapshot to reach the topic", || async {
        let records = consume(&merk, "lay_report");
        (records.len() >= 2).then_some(records)
    })
    .await;

    assert!(
        records
            .iter()
            .all(|r| r.op == Op::Read && r.source.snapshot.is_snapshot()),
        "history captured at startup must be marked as a snapshot read, so a \
         consumer can tell backfill from live traffic: {:?}",
        records
            .iter()
            .map(|r| (r.op, r.source.snapshot))
            .collect::<Vec<_>>()
    );

    connector.abort();
}

/// A restart resumes from the committed offset and does not replay the log.
#[tokio::test]
async fn a_restart_resumes_from_the_committed_offset() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lay_report.db");
    let repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&db).await)
            .await
            .unwrap(),
    );
    let events: Router = build_restlette_router(
        "/lay_report/api",
        repo.clone() as Arc<dyn Repository>,
        Arc::new(NoAuth),
    );

    let merk = broker(dir.path());
    let offsets_path = dir.path().join("o.json");

    // First run: one event, connector captures it, then the connector stops.
    {
        let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path()).unwrap();
        let source = SqliteSource::open(&db, "envelopes", "lay_report")
            .await
            .unwrap();
        let mut offsets = OffsetStore::open(
            &offsets_path,
            "sqlite",
            "lay_report",
            Duration::from_millis(0),
        )
        .unwrap();

        post(&events, "/lay_report/api", json!({"eggs": 1})).await;

        // Drive the connector directly so the stop point is deterministic —
        // a spawned task aborted on a timer would make this test racy about
        // exactly what it had committed.
        let mut stream = source
            .changes(offsets.resume(), SnapshotMode::Initial)
            .await
            .unwrap();
        let first = cert::take(&mut stream, 1, Duration::from_secs(20))
            .await
            .unwrap();
        writer.append(&first[0]).unwrap();
        offsets.stage(
            first[0].position().unwrap(),
            first[0].source.snapshot.in_progress(),
        );
        offsets.commit_now().unwrap();
    }

    // While nothing is watching.
    post(&events, "/lay_report/api", json!({"eggs": 2})).await;

    // Second run: same offset file, new writer.
    {
        let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path())
            .expect("the previous writer released its lock");
        let source = SqliteSource::open(&db, "envelopes", "lay_report")
            .await
            .unwrap();
        let mut offsets = OffsetStore::open(
            &offsets_path,
            "sqlite",
            "lay_report",
            Duration::from_millis(0),
        )
        .unwrap();
        assert!(
            matches!(offsets.resume(), Resume::At(_)),
            "the first run must have committed a position"
        );
        let mut stream = source
            .changes(offsets.resume(), SnapshotMode::Initial)
            .await
            .unwrap();
        let next = cert::take(&mut stream, 1, Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!(
            next[0].after.as_ref().unwrap().payload["eggs"],
            json!(2),
            "a restart must deliver the write that happened while it was down, \
             and must not replay the one it already published"
        );
        writer.append(&next[0]).unwrap();
        offsets.stage(next[0].position().unwrap(), false);
        offsets.commit_now().unwrap();
    }

    let on_topic = consume(&merk, "lay_report");
    assert_eq!(
        on_topic.len(),
        2,
        "exactly two records — no gap, and no replay of the first"
    );
}

// ── The certification ───────────────────────────────────────────────────

struct SqliteCert {
    db: std::path::PathBuf,
    repo: Arc<SqliteRepository>,
}

#[async_trait::async_trait]
impl CertStore for SqliteCert {
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
            SqliteSource::open(&self.db, "envelopes", "cert")
                .await
                .map_err(anyhow::Error::from)?,
        ))
    }
}

async fn sqlite_cert() -> SqliteCert {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let db = dir.path().join("cert.db");
    let repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&db).await)
            .await
            .unwrap(),
    );
    SqliteCert { db, repo }
}

#[tokio::test]
async fn sqlite_certifies_snapshot_then_stream() {
    cert::certify_snapshot_then_stream(&sqlite_cert().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_certifies_native_positions() {
    cert::certify_positions_are_present_and_distinct(&sqlite_cert().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_certifies_resume() {
    cert::certify_resume_delivers_only_what_follows(&sqlite_cert().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_certifies_never_mode() {
    cert::certify_never_mode_skips_history(&sqlite_cert().await)
        .await
        .unwrap();
}

// ── Guards ──────────────────────────────────────────────────────────────

/// A stored position past the end of the table means the database was rebuilt
/// or restored. Resuming from it would wait forever for rows that will never
/// come; restarting silently from zero or from the tail would double-publish
/// or skip. It must be reported, and the snapshot-mode policy must decide.
#[tokio::test]
async fn a_position_past_the_end_of_the_table_is_reported_as_unusable() {
    let cert = sqlite_cert().await;
    cert.write(Envelope::new("a", Stash::new(), vec![]))
        .await
        .unwrap();
    let source = cert.source().await.unwrap();

    let err = match source
        .changes(Resume::At("9999".into()), SnapshotMode::Initial)
        .await
    {
        Ok(_) => panic!("a position past the end of the table must not be honoured"),
        Err(e) => e,
    };
    assert!(
        matches!(err, merkql_connect::CdcError::UnusablePosition { .. }),
        "must be UnusablePosition, got: {err}"
    );

    // The control: an in-range position IS honoured, so the rejection above
    // cannot be passing because every position is refused.
    assert!(source
        .changes(Resume::At("1".into()), SnapshotMode::Initial)
        .await
        .is_ok());
}

/// A garbage position is unusable, not silently treated as zero.
#[tokio::test]
async fn a_malformed_position_is_reported_as_unusable() {
    let cert = sqlite_cert().await;
    cert.write(Envelope::new("a", Stash::new(), vec![]))
        .await
        .unwrap();
    let source = cert.source().await.unwrap();

    for bad in ["", "abc", "0x10", "1:2"] {
        let err = match source
            .changes(Resume::At(bad.into()), SnapshotMode::Initial)
            .await
        {
            Ok(_) => panic!("the malformed position {bad:?} must not be honoured"),
            Err(e) => e,
        };
        assert!(
            matches!(err, merkql_connect::CdcError::UnusablePosition { .. }),
            "{bad:?} got: {err}"
        );
    }
}

/// An interrupted snapshot resumes where it stopped instead of restarting.
///
/// This is the behaviour `Resume::Snapshotting` exists to make possible. Before
/// it, `OffsetStore::resume` discarded a mid-snapshot position and returned
/// `Cold`, so a backfill killed at 90% re-emitted everything — seconds for a
/// table, hours for an enterprise SaaS backfill. The safety property is
/// unchanged: the position is still never treated as a *streaming* position.
#[tokio::test]
async fn an_interrupted_sqlite_snapshot_resumes_rather_than_restarting() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("resume.db");
    let repo = Arc::new(
        SqliteRepository::new_with_pool(pool(&db).await)
            .await
            .unwrap(),
    );

    for i in 1..=5 {
        let mut payload = meshql_core::Stash::new();
        payload.insert("eggs".to_string(), json!(i));
        repo.create(
            Envelope::new(format!("hen-{i}"), payload, vec!["farm".to_string()]),
            &meshql_core::TokenSession::new(vec!["farm".to_string()]),
        )
        .await
        .unwrap();
    }

    // Take the first two records of the snapshot, then "crash".
    let source = SqliteSource::open(&db, "envelopes", "hen").await.unwrap();
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let first = cert::take(&mut stream, 2, std::time::Duration::from_secs(20))
        .await
        .unwrap();
    let interrupted_at = first[1].position().unwrap().to_string();
    assert!(
        first.iter().all(|r| r.source.snapshot.in_progress()),
        "both records must be flagged mid-snapshot for this test to mean anything"
    );
    drop(stream);

    // Resume from exactly that position, as the offset store would offer it.
    let resumed = SqliteSource::open(&db, "envelopes", "hen").await.unwrap();
    let mut stream = resumed
        .changes(
            Resume::Snapshotting(interrupted_at.clone()),
            SnapshotMode::Initial,
        )
        .await
        .unwrap();
    let rest = cert::take(&mut stream, 3, std::time::Duration::from_secs(20))
        .await
        .unwrap();

    let ids: Vec<String> = rest.iter().filter_map(|r| r.key()).collect();
    assert_eq!(
        ids,
        vec!["hen-3", "hen-4", "hen-5"],
        "the resumed snapshot must deliver only what had not been emitted"
    );
    assert!(
        rest.iter().all(|r| r.op == merkql_connect::Op::Read),
        "resumed snapshot records are still snapshot reads, not live creates"
    );

    // And the historical behaviour is still one call away.
    let restarted = SqliteSource::open(&db, "envelopes", "hen").await.unwrap();
    let mut stream = restarted
        .changes(
            Resume::Snapshotting(interrupted_at).without_snapshot_resume(),
            SnapshotMode::Initial,
        )
        .await
        .unwrap();
    let all = cert::take(&mut stream, 5, std::time::Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(all.len(), 5, "opting out must re-emit the whole snapshot");
}

//! `postgresql → merkql`, end to end, against a real server running with
//! `wal_level = logical`.
//!
//! The connector uses a real logical replication slot for the data and a
//! trigger-emitted `NOTIFY` purely as the wake-up edge — see the `postgres`
//! module docs for why that is *not* the known-lossy `LISTEN`/`NOTIFY`
//! pattern. It passes the same `src/cert.rs` contract as SQLite and MongoDB,
//! which is what makes the three interchangeable to a consumer.

use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use merkql_connect::cert::{self, CertStore};
use merkql_connect::postgres::PostgresSource;
use merkql_connect::{
    run_connector, CdcError, ChangeRecord, CommitSource, OffsetStore, Op, Resume, SnapshotMode,
    TopicWriter,
};
use meshql_core::{Envelope, Repository, Stash};
use meshql_postgres::PostgresRepository;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// A server with logical decoding enabled.
///
/// `wal_level` is not runtime-settable, so it has to be a server command-line
/// argument — the stock image ships `replica`, on which every slot creation
/// fails. `max_replication_slots` is raised because each test in this file
/// takes its own slot and the default (10 on modern versions, but small on
/// old ones) is easy to exhaust.
async fn logical_postgres() -> (ContainerAsync<Postgres>, String) {
    let container = Postgres::default()
        .with_cmd([
            "postgres",
            "-c",
            "wal_level=logical",
            "-c",
            "max_replication_slots=40",
            "-c",
            "max_wal_senders=40",
        ])
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432.tcp()).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    (container, url)
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

struct PgCert {
    url: String,
    table: String,
    slot: String,
    publication: String,
    repo: Arc<PostgresRepository>,
    _container: Arc<ContainerAsync<Postgres>>,
}

#[async_trait::async_trait]
impl CertStore for PgCert {
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
        Ok(Box::new(self.raw_source().await?))
    }
}

impl PgCert {
    /// The concrete source, for the tests that need `retained_wal_bytes` and
    /// `confirmed_flush_lsn` — which the trait object cannot reach.
    async fn raw_source(&self) -> anyhow::Result<PostgresSource> {
        PostgresSource::open(
            &self.url,
            &self.table,
            "cert",
            &self.slot,
            &self.publication,
            Duration::from_millis(300),
        )
        .await
        .map_err(anyhow::Error::from)
    }
}

/// A fresh table, slot and publication per test — sharing any of them would
/// let one test's writes satisfy another's assertions, and slots in particular
/// carry state between tests by design.
async fn pg_cert(container: Arc<ContainerAsync<Postgres>>, url: &str) -> PgCert {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let table = format!("env_{}", &suffix[..12]);
    let repo = Arc::new(
        PostgresRepository::new_with_table(url, &table)
            .await
            .unwrap(),
    );
    PgCert {
        url: url.to_string(),
        slot: format!("slot_{}", &suffix[..12]),
        publication: format!("pub_{}", &suffix[..12]),
        table,
        repo,
        _container: container,
    }
}

// ── The shared certification ────────────────────────────────────────────

#[tokio::test]
async fn postgres_passes_the_same_certification_as_sqlite_and_mongo() {
    let (container, url) = logical_postgres().await;
    let container = Arc::new(container);

    cert::certify_snapshot_then_stream(&pg_cert(container.clone(), &url).await)
        .await
        .expect("snapshot-then-stream");
    cert::certify_positions_are_present_and_distinct(&pg_cert(container.clone(), &url).await)
        .await
        .expect("native positions");
    cert::certify_resume_delivers_only_what_follows(&pg_cert(container.clone(), &url).await)
        .await
        .expect("resume");
    cert::certify_never_mode_skips_history(&pg_cert(container.clone(), &url).await)
        .await
        .expect("snapshot_mode = never");
}

// ── The chain ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_committed_postgres_write_reaches_the_merkql_topic() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;
    let dir = tempfile::tempdir().unwrap();

    let merk = broker(dir.path());
    let writer = TopicWriter::claim(merk.clone(), "lay_report", dir.path()).unwrap();
    let source = PostgresSource::open(
        &store.url,
        &store.table,
        "lay_report",
        &store.slot,
        &store.publication,
        Duration::from_millis(300),
    )
    .await
    .unwrap();
    let mut offsets = OffsetStore::open(
        dir.path().join("o.json"),
        "postgresql",
        "lay_report",
        Duration::from_millis(0),
    )
    .unwrap();

    // `Never`, deliberately: `PostgresSource::open` has already created the
    // slot, so it is retaining WAL from before the write below and nothing can
    // be lost — but starting at the live tail makes the write arrive as a
    // live `c` rather than racing the snapshot query for it. The `Initial`
    // path is covered by `certify_snapshot_then_stream`, which controls that
    // ordering properly instead of racing it.
    let connector = tokio::spawn(async move {
        let _ = run_connector(&source, &writer, &mut offsets, SnapshotMode::Never).await;
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
            "the postgres connector never delivered the committed write"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert_eq!(records[0].op, Op::Create);
    assert_eq!(records[0].source.connector, "postgresql");
    assert_eq!(records[0].key().as_deref(), Some("evt-1"));
    assert_eq!(
        records[0].after.as_ref().unwrap().payload["eggs"],
        json!(3),
        "the committed payload must survive pgoutput decoding onto the topic"
    );
    assert_eq!(
        records[0].after.as_ref().unwrap().auth,
        meshql_core::AuthMark::from(vec!["cert".to_string()]),
        "authorized_tokens must survive pgoutput decoding"
    );
    // The position is a real Postgres LSN, not a counter we invented.
    let position = records[0].position().expect("a native position");
    assert!(
        position.contains('/')
            && u64::from_str_radix(position.split('/').next().unwrap(), 16).is_ok(),
        "the position must be a PostgreSQL LSN, got {position}"
    );

    connector.abort();
}

// ── The heartbeat ───────────────────────────────────────────────────────

/// **The WAL trap, and the proof the heartbeat springs it.**
///
/// An idle watched table with a busy database is the dangerous case: the slot
/// has no changes of its own to advance past, so without a heartbeat its
/// `confirmed_flush_lsn` never moves while every *other* table piles WAL up
/// behind it until the disk fills.
///
/// This test generates WAL from a table the publication does not cover, then
/// asserts the slot's confirmed flush LSN moves anyway. Without the heartbeat
/// advance it stays put and the assertion fails.
#[tokio::test]
async fn the_heartbeat_advances_the_slot_while_the_watched_table_is_idle() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;

    // A table nobody is watching — this is the "busy database" half.
    let pool = sqlx::postgres::PgPool::connect(&url).await.unwrap();
    sqlx::query("CREATE TABLE unrelated_traffic (id serial primary key, filler text)")
        .execute(&pool)
        .await
        .unwrap();

    let source = store.raw_source().await.unwrap();
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    let before = source
        .confirmed_flush_lsn()
        .await
        .unwrap()
        .expect("the slot exists");

    // Generate WAL that the connector's publication does not cover.
    for _ in 0..200 {
        sqlx::query("INSERT INTO unrelated_traffic (filler) VALUES (repeat('x', 4096))")
            .execute(&pool)
            .await
            .unwrap();
    }

    // Drive the feed. It has nothing to yield — the watched table is idle —
    // so this drives it through the heartbeat path and then times out, which
    // is the expected shape rather than a failure.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        cert::take(&mut stream, 1, Duration::from_secs(5)),
    )
    .await;

    let after = source
        .confirmed_flush_lsn()
        .await
        .unwrap()
        .expect("the slot still exists");

    assert_ne!(
        before, after,
        "the heartbeat must advance confirmed_flush_lsn even though the watched \
         table saw no writes — otherwise an idle connector pins WAL until the \
         disk fills. before={before} after={after}"
    );

    // And the backlog it is holding must be observable, because that number is
    // the one an operator needs when deciding whether a stopped connector is
    // about to take the database down.
    let retained = source
        .retained_wal_bytes()
        .await
        .unwrap()
        .expect("a live slot reports its backlog");
    assert!(
        retained >= 0,
        "retained WAL must be measurable, got {retained}"
    );
}

/// A slot that does not exist has no backlog to report — and that is exactly
/// the state a retired connector leaves behind if it drops its slot properly.
#[tokio::test]
async fn a_dropped_slot_reports_no_backlog() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;
    // `open` establishes capture, so the slot exists and is already pinning
    // WAL — which is the whole hazard, and why the number must be reportable.
    let source = store.raw_source().await.unwrap();
    assert!(
        source.retained_wal_bytes().await.unwrap().is_some(),
        "a live slot must report its backlog — that number is what tells an \
         operator a stopped connector is about to fill the disk"
    );

    let pool = sqlx::postgres::PgPool::connect(&url).await.unwrap();
    sqlx::query("SELECT pg_drop_replication_slot($1)")
        .bind(&store.slot)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(source.retained_wal_bytes().await.unwrap(), None);
}

// ── Guards ──────────────────────────────────────────────────────────────

/// **The worst state this connector can be in.** If the slot is gone, every
/// change since the stored position was discarded by PostgreSQL and cannot be
/// recovered from anywhere. Starting from a fresh slot would look like a clean
/// start and silently skip all of it.
#[tokio::test]
async fn a_missing_slot_makes_a_stored_position_unusable() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;
    let source = store.raw_source().await.unwrap();

    // Establish the slot and get a real LSN to resume from.
    let _ = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();
    let position = source.confirmed_flush_lsn().await.unwrap().unwrap();

    // The control: while the slot exists, that position IS honoured.
    assert!(
        source
            .changes(Resume::At(position.clone()), SnapshotMode::Initial)
            .await
            .is_ok(),
        "an in-range position on a live slot must be accepted, or the rejection \
         below proves nothing"
    );

    let pool = sqlx::postgres::PgPool::connect(&url).await.unwrap();
    sqlx::query("SELECT pg_drop_replication_slot($1)")
        .bind(&store.slot)
        .execute(&pool)
        .await
        .unwrap();

    let err = match source
        .changes(Resume::At(position.clone()), SnapshotMode::Initial)
        .await
    {
        Ok(_) => panic!("resuming against a dropped slot must not silently restart"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CdcError::UnusablePosition { .. }),
        "must be UnusablePosition, got: {err}"
    );
    assert!(
        err.to_string().contains("no longer exists"),
        "the error must name the cause so an operator can act: {err}"
    );
}

/// An LSN ahead of the server's own write position means this offset file
/// belongs to another cluster, or the database was restored from a backup.
/// Honouring it would wait forever for records that will never arrive.
#[tokio::test]
async fn an_lsn_ahead_of_the_server_is_unusable() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;
    let source = store.raw_source().await.unwrap();
    let _ = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    let err = match source
        .changes(Resume::At("FFFF/FFFFFFFF".into()), SnapshotMode::Initial)
        .await
    {
        Ok(_) => panic!("an LSN past the server's write position must not be honoured"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CdcError::UnusablePosition { .. }),
        "must be UnusablePosition, got: {err}"
    );

    for bad in ["", "not-an-lsn", "0/", "/0", "0/ZZZZ"] {
        let err = match source
            .changes(Resume::At(bad.into()), SnapshotMode::Initial)
            .await
        {
            Ok(_) => panic!("the malformed LSN {bad:?} must not be honoured"),
            Err(e) => e,
        };
        assert!(
            matches!(err, CdcError::UnusablePosition { .. }),
            "{bad:?} got: {err}"
        );
    }
}

/// `wal_level = replica` cannot do logical decoding at all. Failing at open
/// beats failing later with an opaque slot-creation error.
#[tokio::test]
async fn a_server_without_logical_wal_is_a_loud_no_feed() {
    // The stock image: wal_level = replica.
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432.tcp()).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let err = match PostgresSource::open(
        &url,
        "envelopes",
        "cert",
        "some_slot",
        "some_pub",
        Duration::from_millis(300),
    )
    .await
    {
        Ok(_) => panic!("a server without wal_level=logical must not open"),
        Err(e) => e,
    };
    assert!(
        matches!(err, CdcError::NoFeed { .. }),
        "must be NoFeed, got: {err}"
    );
    assert!(err.to_string().contains("wal_level"), "got: {err}");
}

/// **The durability guard, from the outside.**
///
/// Records that have been *read* but not yet confirmed durable must survive a
/// connector restart. Two separate mechanisms have to hold for that, and this
/// test fails if either is removed:
///
/// 1. **`peek`, not `get`.** `pg_logical_slot_get_binary_changes` consumes as
///    it reads, so a crash between reading and appending destroys the records
///    — they exist nowhere. Only `peek` leaves them in the slot.
/// 2. **The heartbeat clamp.** The idle heartbeat advances the slot to release
///    WAL, and it must never advance past records the connector has not
///    confirmed. An unclamped heartbeat discards exactly the records this test
///    is waiting for, and does it on a timer, so the loss looks spontaneous.
///
/// Both failures are silent in production: the records simply never appear on
/// the topic, and every projection folded from it is quietly short.
#[tokio::test]
async fn records_read_but_not_confirmed_durable_survive_a_restart() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;

    let source = store.raw_source().await.unwrap();
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    store
        .write(Envelope::new("unconfirmed", Stash::new(), vec![]))
        .await
        .unwrap();

    let read = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(read[0].key().as_deref(), Some("unconfirmed"));

    // Crucially, `durable_through` is NEVER called: this simulates a connector
    // that read the record and then died before its offset commit.
    //
    // The stream must be KEPT POLLED here, not merely slept beside. A stream
    // advances only while something awaits it, so a bare `sleep` would leave
    // the feed frozen and no heartbeat tick would run at all — the test would
    // then pass against an unclamped heartbeat, proving nothing. This `take`
    // is expected to time out; its job is to drive several heartbeat ticks
    // (300ms each) against the idle slot, which is precisely when an
    // unclamped heartbeat would discard the unconfirmed record.
    let nothing_more = cert::take(&mut stream, 1, Duration::from_millis(1_500)).await;
    assert!(
        nothing_more.is_err(),
        "nothing else was written, so the feed must stay quiet: {:?}",
        nothing_more.map(|r| r.iter().filter_map(|x| x.key()).collect::<Vec<_>>())
    );
    drop(stream);

    // Restart: a fresh source on the same slot, with no stored position.
    let restarted = store.raw_source().await.unwrap();
    let mut stream = restarted
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();
    let replayed = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .expect(
            "a record that was read but never confirmed durable MUST still be in the slot \
             after a restart — losing it is a silent, permanent gap in the log",
        );
    assert_eq!(
        replayed[0].key().as_deref(),
        Some("unconfirmed"),
        "the unconfirmed record must be re-delivered, not skipped"
    );
}

/// The steady-state counterpart: because `peek` does not consume, the feed must
/// not hand the same record over again and again while waiting for the
/// connector to confirm it. Duplicates are the price of a restart, not a
/// per-cycle behaviour.
#[tokio::test]
async fn a_read_record_is_not_re_delivered_while_waiting_for_confirmation() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;

    let source = store.raw_source().await.unwrap();
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    store
        .write(Envelope::new("once", Stash::new(), vec![]))
        .await
        .unwrap();
    let first = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(first[0].key().as_deref(), Some("once"));

    // Nothing else has been written and nothing has been confirmed. A second
    // record arriving here would be the same one over again.
    let again = tokio::time::timeout(
        Duration::from_secs(2),
        cert::take(&mut stream, 1, Duration::from_secs(2)),
    )
    .await;
    assert!(
        matches!(again, Err(_) | Ok(Err(_))),
        "the feed must not re-deliver an unconfirmed record every cycle; got {:?}",
        again.map(|r| r.map(|v| v.iter().filter_map(|x| x.key()).collect::<Vec<_>>()))
    );
}

/// The clamp's other arm: some records are confirmed durable and a later one is
/// not. The heartbeat must release WAL only up to the confirmed point, never up
/// to the current WAL position, or it destroys the record still in flight.
#[tokio::test]
async fn the_heartbeat_never_releases_wal_past_the_confirmed_position() {
    let (container, url) = logical_postgres().await;
    let store = pg_cert(Arc::new(container), &url).await;

    let source = store.raw_source().await.unwrap();
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    // Record A: read AND confirmed durable, exactly as `run_connector` would
    // after an offset commit.
    store
        .write(Envelope::new("confirmed", Stash::new(), vec![]))
        .await
        .unwrap();
    let a = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(a[0].key().as_deref(), Some("confirmed"));
    source
        .durable_through(a[0].position().unwrap())
        .await
        .unwrap();

    // Record B: read but NOT confirmed. The clamp now has a real confirmed
    // position to hold at, which is the arm a `None` durable position cannot
    // reach.
    store
        .write(Envelope::new("in-flight", Stash::new(), vec![]))
        .await
        .unwrap();
    let b = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(b[0].key().as_deref(), Some("in-flight"));

    // Drive the idle heartbeat while B is unconfirmed (see the note in the
    // test above about why this polls rather than sleeps).
    let _ = cert::take(&mut stream, 1, Duration::from_millis(1_500)).await;
    drop(stream);

    let restarted = store.raw_source().await.unwrap();
    let mut stream = restarted
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();
    let replayed = cert::take(&mut stream, 1, Duration::from_secs(20))
        .await
        .expect("the unconfirmed record must survive the heartbeat");
    assert_eq!(
        replayed[0].key().as_deref(),
        Some("in-flight"),
        "the heartbeat may only release WAL up to the CONFIRMED position; \
         releasing to the current WAL position destroys records in flight"
    );
    assert_ne!(
        replayed[0].key().as_deref(),
        Some("confirmed"),
        "the confirmed record should have been released — otherwise the clamp \
         is simply never advancing and this test would pass vacuously"
    );
}

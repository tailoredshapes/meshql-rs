//! End-to-end proof of the full pipeline described in
//! docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md:
//!
//!   POST /lay_report/api (REST)
//!     -> SearcherTail (storage-layer CDC, no restlette hook)
//!       -> ChangeHub (fan-out — the SAME hub examples/farm wires
//!          run_tails/run_merkql_sink onto in production)
//!         -> publish_to_merkql (Component 1: the connector's produce side)
//!           -> merkql topic "lay_report"
//!             -> farm_worker::worker::process_batch (Component 2: the worker)
//!               -> GET /lay_report/graph (detail lookup)
//!               -> GET /hen_productivity/graph (read current)
//!               -> POST or PUT /hen_productivity/api (write)
//!                 -> GET /hen_productivity/graph confirms the result
//!
//! `tick_connector` drives a real `ChangeHub` (`hub.publish` + a
//! `try_recv()` drain loop) rather than calling `publish_to_merkql`
//! directly off the tail — the fan-out hop is real infrastructure
//! `examples/farm` depends on in production (see its `build()`), not
//! just a detail of this test's plumbing. What's still NOT exercised here
//! is `run_tails`/`run_merkql_sink`/`run_forever` themselves — the
//! interval-loop wrappers around these same calls — because spawning them
//! and sleeping would trade this test's determinism for a small amount of
//! extra realism; those wrappers are covered in isolation by
//! `meshql-changes`' own unit tests (`hub.rs`, `merkql_sink.rs`) and
//! `worker.rs`'s `run_forever`... which itself has no dedicated test
//! (it's a thin loop around `process_batch`, already covered here).
//!
//! Also proves idempotency: redelivering the same lay_report id onto the
//! merkql topic must not double-count its eggs.

use farm_worker::config::WorkerConfig;
use farm_worker::worker::process_batch;
use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use meshql_changes::{publish_to_merkql, ChangeEvent, ChangeHub, ChangeSource, SearcherTail};
use meshql_core::{
    GraphletteConfig, Repository, RestletteConfig, RootConfig, Searcher, ServerConfig,
};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast;

const LAY_REPORT_GRAPHQL: &str = r#"
type Query {
  getLayReport(id: ID, at: Float): LayReport
  getLayReportsByHen(id: ID, at: Float): [LayReport]
}
type LayReport {
  id: ID
  henId: String
  eggs: Int
  timeOfDay: String
}
"#;

const HEN_PRODUCTIVITY_GRAPHQL: &str = r#"
type Query {
  getHenProductivityByHen(id: ID, at: Float): [HenProductivity]
}
type HenProductivity {
  id: ID
  henId: String
  totalEggs: Int
  lastLaidAt: String
}
"#;

async fn sqlite_pool() -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1) // one connection per pool — sqlite::memory: is per-connection
        .connect_with(opts)
        .await
        .unwrap()
}

/// Stands up ONE axum server hosting both lay_report and hen_productivity
/// meshes — the most faithful in-process stand-in for "one farm deployment"
/// this workspace's conventions allow without a live Mongo.
async fn start_farm() -> (
    String,
    Arc<SqliteRepository>,
    Arc<SqliteRepository>,
    Arc<dyn Searcher>,
) {
    let lay_pool = sqlite_pool().await;
    let lay_repo = Arc::new(
        SqliteRepository::new_with_pool(lay_pool.clone())
            .await
            .unwrap(),
    );
    let lay_searcher: Arc<dyn Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(lay_pool).await.unwrap());

    let hp_pool = sqlite_pool().await;
    let hp_repo = Arc::new(
        SqliteRepository::new_with_pool(hp_pool.clone())
            .await
            .unwrap(),
    );
    let hp_searcher: Arc<dyn Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(hp_pool).await.unwrap());

    let lay_config = RootConfig::builder()
        .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
        // "payload." prefix required — see the "Facts to respect" note at
        // the top of this plan. Feeds fetch_lay_reports_for_hen's full,
        // freshly-fetched egg-count list.
        .vector("getLayReportsByHen", r#"{"payload.henId": "{{id}}"}"#)
        .build();
    let hp_config = RootConfig::builder()
        // "payload." prefix required — both Mongo and sqlite nest payload
        // fields; a bare "henId" key is silently ignored. See "Facts to
        // respect" at the top of this plan.
        .vector("getHenProductivityByHen", r#"{"payload.henId": "{{id}}"}"#)
        .build();

    let config = ServerConfig {
        port: 0, // overridden below; run() binds 0.0.0.0:port, we instead build the app directly
        graphlettes: vec![
            GraphletteConfig {
                path: "/lay_report/graph".to_string(),
                schema_text: LAY_REPORT_GRAPHQL.to_string(),
                root_config: lay_config,
                searcher: Arc::clone(&lay_searcher),
            },
            GraphletteConfig {
                path: "/hen_productivity/graph".to_string(),
                schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
                root_config: hp_config,
                searcher: Arc::clone(&hp_searcher),
            },
        ],
        restlettes: vec![
            RestletteConfig {
                path: "/lay_report/api".to_string(),
                schema_json: json!({}),
                repository: lay_repo.clone() as Arc<dyn Repository>,
            },
            RestletteConfig {
                path: "/hen_productivity/api".to_string(),
                schema_json: json!({}),
                repository: hp_repo.clone() as Arc<dyn Repository>,
            },
        ],
    };

    let app = meshql_server::build_app(config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    (format!("http://{addr}"), lay_repo, hp_repo, lay_searcher)
}

fn broker() -> BrokerRef {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Broker::open(BrokerConfig::new(dir.path())).unwrap()
}

fn worker_cfg(base: &str) -> WorkerConfig {
    let base = base.to_string();
    WorkerConfig::from_lookup(move |k| match k {
        "SOURCE_GRAPHQL_URL" | "TARGET_REST_URL" | "TARGET_GRAPHQL_URL" => Some(base.clone()),
        _ => None,
    })
}

async fn post_lay_report(base: &str, hen_id: &str, eggs: i64, time_of_day: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/lay_report/api"))
        .json(&json!({ "henId": hen_id, "eggs": eggs, "timeOfDay": time_of_day }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

async fn read_hen_productivity(base: &str, hen_id: &str) -> Option<Value> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/hen_productivity/graph"))
        .json(&json!({
            "query": format!(
                r#"{{ getHenProductivityByHen(id: "{hen_id}", at: 99999999999999) {{ id henId totalEggs lastLaidAt }} }}"#
            )
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    body["data"]["getHenProductivityByHen"]
        .as_array()
        .and_then(|a| a.first().cloned())
}

/// GraphQL exposes ids (REST deliberately doesn't — see meshql-patterns'
/// REST ID model), so this is how the test discovers a lay_report's
/// server-generated id for the redelivery simulation below.
async fn first_lay_report_id(base: &str, hen_id: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/lay_report/graph"))
        .json(&json!({
            "query": format!(
                r#"{{ getLayReportsByHen(id: "{hen_id}", at: 99999999999999) {{ id }} }}"#
            )
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    body["data"]["getLayReportsByHen"][0]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Drives one full tick of Component 1: poll the lay_report tail, fan the
/// result out through a REAL `ChangeHub` (the same fan-out `run_tails`
/// publishes onto and `run_merkql_sink` subscribes from in production —
/// see `examples/farm::build()`), then drain that hub's receiver
/// non-blockingly and mirror each event onto merkql. `hub.publish` is
/// synchronous and `rx.try_recv()` never awaits, so calling this directly
/// (rather than spawning `run_tails`/`run_merkql_sink` and sleeping) stays
/// fully deterministic — no interval-loop timing to race — while still
/// exercising the real hub, not just its two endpoints in isolation.
async fn tick_connector(
    tail: &SearcherTail,
    hub: &ChangeHub,
    rx: &mut broadcast::Receiver<ChangeEvent>,
    broker: &BrokerRef,
) {
    let events = tail.poll().await.unwrap();
    for ev in events {
        hub.publish(ev);
    }
    while let Ok(ev) = rx.try_recv() {
        publish_to_merkql(broker, &ev).unwrap();
    }
}

async fn tick_worker(broker: &BrokerRef, cfg: &WorkerConfig) -> usize {
    let client = reqwest::Client::new();
    let mut consumer = Broker::consumer(
        broker,
        ConsumerConfig {
            group_id: cfg.group_id.clone(),
            auto_commit: false,
            offset_reset: OffsetReset::Earliest,
        },
    );
    consumer.subscribe(&[cfg.topic.as_str()]).unwrap();
    process_batch(&mut consumer, &client, cfg).await.unwrap()
}

#[tokio::test]
async fn full_pipeline_accumulates_across_reports_and_is_idempotent_under_redelivery() {
    let (base, lay_repo, _hp_repo, lay_searcher) = start_farm().await;
    let broker = broker();
    let tail = SearcherTail::new(
        "lay_report",
        lay_searcher,
        lay_repo.clone() as Arc<dyn Repository>,
    );
    let cfg = worker_cfg(&base);
    // Matches examples/farm::build()'s own ChangeHub::new(256) capacity —
    // ample for this test's handful of events, so try_recv() never races
    // a lagged buffer.
    let hub = ChangeHub::new(256);
    let mut hub_rx = hub.subscribe();

    // timeOfDay is written but deliberately never asserted on below — two
    // of the three landed farm retrofits treat it as a morning/afternoon/
    // evening enum, not a timestamp (see the reconciliation note at the
    // top of this plan), so lastLaidAt is sourced from the ChangeEvent's
    // own created_at instead, not from this field.

    // --- First report ---
    post_lay_report(&base, "hen-1", 3, "morning").await;
    tick_connector(&tail, &hub, &mut hub_rx, &broker).await;
    let n = tick_worker(&broker, &cfg).await;
    assert_eq!(n, 1);

    let hp = read_hen_productivity(&base, "hen-1")
        .await
        .expect("hen_productivity created");
    assert_eq!(hp["totalEggs"], json!(3));
    let first_laid_at = hp["lastLaidAt"].as_str().unwrap().to_string();
    assert!(!first_laid_at.is_empty());
    let hp_id = hp["id"].as_str().unwrap().to_string();

    // --- Second report, same hen: must accumulate, must keep the same id ---
    post_lay_report(&base, "hen-1", 2, "evening").await;
    tick_connector(&tail, &hub, &mut hub_rx, &broker).await;
    let n = tick_worker(&broker, &cfg).await;
    assert_eq!(n, 1);

    let hp = read_hen_productivity(&base, "hen-1").await.unwrap();
    assert_eq!(hp["totalEggs"], json!(5));
    let second_laid_at = hp["lastLaidAt"].as_str().unwrap().to_string();
    // Strict >, not >=: this must prove lastLaidAt actually ADVANCES, not
    // merely that it doesn't regress — a >= check would also pass a buggy
    // implementation that freezes lastLaidAt at its first-ever value. The
    // two REST POSTs are separated by real network round-trips (an
    // intervening tick_connector + tick_worker), so a millisecond tie
    // isn't a realistic flake risk here.
    assert!(
        second_laid_at > first_laid_at,
        "lastLaidAt must advance forward as later reports land"
    );
    assert_eq!(
        hp["id"],
        json!(hp_id),
        "PUT must version the SAME record, not create a new one"
    );

    // --- Idempotency: redeliver the FIRST report's event a second time,
    // with a deliberately OLD created_at ---
    // (simulates a batch that committed the merkql write but was retried
    // for an unrelated reason). Unlike the original accumulate-plus-ledger
    // design, this worker has no per-report dedup state at all — it's
    // idempotent because it recomputes totalEggs fresh from the hen's
    // CURRENT lay_report set every time, and merges lastLaidAt via a
    // monotonic max. This redelivery proves both properties at once: the
    // total must not double-count, AND the old timestamp must not regress
    // lastLaidAt backward past what the second report already advanced it
    // to.
    let first_report_id = first_lay_report_id(&base, "hen-1").await;
    publish_to_merkql(
        &broker,
        &ChangeEvent {
            entity: "lay_report".to_string(),
            id: first_report_id,
            created_at: 1, // deliberately older than either real report's created_at
            deleted: false,
            authorized_tokens: vec![],
        },
    )
    .unwrap();
    let n = tick_worker(&broker, &cfg).await;
    assert_eq!(
        n, 1,
        "the redelivered event is still processed (a no-op recompute)"
    );

    let hp = read_hen_productivity(&base, "hen-1").await.unwrap();
    assert_eq!(
        hp["totalEggs"],
        json!(5),
        "redelivery must NOT double-count eggs"
    );
    assert_eq!(
        hp["lastLaidAt"],
        json!(second_laid_at),
        "redelivering an OLDER event must NOT regress lastLaidAt"
    );
}

//! End-to-end proof of the event-sourced pipeline:
//!
//!   business events → egg-events topic → workers (fold) → domain nouns
//!
//! and the payoff property: **a noun can be rebuilt from history alone.** Every
//! noun here is materialized purely by folding the log — nothing is written to a
//! noun directly. The replay test drops the nouns and rebuilds them from the
//! same events, and a brand-new projection is materialized from history that
//! existed before it did.
//!
//! Runs entirely in-process: an embedded merkql broker plus in-memory SQLite
//! noun stores. No Mongo, no Kafka.

use egg_economy::events::{verb, EventRecord};
use egg_economy::projectors::{
    BuildProjector, ContainerInventoryProjector, FarmOutputProjector, HenProductivityProjector,
    HenProjector, Projector,
};
use egg_economy::{publish, Worker};
use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use meshql_core::Repository;
use meshql_sqlite::SqliteRepository;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

const DAY: i64 = 86_400_000;

fn ev(verb: &str, id: &str, ms: i64, payload: serde_json::Value) -> EventRecord {
    EventRecord {
        verb: verb.to_string(),
        id: id.to_string(),
        created_at_ms: ms,
        payload: payload.as_object().cloned().unwrap(),
    }
}

/// The business events for a small farm, in commit order. Front ends only ever
/// write things in this list; every noun is derived from it.
fn scripted_events() -> Vec<EventRecord> {
    let t0 = 1_700_000_000_000; // fixed base so windowed aggregates are deterministic
    vec![
        ev(
            verb::BUILD_FARM,
            "farm-1",
            t0,
            json!({"name":"Green Acres","farm_type":"local_farm","zone":"north","owner":"Mac"}),
        ),
        ev(
            verb::BUILD_COOP,
            "coop-1",
            t0 + 100,
            json!({"farm_id":"farm-1","name":"Sunrise","capacity":20,"coop_type":"layer"}),
        ),
        ev(
            verb::BUILD_CONTAINER,
            "cont-1",
            t0 + 200,
            json!({"name":"Cold Store","container_type":"refrigerator","capacity":500,"zone":"north"}),
        ),
        ev(
            verb::BUILD_CONTAINER,
            "cont-2",
            t0 + 300,
            json!({"name":"Barn Box","container_type":"crate","capacity":100,"zone":"north"}),
        ),
        ev(
            verb::REGISTER_CONSUMER,
            "cons-1",
            t0 + 400,
            json!({"name":"Local Diner","consumer_type":"restaurant","zone":"north","weekly_demand":100}),
        ),
        // Buy 3 hens in one event → hens buy-1-0, buy-1-1, buy-1-2
        ev(
            verb::BUY_HENS,
            "buy-1",
            t0 + 500,
            json!({"coop_id":"coop-1","farm_id":"farm-1","breed":"Leghorn","count":3}),
        ),
        // Eggs laid over three days
        ev(
            verb::EGGS_LAID,
            "lay-1",
            t0 + DAY,
            json!({"hen_id":"buy-1-0","coop_id":"coop-1","farm_id":"farm-1","eggs":3,"quality":"grade_a"}),
        ),
        ev(
            verb::EGGS_LAID,
            "lay-2",
            t0 + 2 * DAY,
            json!({"hen_id":"buy-1-0","coop_id":"coop-1","farm_id":"farm-1","eggs":2,"quality":"grade_b"}),
        ),
        ev(
            verb::EGGS_LAID,
            "lay-3",
            t0 + 2 * DAY,
            json!({"hen_id":"buy-1-1","coop_id":"coop-1","farm_id":"farm-1","eggs":4,"quality":"grade_a"}),
        ),
        // Store, transfer, consume
        ev(
            verb::EGGS_STORED,
            "dep-1",
            t0 + 3 * DAY,
            json!({"container_id":"cont-1","source_type":"farm","source_id":"farm-1","eggs":9}),
        ),
        ev(
            verb::EGGS_TRANSFERRED,
            "xfer-1",
            t0 + 3 * DAY + 10,
            json!({"source_container_id":"cont-1","dest_container_id":"cont-2","eggs":4}),
        ),
        ev(
            verb::EGGS_CONSUMED,
            "eat-1",
            t0 + 3 * DAY + 20,
            json!({"consumer_id":"cons-1","container_id":"cont-2","eggs":1}),
        ),
        // Actor mutations
        ev(
            verb::MOVE_HEN_TO_COOP,
            "mv-1",
            t0 + 4 * DAY,
            json!({"hen_id":"buy-1-2","coop_id":"coop-1"}),
        ),
        ev(
            verb::RETIRE_HEN,
            "ret-1",
            t0 + 5 * DAY,
            json!({"hen_id":"buy-1-1","reason":"age"}),
        ),
    ]
}

async fn make_repo() -> Arc<SqliteRepository> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    Arc::new(SqliteRepository::new_with_pool(pool).await.unwrap())
}

fn broker() -> BrokerRef {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Broker::open(BrokerConfig::new(dir.path())).unwrap()
}

/// One noun repo per projector, wired into a worker.
fn workers(broker: &BrokerRef, repos: &[Arc<SqliteRepository>]) -> Vec<Worker> {
    let projectors: Vec<Box<dyn Projector>> = vec![
        Box::new(BuildProjector::farm()),
        Box::new(BuildProjector::coop()),
        Box::new(HenProjector::new()),
        Box::new(BuildProjector::container()),
        Box::new(BuildProjector::consumer()),
        Box::new(HenProductivityProjector::new()),
        Box::new(ContainerInventoryProjector::new()),
        Box::new(FarmOutputProjector::new()),
    ];
    projectors
        .into_iter()
        .zip(repos.iter())
        .map(|(p, r)| Worker::new(broker.clone(), p, r.clone() as Arc<dyn Repository>))
        .collect()
}

async fn read(
    repo: &Arc<SqliteRepository>,
    id: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    repo.read(
        id,
        &meshql_core::TokenSession::new(vec!["*".to_string()]),
        None,
    )
    .await
    .unwrap()
    .map(|e| e.payload)
}

#[tokio::test]
async fn events_fold_into_nouns_and_replay_rebuilds_them() {
    let broker = broker();
    // Publish the business events onto the topic exactly as the CDC connector would.
    for e in scripted_events() {
        publish(&broker, &e).unwrap();
    }

    // Noun repos, in projector order.
    let repos: Vec<Arc<SqliteRepository>> = {
        let mut v = Vec::new();
        for _ in 0..8 {
            v.push(make_repo().await);
        }
        v
    };
    let (farm, coop, hen, container, consumer, productivity, inventory, output) = (
        &repos[0], &repos[1], &repos[2], &repos[3], &repos[4], &repos[5], &repos[6], &repos[7],
    );

    // Run every worker once — folds the whole log into nouns.
    for mut w in workers(&broker, &repos) {
        w.run_once().await.unwrap();
    }

    // --- Actor nouns were built purely from events ---
    assert_eq!(
        read(farm, "farm-1").await.unwrap()["name"],
        json!("Green Acres")
    );
    assert_eq!(read(coop, "coop-1").await.unwrap()["capacity"], json!(20));
    assert_eq!(
        read(consumer, "cons-1").await.unwrap()["name"],
        json!("Local Diner")
    );
    assert_eq!(
        read(container, "cont-1").await.unwrap()["container_type"],
        json!("refrigerator")
    );

    // buy_hens(count=3) fanned out to three hens
    let h0 = read(hen, "buy-1-0").await.unwrap();
    assert_eq!(h0["breed"], json!("Leghorn"));
    assert_eq!(h0["status"], json!("active"));
    // retire_hen and move_hen_to_coop mutated the right hens
    assert_eq!(
        read(hen, "buy-1-1").await.unwrap()["status"],
        json!("retired")
    );
    assert_eq!(
        read(hen, "buy-1-2").await.unwrap()["coop_id"],
        json!("coop-1")
    );

    // --- Analytic nouns folded from egg-flow events ---
    // hen buy-1-0: 3 + 2 = 5 eggs, one grade_a of 3 → quality_rate 0.6
    let p0 = read(productivity, "buy-1-0").await.unwrap();
    assert_eq!(p0["total_eggs"], json!(5));
    assert_eq!(p0["quality_rate"], json!(0.6));

    // container_inventory: cont-1 got 9 deposited then 4 transferred out = 5
    let inv1 = read(inventory, "cont-1").await.unwrap();
    assert_eq!(inv1["current_eggs"], json!(5));
    assert_eq!(inv1["total_transfers_out"], json!(4));
    // cont-2 received 4, consumed 1 = 3
    let inv2 = read(inventory, "cont-2").await.unwrap();
    assert_eq!(inv2["current_eggs"], json!(3));

    // farm_output: farm-1 total this month = 3 + 2 + 4 = 9, two distinct laying hens
    let out = read(output, "farm-1").await.unwrap();
    assert_eq!(out["eggs_month"], json!(9));
    assert_eq!(out["active_hens"], json!(2));

    // --- Replay: rebuild every noun from history in fresh stores ---
    let fresh: Vec<Arc<SqliteRepository>> = {
        let mut v = Vec::new();
        for _ in 0..8 {
            v.push(make_repo().await);
        }
        v
    };
    for mut w in workers(&broker, &fresh) {
        w.run_once().await.unwrap();
    }
    // Same events → same nouns, field for field.
    assert_eq!(read(&fresh[0], "farm-1").await, read(farm, "farm-1").await);
    assert_eq!(read(&fresh[2], "buy-1-1").await, read(hen, "buy-1-1").await);
    assert_eq!(
        read(&fresh[5], "buy-1-0").await,
        read(productivity, "buy-1-0").await
    );
    assert_eq!(
        read(&fresh[6], "cont-1").await,
        read(inventory, "cont-1").await
    );

    // --- A brand-new noun materializes from history that predates it ---
    // Realize later we want a "coop_output" style projection — here, farm_output
    // for a farm, built fresh from the same old events with a fresh worker.
    let latecomer = make_repo().await;
    let mut w = Worker::new(
        broker.clone(),
        Box::new(FarmOutputProjector::new()),
        latecomer.clone() as Arc<dyn Repository>,
    );
    w.run_once().await.unwrap();
    assert_eq!(
        read(&latecomer, "farm-1").await.unwrap()["eggs_month"],
        json!(9)
    );

    // --- Idempotence: re-running a worker over the same log writes nothing new ---
    let mut w2 = workers(&broker, &repos).into_iter().nth(5).unwrap();
    assert_eq!(
        w2.run_once().await.unwrap(),
        0,
        "second run must be a no-op"
    );
}

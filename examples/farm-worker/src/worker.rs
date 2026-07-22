//! The consumer loop: poll the lay_report merkql topic, fold, write,
//! commit — with the offset-commit discipline the pipeline spec's
//! backpressure guidance asks for. See the module-level comment on
//! `process_batch` for the merkql `Consumer::poll` gotcha this is built
//! around.

use crate::config::WorkerConfig;
use crate::detail::{fetch_lay_report, fetch_lay_reports_for_hen};
use crate::event::ThinEvent;
use crate::productivity::recompute;
use crate::rest_client::{get_current, write};
use chrono::{DateTime, SecondsFormat, Utc};
use merkql::broker::{Broker, BrokerRef};
use merkql::consumer::{Consumer, ConsumerConfig, OffsetReset};
use std::time::Duration;

/// Render an epoch-millis timestamp (a ChangeEvent's `created_at`) as a
/// fixed-width, `Z`-suffixed ISO-8601 instant. Every `last_laid_at` this
/// worker ever writes goes through this one function, which is what makes
/// `productivity::recompute`'s `max(current.last_laid_at, ...)` string
/// comparison agree with chronological order.
///
/// Returns `None` (never panics) for a `created_at_ms` outside the range
/// `DateTime<Utc>` can represent. `created_at` crosses a deserialization
/// boundary from the merkql topic (`ThinEvent` via `serde_json::from_str`),
/// so — like the unparseable-JSON and unexpected-deleted-flag cases right
/// above this function's caller — malformed wire data must be a per-record
/// skip, not a panic: `run_forever` (Task 9) awaits this loop directly on
/// the main task with no `tokio::spawn` isolation, so a panic here would
/// crash the whole worker process, and since the panic would happen before
/// `commit_sync()`, a restart would just re-fetch and re-panic on the same
/// poison record forever.
fn to_iso8601(created_at_ms: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(created_at_ms)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Process everything currently available on the topic in one poll, and
/// commit the consumer offset only if EVERY record folded and wrote
/// successfully.
///
/// `merkql::Consumer::poll` advances its in-memory read position to the
/// batch's tail as soon as it reads the records — BEFORE the caller
/// processes any of them (verified against `merkql/src/consumer.rs`). So a
/// partial failure here must NOT call `commit_sync()` (that would persist a
/// position past records this call never actually processed), and the
/// CALLER must throw this `Consumer` away and build a fresh one for the
/// next attempt (see `run_forever`) — re-polling the SAME `Consumer` after
/// an error returns an empty batch forever, since its in-memory position
/// already points past the very records that failed. Mirrors
/// `SearcherTail::poll`'s "commit only after every fallible op succeeds"
/// discipline, and matches the pipeline spec's backpressure guidance
/// ("don't advance the consumer offset until the REST write ... succeeds").
pub async fn process_batch(
    consumer: &mut Consumer,
    client: &reqwest::Client,
    cfg: &WorkerConfig,
) -> anyhow::Result<usize> {
    let batch = consumer
        .poll(Duration::from_millis(200))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if batch.is_empty() {
        return Ok(0);
    }

    let mut processed = 0;
    for record in &batch {
        let thin: ThinEvent = match serde_json::from_str(&record.value) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[farm-worker] skipping unparseable record: {e}");
                continue;
            }
        };
        if thin.deleted {
            // lay_report is create-only per the retrofit spec; a delete
            // here means something unexpected upstream. Log and skip
            // rather than fail the whole batch over a record this worker
            // was never meant to receive.
            eprintln!(
                "[farm-worker] unexpected deleted lay_report event {}, skipping",
                thin.id
            );
            continue;
        }
        // Validate created_at up front — before spending any network
        // round-trips on a record we'd have to discard anyway — and skip
        // rather than panic (see to_iso8601's doc comment for why a panic
        // here would be a process-crashing poison-pill record).
        let event_created_at_iso = match to_iso8601(thin.created_at) {
            Some(iso) => iso,
            None => {
                eprintln!(
                    "[farm-worker] skipping lay_report event {} with out-of-range created_at={}",
                    thin.id, thin.created_at
                );
                continue;
            }
        };

        // Deliberately NOT thin.created_at: `at` is a hard `createdAt <=
        // at` cutoff on every backend (no fallback — a query for an id
        // whose stored createdAt is AFTER `at` returns null/empty, full
        // stop). thin.created_at is the redelivered event's OWN commit
        // time, which is exactly the record we're about to fetch — using
        // it as the cutoff would exclude that very record (and would
        // exclude any sibling lay_report for the same hen committed later
        // but still present in the same poll batch). now_ms is "as of
        // right now," which is what "the hen's full CURRENT report set"
        // (see fetch_lay_reports_for_hen's doc comment) actually means.
        // thin.created_at is still used below, but only to feed
        // last_laid_at's merge, where it's correct: that's specifically
        // this event's own timestamp, not a query cutoff.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = fetch_lay_report(
            client,
            &cfg.source_graphql_base,
            &thin.id,
            now_ms,
            cfg.query_dialect,
        )
        .await?;
        // Full recompute, not an incremental add — fetched fresh every
        // time so the fold is idempotent under redelivery with no dedup
        // ledger. See productivity::recompute's doc comment.
        let report_eggs = fetch_lay_reports_for_hen(
            client,
            &cfg.source_graphql_base,
            &report.hen_id,
            now_ms,
            cfg.query_dialect,
        )
        .await?;
        let current = get_current(client, cfg, &report.hen_id).await?;
        let next = recompute(
            current.as_ref(),
            &report.hen_id,
            &report_eggs,
            &event_created_at_iso,
        );
        if Some(&next) != current.as_ref() {
            write(client, cfg, &next).await?;
        }
        processed += 1;
    }

    consumer
        .commit_sync()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(processed)
}

/// Build a fresh consumer against the group's last COMMITTED offset (never
/// the previous tick's in-memory position — see `process_batch`) and
/// process one batch. Runs forever; each tick's failure is logged and
/// retried next tick, matching `run_tails`'s "poll errors are logged and
/// retried, never fatal" convention. A fresh `Consumer` per tick also means
/// the worker picks up the `lay_report` topic correctly even if it started
/// before the connector ever produced to it (`Consumer::subscribe` only
/// sees a topic that exists at subscribe time).
pub async fn run_forever(broker: BrokerRef, client: reqwest::Client, cfg: WorkerConfig) {
    loop {
        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: cfg.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        if let Err(e) = consumer.subscribe(&[cfg.topic.as_str()]) {
            eprintln!("[farm-worker] subscribe: {e}");
        } else {
            match process_batch(&mut consumer, &client, &cfg).await {
                Ok(0) => {}
                Ok(n) => println!("[farm-worker] processed {n} lay_report event(s)"),
                Err(e) => {
                    eprintln!("[farm-worker] batch failed, offset not advanced, will retry: {e}")
                }
            }
        }
        tokio::time::sleep(cfg.poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkerConfig;
    use crate::productivity::HenProductivity;
    use axum::extract::{Path, State};
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use merkql::broker::{Broker, BrokerConfig, BrokerRef};
    use merkql::consumer::{ConsumerConfig, OffsetReset};
    use merkql::record::ProducerRecord;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn broker() -> BrokerRef {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Broker::open(BrokerConfig::new(dir.path())).unwrap()
    }

    fn publish_thin_event(broker: &BrokerRef, id: &str, created_at: i64) {
        let producer = Broker::producer(broker);
        let value = format!(
            r#"{{"entity":"lay_report","id":"{id}","created_at":{created_at},"deleted":false}}"#
        );
        producer
            .send(&ProducerRecord::new(
                "lay_report",
                Some(id.to_string()),
                value,
            ))
            .unwrap();
    }

    #[derive(Clone, Default)]
    struct FakeFarm {
        // henId -> report id -> report body (mirrors the source farm's
        // real shape closely enough for this test double: one hen can have
        // several lay_reports).
        lay_reports: Arc<Mutex<std::collections::HashMap<String, Value>>>,
        productivity: Arc<Mutex<Option<HenProductivity>>>,
    }

    async fn lay_report_graph(
        State(farm): State<FakeFarm>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        if query.contains("getLayReportsByHen") {
            // Extract the quoted hen id naively — good enough for a test
            // double. Every currently-registered report for this hen is
            // returned, matching fetch_lay_reports_for_hen's contract of
            // "the hen's FULL current set, fetched fresh."
            let hen_id = query.split('"').nth(1).unwrap_or_default();
            let reports: Vec<Value> = farm
                .lay_reports
                .lock()
                .unwrap()
                .values()
                .filter(|r| r["henId"] == hen_id)
                .map(|r| json!({ "eggs": r["eggs"] }))
                .collect();
            return Json(json!({ "data": { "getLayReportsByHen": reports } }));
        }
        // Otherwise: a single-report lookup by report id.
        let id = query.split('"').nth(1).unwrap_or_default();
        let report = farm.lay_reports.lock().unwrap().get(id).cloned();
        Json(json!({ "data": { "getLayReport": report } }))
    }

    async fn hp_graph(State(farm): State<FakeFarm>, Json(_body): Json<Value>) -> Json<Value> {
        let current = farm.productivity.lock().unwrap().clone();
        let list = match current {
            Some(hp) => vec![serde_json::to_value(&hp).unwrap()],
            None => vec![],
        };
        Json(json!({ "data": { "getHenProductivityByHen": list } }))
    }

    async fn hp_post(State(farm): State<FakeFarm>, Json(body): Json<Value>) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some("hp-1".to_string());
        *farm.productivity.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn hp_put(
        Path(id): Path<String>,
        State(farm): State<FakeFarm>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some(id);
        *farm.productivity.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn start_farm(farm: FakeFarm) -> String {
        let router = Router::new()
            .route("/lay_report/graph", post(lay_report_graph))
            .route("/hen_productivity/graph", post(hp_graph))
            .route("/hen_productivity/api", post(hp_post))
            .route("/hen_productivity/api/:id", put(hp_put))
            .with_state(farm);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn cfg(base: &str) -> WorkerConfig {
        WorkerConfig::from_lookup(move |k| match k {
            "SOURCE_GRAPHQL_URL" | "TARGET_REST_URL" | "TARGET_GRAPHQL_URL" => {
                Some(base.to_string())
            }
            _ => None,
        })
    }

    #[tokio::test]
    async fn process_batch_folds_and_writes_then_commits_the_offset() {
        let broker = broker();
        let farm = FakeFarm::default();
        farm.lay_reports.lock().unwrap().insert(
            "lr-1".to_string(),
            json!({"henId": "hen-1", "eggs": 3, "timeOfDay": "2026-07-22T08:00:00Z"}),
        );
        let base = start_farm(farm.clone()).await;
        let c = cfg(&base);
        let client = reqwest::Client::new();

        publish_thin_event(&broker, "lr-1", 1000);

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&[c.topic.as_str()]).unwrap();
        let n = process_batch(&mut consumer, &client, &c).await.unwrap();
        assert_eq!(n, 1);

        let hp = farm.productivity.lock().unwrap().clone().unwrap();
        assert_eq!(hp.total_eggs, 3);
        // last_laid_at is sourced from the ChangeEvent's own created_at
        // (1000ms epoch, from publish_thin_event above), NOT from
        // timeOfDay — see the reconciliation note at the top of this plan
        // for why: timeOfDay is a morning/afternoon/evening enum on two of
        // the three landed farm retrofits, not a timestamp.
        assert_eq!(hp.last_laid_at, "1970-01-01T00:00:01.000Z");

        // A fresh consumer for the SAME group must see nothing new — the
        // offset was committed.
        let mut consumer2 = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer2.subscribe(&[c.topic.as_str()]).unwrap();
        let n2 = process_batch(&mut consumer2, &client, &c).await.unwrap();
        assert_eq!(
            n2, 0,
            "committed offset must not be replayed by a fresh consumer"
        );
    }

    #[tokio::test]
    async fn process_batch_does_not_commit_when_detail_lookup_fails() {
        // No lay_reports registered on the fake farm -> getLayReport(lr-x)
        // returns null -> fetch_lay_report errors -> the whole batch must
        // be abandoned WITHOUT a commit, so the same event is retried by
        // a fresh consumer next tick rather than silently skipped.
        let broker = broker();
        let farm = FakeFarm::default();
        let base = start_farm(farm).await;
        let c = cfg(&base);
        let client = reqwest::Client::new();

        publish_thin_event(&broker, "lr-missing", 1000);

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&[c.topic.as_str()]).unwrap();
        assert!(process_batch(&mut consumer, &client, &c).await.is_err());

        // A FRESH consumer (per the documented retry contract — never reuse
        // the failed one) must still see the event.
        let mut retry_consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        retry_consumer.subscribe(&[c.topic.as_str()]).unwrap();
        let records = retry_consumer.poll(Duration::from_millis(50)).unwrap();
        assert_eq!(
            records.len(),
            1,
            "the un-committed event must still be there to retry"
        );
    }

    #[tokio::test]
    async fn process_batch_skips_a_record_with_an_out_of_range_created_at_without_panicking() {
        // A record whose created_at can't be rendered as a DateTime<Utc>
        // (e.g. corrupted/malformed data on the wire) must be logged and
        // skipped, exactly like unparseable JSON or an unexpected deleted
        // flag — never panic. A panic here would crash the whole worker
        // process (run_forever has no tokio::spawn isolation) and, since
        // it would happen before commit_sync(), the same poison record
        // would be refetched and re-panic on every restart forever.
        let broker = broker();
        let farm = FakeFarm::default();
        let base = start_farm(farm).await;
        let c = cfg(&base);
        let client = reqwest::Client::new();

        publish_thin_event(&broker, "lr-poison", i64::MAX);

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&[c.topic.as_str()]).unwrap();
        let n = process_batch(&mut consumer, &client, &c).await.unwrap();
        assert_eq!(
            n, 0,
            "the poison record must be skipped, not counted as processed"
        );

        // A fresh consumer for the SAME group must see nothing new — a
        // skip (unlike a batch failure) still commits, exactly like the
        // unparseable-JSON and deleted-flag skip paths.
        let mut consumer2 = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: c.group_id.clone(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer2.subscribe(&[c.topic.as_str()]).unwrap();
        let n2 = process_batch(&mut consumer2, &client, &c).await.unwrap();
        assert_eq!(
            n2, 0,
            "the skipped record's offset must have been committed"
        );
    }
}

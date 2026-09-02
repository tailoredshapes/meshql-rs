//! Throwaway latency/cost benchmark harness for `meshql-dynamo` against real
//! DynamoDB.
//!
//! Runs as a Lambda (`provided.al2023`, arm64) when `AWS_LAMBDA_RUNTIME_API` is
//! set, and as a CLI otherwise — the same code path either way, which is the
//! whole point: the vantage check compares in-region Lambda numbers against
//! workstation numbers for *identical* code.
//!
//! Nothing here is shipped. It does not modify `meshql-dynamo`; the parallel
//! `Scan` prototype lives in this crate precisely so the shipped `scan_latest`
//! stays as it is.

use aws_sdk_dynamodb::types::{
    AttributeValue, PutRequest, ReturnConsumedCapacity, Select, WriteRequest,
};
use aws_sdk_dynamodb::Client;
use chrono::{DateTime, TimeZone, Utc};
use futures::stream::{self, StreamExt};
use meshql_core::{Envelope, Stash};
use meshql_dynamo::metering::{item_size_bytes, CapacityMeter};
use meshql_dynamo::store::{envelope_to_item, now_cutoff_nanos, query_latest, scan_latest, upper_bound, SK};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- item shape

/// Epoch the synthetic version timestamps count from.
fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()
}

fn id_for(n: u64) -> String {
    format!("id-{n:08}")
}

fn envelope(id: &str, created_at: DateTime<Utc>, seq: u64, pad: usize) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(format!("row-{seq}")));
    payload.insert("seq".to_string(), json!(seq));
    payload.insert("pad".to_string(), json!("x".repeat(pad)));
    Envelope {
        id: id.to_string(),
        payload,
        created_at,
        deleted: false,
        auth: vec!["bench".to_string()],
    }
}

/// Build an item whose *predicted* billed size is as close to `target` bytes as
/// a single padding string allows, using `meshql_dynamo`'s own size predictor so
/// the number is the one the crate's cost model claims.
fn make_item(id: &str, created_at: DateTime<Utc>, seq: u64, target: u64) -> HashMap<String, AttributeValue> {
    let bare = item_size_bytes(&envelope_to_item(&envelope(id, created_at, seq, 0)));
    let pad = target.saturating_sub(bare) as usize;
    envelope_to_item(&envelope(id, created_at, seq, pad))
}

/// The (id_index, version) that item number `i` of a population run covers.
/// Interleaved by id so that any 25 consecutive items land on 25 different
/// partition keys.
fn item_coords(i: u64, id_count: u64, _versions: u64) -> (u64, u64) {
    (i % id_count, i / id_count)
}

fn created_for(id_index: u64, version: u64) -> DateTime<Utc> {
    base_time() + chrono::Duration::seconds((id_index * 16 + version) as i64)
}

// ------------------------------------------------------------------ stats

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn summarise(mut samples: Vec<f64>) -> Value {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n.max(1) as f64;
    json!({
        "n": n,
        "min_ms": samples.first().copied().unwrap_or(f64::NAN),
        "p50_ms": pct(&samples, 0.50),
        "p90_ms": pct(&samples, 0.90),
        "p99_ms": pct(&samples, 0.99),
        "max_ms": samples.last().copied().unwrap_or(f64::NAN),
        "mean_ms": mean,
    })
}

// ------------------------------------------------------------------ client

async fn client() -> Client {
    let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .retry_config(aws_config::retry::RetryConfig::standard().with_max_attempts(8))
        .timeout_config(
            aws_config::timeout::TimeoutConfig::builder()
                .operation_attempt_timeout(Duration::from_secs(30))
                .operation_timeout(Duration::from_secs(120))
                .build(),
        )
        .load()
        .await;
    Client::new(&cfg)
}

// ---------------------------------------------------------------- commands

async fn cmd_ensure(c: &Client, tables: &[String]) -> Value {
    let mut out = vec![];
    for t in tables {
        let r = meshql_dynamo::store::ensure_table(c, t).await;
        out.push(json!({"table": t, "ok": r.is_ok(), "err": r.err().map(|e| e.to_string())}));
    }
    json!({ "ensured": out })
}

/// Single `PutItem` and single by-id `Query` latency. The vantage check.
async fn cmd_vantage(c: &Client, table: &str, samples: usize, target: u64) -> Value {
    let cutoff = now_cutoff_nanos();
    let tag = format!("vantage-{}", Utc::now().timestamp());

    // Warm the connection pool / TLS / credentials before timing anything.
    for w in 0..5 {
        let item = make_item(&format!("{tag}-warm-{w}"), Utc::now(), w, target);
        let _ = c.put_item().table_name(table).set_item(Some(item)).send().await;
        let _ = query_latest(c, table, &format!("{tag}-warm-{w}"), cutoff, None).await;
    }

    let mut put = Vec::with_capacity(samples);
    let mut qry = Vec::with_capacity(samples);
    let mut sizes = vec![];
    for i in 0..samples {
        let id = format!("{tag}-{i}");
        let item = make_item(&id, Utc::now(), i as u64, target);
        sizes.push(item_size_bytes(&item));
        let t = Instant::now();
        c.put_item().table_name(table).set_item(Some(item)).send().await.unwrap();
        put.push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let got = query_latest(c, table, &id, cutoff, None).await.unwrap();
        qry.push(t.elapsed().as_secs_f64() * 1000.0);
        // cutoff is fixed at entry, so a just-written item may or may not be
        // inside it; we only care about the round trip, not the result.
        let _ = got;
    }

    json!({
        "where": vantage_label(),
        "table": table,
        "items_written": samples + 5,
        "predicted_item_bytes": sizes.first(),
        "put_item": summarise(put),
        "query_latest": summarise(qry),
    })
}

fn vantage_label() -> String {
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok() {
        format!(
            "lambda:{}:{}MB",
            std::env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap_or_default(),
            std::env::var("AWS_LAMBDA_FUNCTION_MEMORY_SIZE").unwrap_or_default()
        )
    } else {
        "workstation".to_string()
    }
}

/// Populate `[id_start, id_start+id_count)` with `versions` versions each via
/// `BatchWriteItem`, retrying `UnprocessedItems` and throttles.
#[allow(clippy::too_many_arguments)]
async fn cmd_populate(
    c: &Client,
    table: &str,
    id_start: u64,
    id_count: u64,
    versions: u64,
    target: u64,
    concurrency: usize,
    deadline_s: u64,
) -> Value {
    let total = id_count * versions;
    let batches = total.div_ceil(25);
    let written = Arc::new(AtomicU64::new(0));
    let wru = Arc::new(AtomicU64::new(0)); // milli-units
    let unproc_rounds = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let stopped = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let first_size = Arc::new(AtomicU64::new(0));

    stream::iter(0..batches)
        .map(|b| {
            let c = c.clone();
            let table = table.to_string();
            let written = written.clone();
            let wru = wru.clone();
            let unproc_rounds = unproc_rounds.clone();
            let errors = errors.clone();
            let stopped = stopped.clone();
            let first_size = first_size.clone();
            async move {
                if stopped.load(Ordering::Relaxed) > 0 {
                    return;
                }
                if start.elapsed().as_secs() > deadline_s {
                    stopped.store(1, Ordering::Relaxed);
                    return;
                }
                let lo = b * 25;
                let hi = ((b + 1) * 25).min(total);
                let mut reqs = Vec::with_capacity((hi - lo) as usize);
                for i in lo..hi {
                    let (idx, v) = item_coords(i, id_count, versions);
                    let id = id_for(id_start + idx);
                    let item = make_item(&id, created_for(id_start + idx, v), v, target);
                    if first_size.load(Ordering::Relaxed) == 0 {
                        first_size.store(item_size_bytes(&item), Ordering::Relaxed);
                    }
                    reqs.push(
                        WriteRequest::builder()
                            .put_request(PutRequest::builder().set_item(Some(item)).build().unwrap())
                            .build(),
                    );
                }

                let mut pending = reqs;
                let mut attempt = 0u32;
                while !pending.is_empty() && attempt < 12 {
                    let n_sent = pending.len() as u64;
                    let out = c
                        .batch_write_item()
                        .request_items(&table, pending.clone())
                        .return_consumed_capacity(ReturnConsumedCapacity::Total)
                        .send()
                        .await;
                    match out {
                        Ok(o) => {
                            for cap in o.consumed_capacity() {
                                if let Some(u) = cap.capacity_units() {
                                    wru.fetch_add((u * 1000.0).round() as u64, Ordering::Relaxed);
                                }
                            }
                            let left = o
                                .unprocessed_items()
                                .and_then(|m| m.get(&table))
                                .cloned()
                                .unwrap_or_default();
                            written.fetch_add(n_sent - left.len() as u64, Ordering::Relaxed);
                            if left.is_empty() {
                                return;
                            }
                            unproc_rounds.fetch_add(1, Ordering::Relaxed);
                            pending = left;
                        }
                        Err(_) => {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(
                        (25u64 << attempt.min(6)).min(2000),
                    ))
                    .await;
                }
                if !pending.is_empty() {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

    let elapsed = start.elapsed().as_secs_f64();
    let w = written.load(Ordering::Relaxed);
    json!({
        "table": table,
        "id_start": id_start,
        "id_count": id_count,
        "versions": versions,
        "requested_items": total,
        "items_written": w,
        "complete": stopped.load(Ordering::Relaxed) == 0 && w == total,
        "predicted_item_bytes": first_size.load(Ordering::Relaxed),
        "wru_consumed": wru.load(Ordering::Relaxed) as f64 / 1000.0,
        "unprocessed_retry_rounds": unproc_rounds.load(Ordering::Relaxed),
        "errors": errors.load(Ordering::Relaxed),
        "elapsed_s": elapsed,
        "items_per_s": w as f64 / elapsed,
    })
}

/// Exact item count via a paginated `Select=COUNT` scan. Also the honest way to
/// learn the real billed byte total: RRU is charged on bytes examined.
async fn cmd_count(c: &Client, table: &str) -> Value {
    let start = Instant::now();
    let mut count: u64 = 0;
    let mut scanned: u64 = 0;
    let mut pages: u64 = 0;
    let mut rru = 0.0f64;
    let mut key = None;
    loop {
        let mut req = c
            .scan()
            .table_name(table)
            .select(Select::Count)
            .return_consumed_capacity(ReturnConsumedCapacity::Total);
        if let Some(k) = key.take() {
            req = req.set_exclusive_start_key(Some(k));
        }
        let out = req.send().await.unwrap();
        pages += 1;
        count += out.count() as u64;
        scanned += out.scanned_count() as u64;
        rru += out.consumed_capacity().and_then(|c| c.capacity_units()).unwrap_or(0.0);
        match out.last_evaluated_key() {
            Some(k) if !k.is_empty() => key = Some(k.clone()),
            _ => break,
        }
    }
    json!({
        "table": table,
        "count": count,
        "scanned_count": scanned,
        "pages": pages,
        "rru": rru,
        // RRU = ceil(bytes/4096)/2 aggregated per page, so bytes ~ rru*2*4096.
        "implied_bytes": rru * 2.0 * 4096.0,
        "implied_bytes_per_item": if count > 0 { rru * 2.0 * 4096.0 / count as f64 } else { 0.0 },
        "elapsed_s": start.elapsed().as_secs_f64(),
    })
}

/// by-id `query_latest`, and `read_many` = k concurrent `query_latest`.
async fn cmd_reads(c: &Client, table: &str, id_count: u64, samples: usize, ks: &[usize]) -> Value {
    let cutoff = now_cutoff_nanos();
    // warm
    for i in 0..5u64 {
        let _ = query_latest(c, table, &id_for(i), cutoff, None).await;
    }

    let mut one = Vec::with_capacity(samples);
    let mut misses = 0u64;
    for i in 0..samples {
        let id = id_for((i as u64 * 7919) % id_count);
        let t = Instant::now();
        let got = query_latest(c, table, &id, cutoff, None).await.unwrap();
        one.push(t.elapsed().as_secs_f64() * 1000.0);
        if got.is_none() {
            misses += 1;
        }
    }

    let mut many = serde_json::Map::new();
    for &k in ks {
        let mut lat = Vec::with_capacity(samples);
        let mut hit = 0u64;
        for s in 0..samples {
            let ids: Vec<String> = (0..k)
                .map(|j| id_for(((s * k + j) as u64 * 7919) % id_count))
                .collect();
            let t = Instant::now();
            let res = futures::future::join_all(
                ids.iter().map(|id| query_latest(c, table, id, cutoff, None)),
            )
            .await;
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
            hit += res.iter().filter(|r| matches!(r, Ok(Some(_)))).count() as u64;
        }
        many.insert(
            format!("k{k}"),
            json!({"stats": summarise(lat), "envelopes_resolved": hit}),
        );
    }

    json!({
        "table": table,
        "query_latest": summarise(one),
        "query_latest_misses": misses,
        "read_many": many,
    })
}

/// The shipped `scan_latest`, timed. `CapacityMeter::requests` for `Op::Scan`
/// *is* the page count, which is the round-trip count that sets the latency.
async fn cmd_scan(c: &Client, table: &str, samples: usize) -> Value {
    let cutoff = now_cutoff_nanos();
    let mut runs = vec![];
    let mut wall = Vec::with_capacity(samples);
    for _ in 0..samples {
        let meter = CapacityMeter::new();
        let t = Instant::now();
        let res = scan_latest(c, table, cutoff, Some(&meter)).await.unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        wall.push(ms);
        let rep = meter.snapshot();
        runs.push(json!({
            "ms": ms,
            "pages": rep.scan.requests,
            "rru": rep.scan.capacity_units,
            "envelopes": res.len(),
            "pages_per_s": rep.scan.requests as f64 / (ms / 1000.0),
        }));
    }
    json!({ "table": table, "wall": summarise(wall), "runs": runs })
}

/// Parallel `Scan` prototype: `Segment`/`TotalSegments`, segments run
/// concurrently, latest-per-id merged across segments in Rust. Deliberately a
/// separate implementation from `meshql_dynamo::store::scan_latest`.
async fn parallel_scan(
    c: &Client,
    table: &str,
    cutoff: i64,
    total_segments: i32,
) -> (usize, u64, f64) {
    let hi = upper_bound(cutoff);
    let pages = Arc::new(AtomicU64::new(0));
    let milli_rru = Arc::new(AtomicU64::new(0));

    let futs = (0..total_segments).map(|seg| {
        let c = c.clone();
        let table = table.to_string();
        let hi = hi.clone();
        let pages = pages.clone();
        let milli_rru = milli_rru.clone();
        async move {
            let mut latest: HashMap<String, (String, Envelope)> = HashMap::new();
            let mut key = None;
            loop {
                let mut req = c
                    .scan()
                    .table_name(&table)
                    .filter_expression("#sk < :hi")
                    .expression_attribute_names("#sk", SK)
                    .expression_attribute_values(":hi", AttributeValue::S(hi.clone()))
                    .return_consumed_capacity(ReturnConsumedCapacity::Total)
                    .segment(seg)
                    .total_segments(total_segments);
                if let Some(k) = key.take() {
                    req = req.set_exclusive_start_key(Some(k));
                }
                let out = req.send().await.unwrap();
                pages.fetch_add(1, Ordering::Relaxed);
                if let Some(u) = out.consumed_capacity().and_then(|c| c.capacity_units()) {
                    milli_rru.fetch_add((u * 1000.0).round() as u64, Ordering::Relaxed);
                }
                for item in out.items() {
                    let sk = match item.get(SK) {
                        Some(AttributeValue::S(s)) => s.clone(),
                        _ => continue,
                    };
                    let env = meshql_dynamo::store::item_to_envelope(item).unwrap();
                    match latest.get(&env.id) {
                        Some((seen, _)) if *seen >= sk => {}
                        _ => {
                            latest.insert(env.id.clone(), (sk, env));
                        }
                    }
                }
                match out.last_evaluated_key() {
                    Some(k) if !k.is_empty() => key = Some(k.clone()),
                    _ => break,
                }
            }
            latest
        }
    });

    let per_segment = futures::future::join_all(futs).await;
    let mut merged: HashMap<String, (String, Envelope)> = HashMap::new();
    for seg in per_segment {
        for (id, (sk, env)) in seg {
            match merged.get(&id) {
                Some((seen, _)) if *seen >= sk => {}
                _ => {
                    merged.insert(id, (sk, env));
                }
            }
        }
    }
    let n = merged.values().filter(|(_, e)| !e.deleted).count();
    (
        n,
        pages.load(Ordering::Relaxed),
        milli_rru.load(Ordering::Relaxed) as f64 / 1000.0,
    )
}

async fn cmd_pscan(c: &Client, table: &str, segs: &[i32], samples: usize) -> Value {
    let cutoff = now_cutoff_nanos();
    let mut out = vec![];
    for &s in segs {
        let mut wall = vec![];
        let mut last = (0usize, 0u64, 0.0f64);
        for _ in 0..samples {
            let t = Instant::now();
            last = parallel_scan(c, table, cutoff, s).await;
            wall.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        out.push(json!({
            "segments": s,
            "wall": summarise(wall),
            "pages": last.1,
            "rru": last.2,
            "envelopes": last.0,
        }));
    }
    json!({ "table": table, "parallel_scan": out })
}

/// `create` = one `PutItem` of a ~1 KiB envelope. Run last: it adds items.
async fn cmd_create(c: &Client, table: &str, samples: usize, target: u64) -> Value {
    let tag = format!("zz-create-{}", Utc::now().timestamp());
    for w in 0..5u64 {
        let item = make_item(&format!("{tag}-warm-{w}"), Utc::now(), w, target);
        let _ = c.put_item().table_name(table).set_item(Some(item)).send().await;
    }
    let meter = CapacityMeter::new();
    let mut lat = Vec::with_capacity(samples);
    let mut bytes = 0u64;
    for i in 0..samples {
        let item = make_item(&format!("{tag}-{i}"), Utc::now(), i as u64, target);
        bytes = item_size_bytes(&item);
        let t = Instant::now();
        let out = c
            .put_item()
            .table_name(table)
            .set_item(Some(item))
            .return_consumed_capacity(ReturnConsumedCapacity::Total)
            .send()
            .await
            .unwrap();
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
        meter.record(
            meshql_dynamo::Op::PutItem,
            out.consumed_capacity(),
        );
    }
    let rep = meter.snapshot();
    json!({
        "table": table,
        "create": summarise(lat),
        "items_written": samples + 5,
        "predicted_item_bytes": bytes,
        "wru_total": rep.put_item.capacity_units,
        "wru_per_put": rep.put_item.capacity_units / samples as f64,
    })
}

// ------------------------------------------------------------------ dispatch

async fn dispatch(v: Value) -> Value {
    let c = client().await;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let u = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d);
    let target = u("item_bytes", 1000);
    let table = s("table");

    match s("cmd").as_str() {
        "ensure" => {
            let tables: Vec<String> = v
                .get("tables")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec![table.clone()]);
            cmd_ensure(&c, &tables).await
        }
        "vantage" => cmd_vantage(&c, &table, u("samples", 100) as usize, target).await,
        "populate" => {
            cmd_populate(
                &c,
                &table,
                u("id_start", 0),
                u("id_count", 100),
                u("versions", 10),
                target,
                u("concurrency", 128) as usize,
                u("deadline_s", 780),
            )
            .await
        }
        "count" => cmd_count(&c, &table).await,
        "reads" => {
            let ks: Vec<usize> = v
                .get("ks")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|k| k.as_u64()).map(|k| k as usize).collect())
                .unwrap_or_else(|| vec![1, 10, 100]);
            cmd_reads(&c, &table, u("id_count", 100), u("samples", 100) as usize, &ks).await
        }
        "scan" => cmd_scan(&c, &table, u("samples", 10) as usize).await,
        "pscan" => {
            let segs: Vec<i32> = v
                .get("segments")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|k| k.as_u64()).map(|k| k as i32).collect())
                .unwrap_or_else(|| vec![1, 4, 16, 64]);
            cmd_pscan(&c, &table, &segs, u("samples", 1) as usize).await
        }
        "create" => cmd_create(&c, &table, u("samples", 100) as usize, target).await,
        other => json!({ "error": format!("unknown cmd {other:?}") }),
    }
}

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok() {
        lambda_runtime::run(lambda_runtime::service_fn(
            |e: lambda_runtime::LambdaEvent<Value>| async move { Ok::<Value, lambda_runtime::Error>(dispatch(e.payload).await) },
        ))
        .await
    } else {
        let arg = std::env::args().nth(1).unwrap_or_else(|| "{}".to_string());
        let v: Value = serde_json::from_str(&arg)?;
        let out = dispatch(v).await;
        println!("{}", serde_json::to_string_pretty(&out)?);
        Ok(())
    }
}

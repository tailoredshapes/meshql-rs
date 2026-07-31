//! Predicted capacity vs. **metered** capacity, against real AWS DynamoDB.
//!
//! The certification suites point at `MESHQL_DYNAMO_ENDPOINT`, which defaults to
//! DynamoDB Local. DynamoDB Local does not meter — it returns no
//! `ConsumedCapacity` at all — so "passes all certs" attests semantics and says
//! precisely nothing about cost. This suite exists to say something about cost,
//! and it can only do that against the real service.
//!
//! Every check states a **prediction** from [`meshql_dynamo::metering`] first and
//! then compares it to what DynamoDB reported. A model nobody compared to a bill
//! is a guess; a test that cannot fail is decoration. Each check therefore
//! asserts an exact figure, not a bound.
//!
//! # Running it
//!
//! ```sh
//! MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 \
//!   cargo test -p meshql-dynamo --test capacity_cost
//! ```
//!
//! Without `MESHQL_DYNAMO_COST_TESTS=1`, or without usable AWS credentials, the
//! suite **skips and exits 0** with a message saying why. It never fails for
//! want of an account. `harness = false` is what makes that message visible:
//! libtest captures `println!` from a passing test, so a skip announced through
//! the standard harness would be invisible without `--nocapture`, which is the
//! same as not announcing it.
//!
//! # What it costs to run
//!
//! Well under a cent. The largest table it builds is ~4,000 items of ~1 KiB
//! (~4 MiB, ~4,000 WRU ≈ $0.0025), it scans that table a handful of times, and
//! it deletes every table it made. Tables are named `dynamocost-*`.

use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_dynamodb::Client;
use chrono::{DateTime, TimeZone, Utc};
use meshql_core::{Envelope, Repository, Searcher, Stash};
use meshql_dynamo::metering::{item_size_bytes, read_units, write_units};
use meshql_dynamo::{CapacityMeter, DynamoRepository, DynamoSearcher, Rates};
use serde_json::json;

// ---------------------------------------------------------------- harness ---

/// Failures are collected rather than panicked on, so one wrong prediction does
/// not hide the other nine — the point of the run is the whole table.
#[derive(Default)]
struct Checks {
    passed: usize,
    failures: Vec<String>,
}

impl Checks {
    /// `what` is the claim, `predicted` what the model said, `actual` what
    /// DynamoDB billed. Capacity units are exact multiples of 0.5, so this is an
    /// exact comparison with only a float-representation epsilon.
    fn eq(&mut self, what: &str, predicted: f64, actual: f64) {
        let ok = (predicted - actual).abs() < 1e-9;
        println!(
            "  {} {:<62} predicted {:>10.1}  metered {:>10.1}",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual
        );
        if ok {
            self.passed += 1;
        } else {
            self.failures.push(format!(
                "{what}: predicted {predicted}, DynamoDB metered {actual}"
            ));
        }
    }

    fn eq_u64(&mut self, what: &str, predicted: u64, actual: u64) {
        self.eq(what, predicted as f64, actual as f64);
    }

    /// `predicted` is a floor and `predicted * headroom` a ceiling.
    fn within(&mut self, what: &str, predicted: f64, actual: f64, headroom: f64) {
        let ok = actual >= predicted - 1e-9 && actual <= predicted * headroom + 1e-9;
        println!(
            "  {} {:<62} model {:>10.1}  metered {:>10.1} ({:+.2}%)",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual,
            (actual / predicted - 1.0) * 100.0
        );
        if ok {
            self.passed += 1;
        } else {
            self.failures.push(format!(
                "{what}: model {predicted}, metered {actual} — outside [1.00, {headroom:.2}]x"
            ));
        }
    }

    fn assert_true(&mut self, what: &str, ok: bool) {
        println!("  {} {}", if ok { "PASS" } else { "FAIL" }, what);
        if ok {
            self.passed += 1;
        } else {
            self.failures.push(what.to_string());
        }
    }
}

/// Every table this run created, so teardown can be verified against exactly
/// those and not against a shared prefix.
static CREATED_TABLES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn table_name(suffix: &str) -> String {
    let name = format!("dynamocost-{suffix}-{}", uuid::Uuid::new_v4().simple());
    CREATED_TABLES.lock().unwrap().push(name.clone());
    name
}

/// A payload whose serialised envelope lands as close to `target` bytes as the
/// padding granularity allows. Returns the payload and the *predicted* item
/// size, which is the number under test.
fn envelope_of_size(id: &str, target_bytes: u64) -> (Envelope, u64) {
    // Grow a single padding string until the predicted item size reaches the
    // target. One byte of padding is one byte of item, so this converges
    // immediately, but doing it by measurement rather than by arithmetic means
    // the fixture cannot drift when the envelope gains an attribute.
    let mut pad = 0usize;
    loop {
        let env = envelope_with_pad(id, pad);
        let size = item_size_bytes(&meshql_dynamo::store::envelope_to_item(&env));
        if size >= target_bytes || pad > 400_000 {
            return (env, size);
        }
        pad += (target_bytes - size) as usize;
    }
}

fn envelope_with_pad(id: &str, pad: usize) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("kind".to_string(), json!("bench"));
    payload.insert("pad".to_string(), json!("x".repeat(pad)));
    Envelope {
        id: id.to_string(),
        payload,
        created_at: Utc::now(),
        deleted: false,
        authorized_tokens: vec!["*".to_string()],
    }
}

/// `Envelope` has no `PartialEq`, and comparing the fields that matter is a
/// better test than deriving one would be: this is what a *client* observes.
///
/// The payload is compared **by value**, not by its serialised form.
/// `convert::map_to_object` walks the SDK's `HashMap<String, AttributeValue>`,
/// whose iteration order is randomised per process, so two reads of byte-
/// identical data render to different JSON strings while being equal as values.
/// Comparing the strings made this check fail for a reason that had nothing to
/// do with metering — which is how that was found.
fn comparable(env: &Envelope) -> (String, String, bool, Vec<String>, Stash) {
    (
        env.id.clone(),
        env.created_at.to_rfc3339(),
        env.deleted,
        env.authorized_tokens.clone(),
        env.payload.clone(),
    )
}

fn comparable_all(envs: &[Envelope]) -> Vec<(String, String, bool, Vec<String>, Stash)> {
    envs.iter().map(comparable).collect()
}

/// Read-side capacity runs a little above the write-side size model — see
/// [`calibrate_the_read_side_of_the_size_model`]. Scan predictions are therefore
/// asserted as "at least the model, and no more than 3% above it".
///
/// 3% is deliberately far tighter than any modelling error that matters: getting
/// the rounding per-item instead of aggregate is 8x, costing V instead of M is
/// 10x, and charging strongly-consistent rates is 2x. All three still go red.
const READ_MODEL_HEADROOM: f64 = 1.03;

async fn drop_table(client: &Client, table: &str) {
    if let Err(e) = meshql_dynamo::drop_table(client, table).await {
        eprintln!("WARNING: failed to drop {table}: {e} — DELETE IT BY HAND");
    }
}

// ------------------------------------------------------------- the checks ---

/// A write costs `ceil(item_size / 1 KB)` write request units, and the cliff at
/// the kilobyte is real: one byte over doubles the charge.
///
/// This check is also what validates [`item_size_bytes`]. If the size model were
/// wrong by even one byte, an item predicted at exactly 1024 would meter as 2
/// WRU and this would go red — which is why the sizes chosen sit *on* the
/// boundaries rather than comfortably inside them.
async fn write_units_at_the_kilobyte_boundary(client: &Client, checks: &mut Checks) {
    let table = table_name("write");
    let meter = CapacityMeter::new();
    let repo = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("create table")
        .with_meter(meter.clone());

    println!("\n== a write costs ceil(size / 1 KB) WRU ==");
    for target in [200u64, 1023, 1024, 1025, 2048, 2049, 3000, 4096] {
        let (env, size) = envelope_of_size(&format!("w-{target}"), target);
        let before = meter.snapshot();
        repo.create(env, &["*".to_string()]).await.expect("write");
        let delta = meter.snapshot().minus(&before);

        checks.eq(
            &format!("write of a {size}-byte item (target {target})"),
            write_units(size),
            delta.write_units(),
        );
        checks.eq_u64(
            &format!("write of a {size}-byte item is one round trip"),
            1,
            delta.put_item.requests,
        );
    }

    drop_table(client, &table).await;
}

/// The claim the whole sort-key design rests on, in money: `read(id, .., at)` is
/// **one** `Query` examining **one** item, so it is 0.5 RRU — not a scan, and
/// not a function of how many versions the id has.
///
/// The version count is the part worth pinning. An implementation that fetched
/// every version and picked the newest in Rust would pass every semantic cert
/// and cost 25× more here.
async fn a_temporal_read_is_half_an_rru_however_many_versions_exist(
    client: &Client,
    checks: &mut Checks,
) {
    let table = table_name("read");
    let meter = CapacityMeter::new();
    let repo = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("create table")
        .with_meter(meter.clone());

    println!("\n== a by-id temporal read is 0.5 RRU and 1 round trip ==");

    // 50 versions of one id, each well under 4 KB.
    let mut stamps: Vec<DateTime<Utc>> = Vec::new();
    for _ in 0..50 {
        let (env, _) = envelope_of_size("hot", 300);
        let written = repo.create(env, &["*".to_string()]).await.expect("write");
        stamps.push(written.created_at);
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    for (label, at) in [
        ("at = now", None),
        ("at = the 1st version", Some(stamps[0])),
        ("at = the 25th version", Some(stamps[24])),
        ("at = the 50th version", Some(stamps[49])),
    ] {
        let before = meter.snapshot();
        let found = repo
            .read("hot", &["*".to_string()], at)
            .await
            .expect("read");
        let delta = meter.snapshot().minus(&before);

        checks.assert_true(&format!("read {label} resolved a version"), found.is_some());
        checks.eq(
            &format!("read {label} over 50 versions costs 0.5 RRU"),
            read_units(1, true),
            delta.read_units(),
        );
        checks.eq_u64(
            &format!("read {label} is one Query, zero Scans"),
            1,
            delta.query.requests,
        );
        checks.eq_u64(
            &format!("read {label} issues no Scan at all"),
            0,
            delta.scan.requests,
        );
    }

    // read_many: k concurrent Queries, k × 0.5 RRU, and nothing superlinear.
    println!("\n== read_many(k) is k Queries at 0.5 RRU each ==");
    let ids: Vec<String> = (0..100).map(|i| format!("m-{i:04}")).collect();
    for id in &ids {
        let (env, _) = envelope_of_size(id, 300);
        repo.create(env, &["*".to_string()]).await.expect("write");
    }
    for k in [1usize, 10, 100] {
        let before = meter.snapshot();
        let found = repo
            .read_many(&ids[..k], &["*".to_string()])
            .await
            .expect("read_many");
        let delta = meter.snapshot().minus(&before);

        checks.eq_u64(
            &format!("read_many(k={k}) returned k envelopes"),
            k as u64,
            found.len() as u64,
        );
        checks.eq(
            &format!("read_many(k={k}) costs k x 0.5 RRU"),
            k as f64 * read_units(1, true),
            delta.read_units(),
        );
        checks.eq_u64(
            &format!("read_many(k={k}) is k Queries"),
            k as u64,
            delta.query.requests,
        );
    }

    drop_table(client, &table).await;
}

/// A search with no `"id"` key is a full `Scan`, and its cost is the **aggregate**
/// bytes examined rounded to 4 KB — `V·S / 8 KiB` RRU eventually consistent —
/// not `0.5 × V`.
///
/// Three separate claims are pinned here, and each is a thing a client will
/// otherwise get wrong by an order of magnitude:
///
/// 1. cost is set by **V**, the version count, not by M, the distinct-id count;
/// 2. the rounding is aggregate, so a 1 KiB item costs an *eighth* of an RRU and
///    not a half;
/// 3. the temporal `FilterExpression` reduces what comes back and **not** what
///    is billed — a search resolving zero records costs exactly what a search
///    resolving all of them costs.
async fn a_search_costs_the_aggregate_bytes_examined(client: &Client, checks: &mut Checks) {
    let table = table_name("scan");
    let meter = CapacityMeter::new();
    let repo = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("create table")
        .with_meter(meter.clone());
    let searcher = DynamoSearcher::new_with_client(client.clone(), &table)
        .await
        .expect("searcher")
        .with_meter(meter.clone());

    println!("\n== a search costs the aggregate bytes examined ==");

    // V = 400 versions over M = 40 ids, so a cost model keyed on M rather than V
    // is out by 10x and cannot pass.
    let m = 40usize;
    let versions_per_id = 10usize;
    let mut total_bytes = 0u64;
    for v in 0..versions_per_id {
        for i in 0..m {
            let (mut env, size) = envelope_of_size(&format!("s-{i:04}"), 1024);
            env.payload.insert("gen".to_string(), json!(format!("{v}")));
            let item = meshql_dynamo::store::envelope_to_item(&env);
            total_bytes += item_size_bytes(&item);
            let _ = size;
            repo.create(env, &["*".to_string()]).await.expect("write");
        }
    }
    let v_total = (m * versions_per_id) as u64;
    println!(
        "  (V = {v_total} versions over M = {m} ids, {total_bytes} bytes total, \
         {:.0} bytes/item)",
        total_bytes as f64 / v_total as f64
    );

    // One page: 400 KiB is far inside the 1 MiB page limit, so the aggregate
    // rounding is exact and the round trip count is 1.
    let before = meter.snapshot();
    let results = searcher
        .find_all(
            r#"{"payload.kind": "bench"}"#,
            &Stash::new(),
            &["*".to_string()],
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("search");
    let delta = meter.snapshot().minus(&before);

    checks.eq_u64(
        "a search resolves M records, not V",
        m as u64,
        results.len() as u64,
    );
    checks.within(
        "a search costs ~ceil(aggregate bytes / 4 KB) x 0.5 RRU",
        read_units(total_bytes, true),
        delta.read_units(),
        READ_MODEL_HEADROOM,
    );
    checks.assert_true(
        "...which is 8x cheaper than per-item rounding would be",
        (delta.read_units() - 0.5 * v_total as f64).abs() > 1.0,
    );
    checks.eq_u64(
        "a 400 KiB table scans in one round trip",
        1,
        delta.scan.requests,
    );
    checks.eq_u64("a search issues no Query at all", 0, delta.query.requests);

    // The headline for the decision rule: a search whose cutoff excludes every
    // version still pays for every version.
    let before = meter.snapshot();
    let empty = searcher
        .find_all(
            r#"{"payload.kind": "bench"}"#,
            &Stash::new(),
            &["*".to_string()],
            Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis(),
        )
        .await
        .expect("search");
    let past = meter.snapshot().minus(&before);

    checks.eq_u64(
        "a search at at=2000-01-01 resolves nothing",
        0,
        empty.len() as u64,
    );
    checks.eq(
        "...and is billed exactly the same as the search that returned everything",
        delta.read_units(),
        past.read_units(),
    );

    // A limit does not bound the cost either: it is applied after resolution and
    // after visibility, so it cannot be pushed into the scan.
    let mut limited = Stash::new();
    limited.insert("limit".to_string(), json!(1));
    let before = meter.snapshot();
    let one = searcher
        .find_all(
            r#"{"payload.kind": "bench"}"#,
            &limited,
            &["*".to_string()],
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("search");
    let with_limit = meter.snapshot().minus(&before);

    checks.eq_u64("a limit:1 search returns 1 record", 1, one.len() as u64);
    checks.eq(
        "...and costs the same as the unlimited search: a limit is not a budget",
        delta.read_units(),
        with_limit.read_units(),
    );

    // And the one safe pushdown: an "id" key takes the Query path.
    println!("\n== a template carrying `id` takes the Query path ==");
    let before = meter.snapshot();
    let by_id = searcher
        .find_all(
            r#"{"id": "s-0007"}"#,
            &Stash::new(),
            &["*".to_string()],
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("search by id");
    let pushdown = meter.snapshot().minus(&before);

    checks.eq_u64(
        "a search on `id` returns the one record",
        1,
        by_id.len() as u64,
    );
    checks.eq(
        "a search on `id` costs 0.5 RRU, not a scan",
        read_units(1, true),
        pushdown.read_units(),
    );
    checks.eq_u64("a search on `id` is one Query", 1, pushdown.query.requests);
    checks.eq_u64(
        "a search on `id` issues ZERO Scans",
        0,
        pushdown.scan.requests,
    );
    checks.assert_true(
        &format!(
            "the pushdown is {:.0}x cheaper than the scan it replaces",
            delta.read_units() / pushdown.read_units()
        ),
        pushdown.read_units() * 20.0 < delta.read_units(),
    );

    // Money, at the rates the Price List API returned on 2026-07-30.
    let rates = Rates::ON_DEMAND_US_EAST_1;
    println!(
        "\n  one scan at V={v_total}: {:.1} RRU = ${:.8}; extrapolated to V=1,000,000: ${:.6}",
        delta.read_units(),
        delta.cost_usd(&rates),
        delta.cost_usd(&rates) * (1_000_000.0 / v_total as f64)
    );

    drop_table(client, &table).await;
}

/// Metering must not be able to change an answer.
///
/// `ReturnConsumedCapacity` adds a field to a *response*; it cannot alter which
/// items a request matches. That is an argument, not evidence, so this runs the
/// same reads and the same search twice over one table — once through a metered
/// handle and once through an unmetered one — and requires the results to be
/// identical.
async fn metering_does_not_change_results(client: &Client, checks: &mut Checks) {
    let table = table_name("nosem");
    let meter = CapacityMeter::new();
    let plain = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("create table");
    let metered = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("repo")
        .with_meter(meter.clone());
    let plain_search = DynamoSearcher::new_with_client(client.clone(), &table)
        .await
        .expect("searcher");
    let metered_search = DynamoSearcher::new_with_client(client.clone(), &table)
        .await
        .expect("searcher")
        .with_meter(meter.clone());

    println!("\n== metering cannot change semantics ==");

    for i in 0..20 {
        let (env, _) = envelope_of_size(&format!("n-{i:03}"), 400);
        plain.create(env, &["*".to_string()]).await.expect("write");
    }
    // A tombstone and a superseded version, so the comparison covers the two
    // cases where version resolution actually has to decide something.
    let (env, _) = envelope_of_size("n-000", 500);
    plain.create(env, &["*".to_string()]).await.expect("write");
    plain
        .remove("n-001", &["*".to_string()])
        .await
        .expect("remove");

    // Reads here are eventually consistent — this crate never sets
    // ConsistentRead — so two Scans taken moments apart can legitimately
    // disagree while a write burst propagates. That is a real property clients
    // must know about, and it is *not* what this check is about, so settle
    // first. Without this sleep the check fails, which is how the property was
    // found.
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let tokens = ["*".to_string()];
    let a = plain.list(&tokens).await.expect("list");
    let b = metered.list(&tokens).await.expect("list");
    checks.assert_true(
        "list() agrees with and without a meter",
        comparable_all(&a) == comparable_all(&b),
    );

    // A pinned cutoff, not `None`: `None` means "now", and the two calls have
    // different nows.
    let pinned = Some(Utc::now());
    let a = plain.read("n-000", &tokens, pinned).await.expect("read");
    let b = metered.read("n-000", &tokens, pinned).await.expect("read");
    checks.assert_true(
        "read() agrees with and without a meter",
        a.as_ref().map(comparable) == b.as_ref().map(comparable),
    );

    let ids: Vec<String> = (0..20).map(|i| format!("n-{i:03}")).collect();
    let a = plain.read_many(&ids, &tokens).await.expect("read_many");
    let b = metered.read_many(&ids, &tokens).await.expect("read_many");
    checks.assert_true(
        "read_many() agrees with and without a meter",
        comparable_all(&a) == comparable_all(&b),
    );

    let now = Utc::now().timestamp_millis();
    let a = plain_search
        .find_all(r#"{"payload.kind": "bench"}"#, &Stash::new(), &tokens, now)
        .await
        .expect("search");
    let b = metered_search
        .find_all(r#"{"payload.kind": "bench"}"#, &Stash::new(), &tokens, now)
        .await
        .expect("search");
    checks.assert_true("find_all() agrees with and without a meter", a == b);

    let a = plain_search
        .find(r#"{"id": "n-005"}"#, &Stash::new(), &tokens, now)
        .await
        .expect("find");
    let b = metered_search
        .find(r#"{"id": "n-005"}"#, &Stash::new(), &tokens, now)
        .await
        .expect("find");
    checks.assert_true("find() agrees with and without a meter", a == b);

    // And the unmetered handle contributed nothing to the meter, which is the
    // "costs nothing when unused" claim in machine-checkable form.
    let report = meter.snapshot();
    checks.assert_true(
        "only the metered handle's requests were counted",
        report.round_trips() > 0,
    );

    drop_table(client, &table).await;
}

/// A paginated `Scan` charges per page, and the pages are chained.
///
/// The round-trip count is the latency model in `docs/cost-model-dynamodb.md`,
/// so it is asserted here rather than assumed: build a table larger than one
/// 1 MiB page and require the scan to report more than one `Scan` request, with
/// the count matching `ceil(bytes / 1 MiB)`.
async fn a_scan_pages_at_one_mebibyte(client: &Client, checks: &mut Checks) {
    let table = table_name("pages");
    let meter = CapacityMeter::new();
    let repo = DynamoRepository::new_with_client(client.clone(), &table)
        .await
        .expect("create table")
        .with_meter(meter.clone());

    println!("\n== a scan pages at 1 MiB, and the pages are serial ==");

    // ~3.5 MiB, so 4 pages. Written concurrently or this takes a minute.
    let n = 3_500usize;
    let mut total_bytes = 0u64;
    let repo = Arc::new(repo);
    let mut batch = Vec::new();
    for i in 0..n {
        let (env, _) = envelope_of_size(&format!("p-{i:05}"), 1024);
        total_bytes += item_size_bytes(&meshql_dynamo::store::envelope_to_item(&env));
        let repo = repo.clone();
        batch.push(tokio::spawn(async move {
            repo.create(env, &["*".to_string()]).await.expect("write");
        }));
        if batch.len() == 100 {
            for t in batch.drain(..) {
                t.await.expect("join");
            }
        }
    }
    for t in batch.drain(..) {
        t.await.expect("join");
    }

    let expected_pages = total_bytes.div_ceil(1024 * 1024);
    let started = std::time::Instant::now();
    let before = meter.snapshot();
    let listed = repo.list(&["*".to_string()]).await.expect("list");
    let elapsed = started.elapsed();
    let delta = meter.snapshot().minus(&before);

    checks.eq_u64("the scan resolved every id", n as u64, listed.len() as u64);
    checks.within(
        "a multi-page scan costs the same aggregate RRU",
        read_units(total_bytes, true),
        delta.read_units(),
        READ_MODEL_HEADROOM,
    );
    checks.assert_true(
        &format!(
            "a {:.2} MiB table takes {} scan round trips (predicted ~{expected_pages})",
            total_bytes as f64 / (1024.0 * 1024.0),
            delta.scan.requests
        ),
        delta.scan.requests >= expected_pages,
    );
    println!(
        "  serial page walk: {} pages in {:.0} ms = {:.1} ms/page",
        delta.scan.requests,
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / delta.scan.requests.max(1) as f64
    );

    drop_table(client, &table).await;
}

/// Calibrate the *read* side of the size model against the *write* side.
///
/// `item_size_bytes` is validated exactly by the write checks: an item the model
/// calls 1024 bytes bills 1 WRU and one it calls 1025 bills 2, so the model is
/// right to the byte for `PutItem`. Scans, however, meter slightly higher than
/// the same arithmetic predicts, which means DynamoDB reads items at a size that
/// is not identical to the size it writes them at.
///
/// Rather than fit a fudge factor to one measurement, this solves for it: two
/// tables of *identical* items, N = 200 and N = 400, give two equations in
/// per-request overhead and per-item overhead. Every item shares one
/// `created_at`, because `DateTime::to_rfc3339` emits 0, 3, 6 or 9 fractional
/// digits depending on the value, so envelopes built at different instants
/// differ in size by up to 10 bytes and would smear the result.
async fn calibrate_the_read_side_of_the_size_model(client: &Client, checks: &mut Checks) {
    println!("\n== calibrating: does a Scan meter the same size a PutItem does? ==");

    let fixed = Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap();
    let mut measurements = Vec::new();

    for n in [200usize, 400usize] {
        let table = table_name("calib");
        let meter = CapacityMeter::new();
        let repo = DynamoRepository::new_with_client(client.clone(), &table)
            .await
            .expect("create table")
            .with_meter(meter.clone());

        // Identical items but for the id, which is fixed-width.
        let mut per_item = 0u64;
        for i in 0..n {
            let mut payload = Stash::new();
            payload.insert("pad".to_string(), json!("x".repeat(900)));
            let env = Envelope {
                id: format!("c-{i:05}"),
                payload,
                created_at: fixed,
                deleted: false,
                authorized_tokens: vec!["*".to_string()],
            };
            per_item = item_size_bytes(&meshql_dynamo::store::envelope_to_item(&env));
            repo.create(env, &["*".to_string()]).await.expect("write");
        }

        let before = meter.snapshot();
        let listed = repo.list(&["*".to_string()]).await.expect("list");
        let delta = meter.snapshot().minus(&before);
        assert_eq!(listed.len(), n, "calibration table is the wrong size");

        // metered RRU -> read units -> a byte interval
        let units = (delta.read_units() * 2.0).round() as u64;
        let hi = units * 4096;
        let lo = hi.saturating_sub(4096) + 1;
        let modelled = per_item * n as u64;
        println!(
            "  N={n:4} pages={} model {modelled} B, metered bytes in [{lo}, {hi}] \
             => overhead/item in [{:.2}, {:.2}] B",
            delta.scan.requests,
            (lo.saturating_sub(modelled)) as f64 / n as f64,
            (hi - modelled) as f64 / n as f64
        );
        measurements.push((n as f64, modelled as f64, lo as f64, hi as f64));
        drop_table(client, &table).await;
    }

    // Two intervals; report the intersection of the implied per-item overheads.
    let (n1, m1, l1, h1) = measurements[0];
    let (n2, m2, l2, h2) = measurements[1];
    let lo = ((l1 - m1) / n1).max((l2 - m2) / n2);
    let hi = ((h1 - m1) / n1).min((h2 - m2) / n2);
    println!("  => per-item read overhead is in [{lo:.2}, {hi:.2}] bytes");
    // Assert on the *intersection*, not on either interval alone: a single N
    // only localises the overhead to within 4096/N bytes per item, which at
    // N=200 is 20 B of slack and cannot resolve a 20 B effect. Two N do.
    let item = m1 / n1;
    checks.assert_true(
        &format!(
            "the two measurements agree (per-item read overhead [{lo:.1}, {hi:.1}] B \
             on a ~{item:.0} B item)",
        ),
        lo <= hi && lo > 0.0,
    );
    checks.assert_true(
        &format!(
            "a Scan meters within 3% of the write-side size model (+{:.1}%..+{:.1}%)",
            100.0 * lo / item,
            100.0 * hi / item
        ),
        hi / item < 0.03,
    );
}

// -------------------------------------------------------------------- main ---

#[tokio::main]
async fn main() {
    if std::env::var("MESHQL_DYNAMO_COST_TESTS").as_deref() != Ok("1") {
        println!(
            "SKIPPED: meshql-dynamo capacity-cost suite.\n  \
             Reason: MESHQL_DYNAMO_COST_TESTS is not set to 1.\n  \
             These checks bill a real AWS account (well under a cent) because \
             DynamoDB Local does not\n  report ConsumedCapacity and therefore \
             cannot validate a cost model.\n  \
             Run with: MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 cargo test \
             -p meshql-dynamo --test capacity_cost"
        );
        return;
    }

    if let Ok(endpoint) = std::env::var("MESHQL_DYNAMO_ENDPOINT") {
        println!(
            "SKIPPED: meshql-dynamo capacity-cost suite.\n  \
             Reason: MESHQL_DYNAMO_ENDPOINT is set to {endpoint:?}.\n  \
             This suite must run against real AWS. DynamoDB Local returns no \
             ConsumedCapacity,\n  so every prediction here would compare against \
             zero and pass or fail for the wrong reason.\n  \
             Unset MESHQL_DYNAMO_ENDPOINT to run it."
        );
        return;
    }

    // Real AWS from the ambient config.
    let client = meshql_dynamo::make_client(None).await;
    if let Err(e) = client.list_tables().limit(1).send().await {
        println!(
            "SKIPPED: meshql-dynamo capacity-cost suite.\n  \
             Reason: no usable AWS credentials or region — ListTables failed.\n  \
             Detail: {e}\n  \
             Configure credentials (AWS_PROFILE, instance role, or env vars) and \
             a region, then re-run."
        );
        return;
    }

    let region = client
        .config()
        .region()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "<none>".into());
    println!(
        "meshql-dynamo capacity-cost suite — REAL AWS DynamoDB, region {region}.\n\
         Predictions come from meshql_dynamo::metering; metered figures come from \
         DynamoDB's own\nConsumedCapacity. Tables are named dynamocost-* and are \
         dropped at the end of each check."
    );

    let mut checks = Checks::default();
    calibrate_the_read_side_of_the_size_model(&client, &mut checks).await;
    write_units_at_the_kilobyte_boundary(&client, &mut checks).await;
    a_temporal_read_is_half_an_rru_however_many_versions_exist(&client, &mut checks).await;
    a_search_costs_the_aggregate_bytes_examined(&client, &mut checks).await;
    a_scan_pages_at_one_mebibyte(&client, &mut checks).await;
    metering_does_not_change_results(&client, &mut checks).await;

    println!("\n---------------------------------------------------------------");
    if checks.failures.is_empty() {
        println!(
            "{} checks passed; the cost model matches the meter.",
            checks.passed
        );
    } else {
        println!(
            "{} passed, {} FAILED:",
            checks.passed,
            checks.failures.len()
        );
        for f in &checks.failures {
            println!("  - {f}");
        }
        std::process::exit(1);
    }

    // Leave nothing behind, and say what is left if anything is.
    // Check only the tables *this run* created. Filtering the whole
    // `dynamocost-` namespace looks equivalent and is not: another process
    // sharing the prefix — a benchmark, a colleague, a second agent — makes this
    // suite fail for someone else's resources, and would justify a cleanup that
    // destroyed live data. That collision actually happened during this suite's
    // development.
    let mine = CREATED_TABLES.lock().unwrap().clone();
    let present: Vec<String> = client
        .list_tables()
        .send()
        .await
        .map(|o| o.table_names().to_vec())
        .unwrap_or_default();
    let others: Vec<&String> = present
        .iter()
        .filter(|t| t.starts_with("dynamocost-") && !mine.contains(t))
        .collect();
    if !others.is_empty() {
        println!(
            "NOTE: {} other dynamocost-* table(s) exist and are NOT this run's: \
             {others:?} — left alone.",
            others.len()
        );
    }
    let leftovers: Vec<String> = present.into_iter().filter(|t| mine.contains(t)).collect();
    if leftovers.is_empty() {
        println!(
            "Teardown verified: all {} tables this run created are gone.",
            mine.len()
        );
    } else {
        println!("WARNING: dynamocost-* tables still present: {leftovers:?}");
        std::process::exit(1);
    }
}

// Silence the unused-import warning when the suite skips.
#[allow(dead_code)]
fn _unused(_: HashMap<String, String>) {}

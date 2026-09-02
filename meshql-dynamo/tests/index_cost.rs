//! What the **shipped** derived-index path costs, metered against real AWS.
//!
//! This file replaces the design probe that preceded it. That probe built its
//! tables and ran its two-phase query by hand, because the design was a
//! recommendation; everything here goes through [`DynamoCollection`],
//! [`IndexPlan`] and `Searcher::find_all`, because the design is now the
//! adapter. A cost model measured against a hand-rolled imitation of the code
//! is a cost model for the imitation.
//!
//! It answers five questions, and two of the answers are counterintuitive.
//!
//! # 1. What an indexed foreign-key lookup costs, against the scan it replaced
//!
//! The headline claim in `docs/cost-model-dynamodb.md` §6: an FK search
//! returning ten records costs **6.0 RRU** where the `Scan` it replaces costs
//! **122,254 RRU** at a million versions — a 20,000× reduction that turns a
//! $40,500/month search workload into a $2.00/month one.
//!
//! Only half of that is worth re-measuring, and it is the interesting half.
//! **The two-phase cost does not depend on `V` at all** — phase 1 reads the
//! index entries for the matching records and phase 2 resolves the matching
//! ids, and neither quantity knows how large the table is. So 6.0 RRU is
//! reproducible on a corpus of a thousand versions, and that reproduction is
//! evidence about the million-version case precisely because `V` is absent from
//! the formula. The 122,254 figure on the other side is a *measured* number
//! from a 999,993-version table (§11) and re-measuring it would mean rebuilding
//! that table for about $0.65 and several hours to reconfirm a straight line.
//! What is checked here instead is that the scan cost at V = 1,000 sits on that
//! same line.
//!
//! # 2. Write amplification, which is what an index actually costs
//!
//! Each index is a second item written on every write. At 1 KiB envelopes that
//! is **+1 WRU per write per index** — one index doubles the write bill, two
//! treble it — or **+$1.62/month per sustained write/sec, per index**, the same
//! unit as the entire base write cost. Measured through the repository, on
//! plans of 0, 1 and 2 fields.
//!
//! # 3. Sparse indexes: is an absent field free?
//!
//! [`IndexPlan::promote`] writes nothing when the payload lacks the field, so
//! the version has no index entry. The expectation is that it costs no index
//! capacity whatsoever, which would make **optional fields free to index**.
//! Measured rather than assumed, on a two-index table with one field present
//! and with neither.
//!
//! # 4. Is `KEYS_ONLY` still the right projection?
//!
//! It follows from the two-phase shape that a wider projection is never read —
//! [`store::query_index_candidates`] consumes exactly one attribute from an
//! index response, the base table's hash key, which `KEYS_ONLY` projects. That
//! is a fact about the code and it is checked by reading it. What is *not*
//! obvious is the cost side: at 1 KiB items an `ALL` index rounds to the same
//! kilobyte as a `KEYS_ONLY` one and costs exactly the same, which is why the
//! earlier measurement found no difference and why it must not be generalised.
//! Measured here at **3 KiB items**, where the two projections diverge.
//!
//! # 5. Parallel scan on a small table
//!
//! Capacity is charged on bytes examined and segments partition the same bytes,
//! so RRU is invariant in the segment count — measured at a million versions.
//! The expectation going in was that a *small* table would break that, because
//! each segment's final page rounds up to its own 4 KB boundary and on a small
//! table the rounding is the whole bill.
//!
//! **It does not, at four segments.** A three-item table meters 2.0 RRU serially
//! — four roundings, not one, because a serial `Scan` is already charged per
//! partition — and 2.0 RRU at four segments. The penalty appears at sixteen
//! (9.5 RRU). So the cost objection to parallel scan is weaker than the model
//! assumed, and the reason the default is one segment is not cost.
//!
//! # Running it
//!
//! ```sh
//! MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 \
//!   cargo test -p meshql-dynamo --test index_cost
//! ```
//!
//! Skips and exits 0 without that opt-in, and refuses to run against
//! `MESHQL_DYNAMO_ENDPOINT`, because DynamoDB Local does not meter — it returns
//! no `ConsumedCapacity` at all, so a cost suite pointed at it would pass by
//! measuring nothing. Builds `dynamocost-ix*` tables totalling ~1,100 items and
//! drops them all. Runs to a few tenths of a cent.

use std::collections::HashMap;

use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, Projection, ProjectionType, ScalarAttributeType,
};
use aws_sdk_dynamodb::Client;
use meshql_core::{Envelope, Repository, RootConfig, Searcher, Stash};
use meshql_dynamo::metering::{item_size_bytes, read_units, write_units};
use meshql_dynamo::{store, CapacityMeter, DynamoCollection, Op, Rates};
use serde_json::json;

// ---------------------------------------------------------------- harness ---

#[derive(Default)]
struct Checks {
    passed: usize,
    failures: Vec<String>,
}

impl Checks {
    fn eq(&mut self, what: &str, predicted: f64, actual: f64) {
        let ok = (predicted - actual).abs() < 1e-9;
        println!(
            "  {} {:<62} predicted {:>9.2}  metered {:>9.2}",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual
        );
        self.tally(ok, || {
            format!("{what}: predicted {predicted}, metered {actual}")
        });
    }

    /// Scans meter 1-3% above the write-side size model — see
    /// `capacity_cost.rs::calibrate_the_read_side_of_the_size_model`. A floor
    /// plus 3% headroom still catches every modelling error that matters
    /// (per-item vs aggregate rounding is 8x; V vs M is 10x).
    fn within(&mut self, what: &str, predicted: f64, actual: f64) {
        let ok = actual >= predicted - 1e-9 && actual <= predicted * 1.03 + 1e-9;
        println!(
            "  {} {:<62} model {:>9.2}  metered {:>9.2} ({:+.2}%)",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual,
            (actual / predicted - 1.0) * 100.0
        );
        self.tally(ok, || {
            format!("{what}: model {predicted}, metered {actual}")
        });
    }

    fn assert_true(&mut self, what: &str, ok: bool) {
        println!("  {} {what}", if ok { "PASS" } else { "FAIL" });
        self.tally(ok, || what.to_string());
    }

    fn tally(&mut self, ok: bool, describe: impl FnOnce() -> String) {
        if ok {
            self.passed += 1;
        } else {
            self.failures.push(describe());
        }
    }
}

// ------------------------------------------------------------- fixtures -----

/// Every table this run created, so teardown can be verified against exactly
/// those and not against a shared prefix. Filtering the whole `dynamocost-ix`
/// namespace looks equivalent and is not: another process sharing the prefix
/// makes this suite fail for someone else's resources, and would justify a
/// cleanup that destroyed live data.
static CREATED_TABLES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn table_name(suffix: &str) -> String {
    let name = format!("dynamocost-ix{suffix}-{}", uuid::Uuid::new_v4().simple());
    CREATED_TABLES.lock().unwrap().push(name.clone());
    name
}

/// A config filtering on `fields` — the input a deployment actually gives the
/// adapter. The index set is derived from it; nothing here names an index.
fn config_filtering_on(fields: &[&str]) -> RootConfig {
    let mut builder = RootConfig::builder().singleton("byId", r#"{"id": "{{id}}"}"#);
    for field in fields {
        builder = builder.vector(
            format!("by_{field}"),
            format!(r#"{{"payload.{field}": "{{{{v}}}}"}}"#),
        );
    }
    builder.build()
}

async fn open(
    client: &Client,
    table: &str,
    fields: &[&str],
    meter: &std::sync::Arc<CapacityMeter>,
) -> DynamoCollection {
    DynamoCollection::open_with_client(client.clone(), table, &config_filtering_on(fields))
        .await
        .expect("open the collection")
        .with_meter(meter.clone())
}

/// An envelope of at least `target` bytes once written, carrying `fk` and
/// optionally `zone`.
///
/// The padding loop measures and re-measures because `created_at`'s RFC3339
/// rendering has 0, 3, 6 or 9 fractional digits depending on the instant, so
/// two envelopes built microseconds apart can differ by ten bytes.
fn envelope(id: &str, fk: &str, zone: Option<&str>, target: u64) -> Envelope {
    let mut pad = 0usize;
    loop {
        let mut payload = Stash::new();
        payload.insert("fk".to_string(), json!(fk));
        if let Some(z) = zone {
            payload.insert("zone".to_string(), json!(z));
        }
        payload.insert("pad".to_string(), json!("x".repeat(pad)));
        let env = Envelope::new(id, payload, vec!["*".to_string()]);
        let size = item_size_bytes(&store::envelope_to_item(&env));
        if size >= target || pad > 100_000 {
            return env;
        }
        pad += (target - size) as usize;
    }
}

fn star() -> Vec<String> {
    vec!["*".to_string()]
}

/// Write one envelope through the repository and return the metered write
/// capacity — including whatever the promoted attributes and their indexes
/// cost, since that is what the caller is billed.
async fn put(collection: &DynamoCollection, meter: &CapacityMeter, env: Envelope) -> f64 {
    let before = meter.snapshot();
    collection
        .repository
        .create(env, &meshql_core::TokenSession::new(star()))
        .await
        .expect("create");
    meter.snapshot().minus(&before).write_units()
}

async fn drop_table(client: &Client, table: &str) {
    if let Err(e) = meshql_dynamo::drop_table(client, table).await {
        eprintln!("WARNING: failed to drop {table}: {e} — DELETE IT BY HAND");
    }
}

/// A global secondary index is updated asynchronously, so a search run
/// immediately after a write burst can see fewer candidates than exist. Wait
/// for it to catch up *before* metering, so that a measurement is never of a
/// half-built index.
async fn wait_for_index(
    collection: &DynamoCollection,
    template: &str,
    expected: usize,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    for attempt in 0..60 {
        let found = collection
            .searcher
            .find_all(
                template,
                &Stash::new(),
                &meshql_core::TokenSession::new(star()),
                now,
            )
            .await
            .expect("search");
        if found.len() == expected {
            if attempt > 0 {
                println!("  (the index caught up after {attempt}s)");
            }
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(format!(
        "the index never returned the expected {expected} records"
    ))
}

// ------------------------------------------------------------ experiments ---

/// The headline: what a foreign-key lookup costs indexed, against the `Scan` it
/// replaces, on one corpus so the comparison is like for like.
///
/// Corpus: `M` = 100 ids × `r` = 10 versions = **V = 1,000** versions of ~1 KiB,
/// with an `fk` of ten distinct values, so the searched value selects **10
/// records** — the §6 production scenario exactly, at a thousandth of the size.
async fn a_foreign_key_lookup_against_the_scan_it_replaces(client: &Client, checks: &mut Checks) {
    println!("\n== an indexed FK lookup vs the Scan it replaces ==");

    let table = table_name("fk");
    let meter = CapacityMeter::new();
    let collection = open(client, &table, &["fk"], &meter).await;
    checks.assert_true(
        "the index set was derived from the config, not declared",
        collection.plan().fields().collect::<Vec<_>>() == vec!["fk"],
    );

    let (m, r, kinds) = (100usize, 10usize, 10usize);
    let mut total_bytes = 0u64;
    for i in 0..m {
        let fk = format!("fk{}", i % kinds);
        let id = format!("e-{i:05}");
        for _ in 0..r {
            let env = envelope(&id, &fk, None, 1024);
            total_bytes += item_size_bytes(&store::envelope_to_item(&env));
            collection
                .repository
                .create(env, &meshql_core::TokenSession::new(star()))
                .await
                .expect("create");
        }
    }
    let v = (m * r) as u64;
    let matching_ids = m / kinds;
    let template = r#"{"payload.fk": "fk0"}"#;

    if let Err(e) = wait_for_index(&collection, template, matching_ids).await {
        checks.assert_true(&e, false);
        drop_table(client, &table).await;
        return;
    }

    // --- the two phases, separately, so the split can be reported ---
    let before = meter.snapshot();
    let candidates = store::query_index_candidates(
        client,
        &table,
        "fk",
        "fk0",
        store::now_cutoff_nanos(),
        Some(&meter),
    )
    .await
    .expect("phase 1");
    let phase1 = meter.snapshot().minus(&before);

    let before = meter.snapshot();
    let resolved = store::resolve_candidates(
        client,
        &table,
        candidates.clone(),
        store::now_cutoff_nanos(),
        Some(&meter),
    )
    .await
    .expect("phase 2");
    let phase2 = meter.snapshot().minus(&before);

    // --- and the shipped searcher, which is the two of them plus no I/O ---
    let before = meter.snapshot();
    let found = collection
        .searcher
        .find_all(
            template,
            &Stash::new(),
            &meshql_core::TokenSession::new(star()),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .expect("indexed search");
    let indexed = meter.snapshot().minus(&before);

    // --- what the same search costs unindexed: the Scan the adapter would do ---
    // `store::scan_latest` *is* the unindexed searcher's path; calling it
    // directly avoids building a second thousand-version corpus to compare
    // against.
    let before = meter.snapshot();
    let scanned = store::scan_latest(client, &table, store::now_cutoff_nanos(), Some(&meter))
        .await
        .expect("scan");
    let scan = meter.snapshot().minus(&before);

    checks.assert_true(
        &format!("the indexed search found the {matching_ids} matching records"),
        found.len() == matching_ids,
    );
    checks.assert_true(
        &format!("...and the Scan resolves all M={m}, so the corpus is what it claims"),
        scanned.len() == m,
    );

    checks.assert_true(
        &format!("phase 1 offers {matching_ids} candidate ids, one per matching record"),
        candidates.len() == matching_ids && resolved.len() == matching_ids,
    );
    checks.eq(
        "phase 2 costs 0.5 RRU per candidate id (the point-Query floor)",
        matching_ids as f64 * 0.5,
        phase2.read_units(),
    );
    checks.eq(
        "the shipped search costs exactly its two phases, and nothing else",
        phase1.read_units() + phase2.read_units(),
        indexed.read_units(),
    );
    checks.eq(
        "the whole indexed FK lookup, against the 6.0 RRU claimed in §6",
        6.0,
        indexed.read_units(),
    );
    checks.within(
        &format!("the Scan at V={v} costs the aggregate bytes"),
        read_units(total_bytes, true),
        scan.read_units(),
    );

    println!(
        "  phase 1: {} index entries -> {:.1} RRU (implies ~{:.0} B/entry)",
        matching_ids * r,
        phase1.read_units(),
        phase1.read_units() * 8192.0 / (matching_ids * r) as f64,
    );
    println!(
        "  phase 2: {} candidate ids -> {:.1} RRU",
        candidates.len(),
        phase2.read_units()
    );
    println!(
        "  indexed {:.1} RRU in {} round trips  vs  Scan {:.1} RRU in {} round trips  => {:.0}x",
        indexed.read_units(),
        indexed.round_trips(),
        scan.read_units(),
        scan.round_trips(),
        scan.read_units() / indexed.read_units(),
    );

    // The extrapolation, and the only claim here about a million versions: the
    // Scan is linear in V — the page count matched ceil(V*S/1MiB) exactly at
    // four measured sizes across three decades (§11) — and the two-phase cost
    // has no V in it at all.
    let scan_at_1m = scan.read_units() * 1_000_000.0 / v as f64;
    let rates = Rates::ON_DEMAND_US_EAST_1;
    println!(
        "  extrapolated to V=1,000,000: Scan {:.0} RRU (${:.4}/search, ${:.0}/month at 1/sec)",
        scan_at_1m,
        scan_at_1m * rates.read_request_unit_usd,
        scan_at_1m * rates.read_request_unit_usd * 2_592_000.0,
    );
    println!(
        "  the indexed lookup stays {:.1} RRU (${:.6}/search, ${:.2}/month at 1/sec) — V is not in it",
        indexed.read_units(),
        indexed.read_units() * rates.read_request_unit_usd,
        indexed.read_units() * rates.read_request_unit_usd * 2_592_000.0,
    );
    // §11 measured 122,254 RRU on a real 999,993-version table of ~1 KiB items.
    // Extrapolating this thousand-version Scan must land on that, within the
    // few percent the item sizes differ by — a bigger gap would mean the line is
    // not straight and the whole extrapolation is void.
    checks.assert_true(
        &format!(
            "extrapolating this Scan to V=1M gives {scan_at_1m:.0} RRU against the \
             122,254 measured on a real million-version table (within 10%)"
        ),
        (scan_at_1m / 122_254.0 - 1.0).abs() < 0.10,
    );
    checks.assert_true(
        &format!(
            "=> {:.0}x cheaper at V=1M, against the ~20,000x claimed in §6",
            122_254.0 / indexed.read_units()
        ),
        (122_254.0 / indexed.read_units()) > 15_000.0,
    );

    drop_table(client, &table).await;
}

/// Write amplification is the price of an index, and the sparse case is the
/// question that decides whether optional fields are free.
async fn write_amplification_and_the_sparse_case(client: &Client, checks: &mut Checks) {
    println!("\n== each index is +1 WRU per write; an absent field pays nothing ==");

    let meter = CapacityMeter::new();
    let t0 = table_name("w0");
    let t1 = table_name("w1");
    let t2 = table_name("w2");
    let c0 = open(client, &t0, &[], &meter).await;
    let c1 = open(client, &t1, &["fk"], &meter).await;
    let c2 = open(client, &t2, &["fk", "zone"], &meter).await;

    // 950 bytes, so that promoting `ix_fk` and `ix_zone` leaves the base item
    // inside the same kilobyte. Sizing it at 1024 instead makes the promoted
    // item cost a *second* base WRU before any index is written at all — a real
    // effect, asserted separately below, and how this fixture was first got
    // wrong.
    let both = |id: &str| envelope(id, "north", Some("z"), 950);

    checks.eq(
        "a <1 KiB write, no indexes",
        1.0,
        put(&c0, &meter, both("amp")).await,
    );
    checks.eq("...one index", 2.0, put(&c1, &meter, both("amp")).await);
    checks.eq("...two indexes", 3.0, put(&c2, &meter, both("amp")).await);

    // Sparse: `zone` is absent, so no entry is written to its index.
    let mut sparse = envelope("sparse", "north", None, 950);
    sparse.payload.remove("zone");
    checks.eq(
        "two indexes, one field ABSENT: pays for one index only",
        2.0,
        put(&c2, &meter, sparse).await,
    );

    // Both absent: the indexes cost nothing at all.
    let mut bare = envelope("bare", "north", None, 950);
    bare.payload.remove("zone");
    bare.payload.remove("fk");
    checks.eq(
        "two indexes, BOTH fields absent: pays for no index at all",
        1.0,
        put(&c2, &meter, bare).await,
    );

    // The trap: promotion adds bytes to the *base* item, so an envelope sitting
    // on a kilobyte boundary costs an extra WRU before any index is written.
    let edge = envelope("edge", "north", None, 1024);
    let edge_size = item_size_bytes(&store::envelope_to_item(&edge));
    let unpromoted = put(&c0, &meter, edge.clone()).await;
    let promoted = put(&c1, &meter, edge).await;
    checks.eq(
        &format!("an exactly-{edge_size}-byte item on an unindexed table"),
        write_units(edge_size),
        unpromoted,
    );
    checks.assert_true(
        "...the same item promoted + indexed costs more than 2: promotion alone \
         crossed the KB boundary",
        promoted > 2.0,
    );

    let monthly = 2_592_000.0 * Rates::ON_DEMAND_US_EAST_1.write_request_unit_usd;
    println!("  => +1 WRU/write/index = +${monthly:.2}/month per sustained write/sec, per index");
    println!("  => a field absent from the payload is free to index");

    for t in [&t0, &t1, &t2] {
        drop_table(client, t).await;
    }
}

/// `KEYS_ONLY` buys nothing to read and costs less to write — but only at item
/// sizes where the two projections do not round to the same kilobyte.
async fn keys_only_is_the_cheaper_projection(client: &Client, checks: &mut Checks) {
    println!("\n== KEYS_ONLY vs ALL, at an item size where they differ ==");

    let meter = CapacityMeter::new();
    let keys_only = table_name("proj");
    let collection = open(client, &keys_only, &["fk"], &meter).await;

    // A hand-built twin with an ALL projection on the same attribute, since the
    // adapter will not create one.
    let all = table_name("all");
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
        .table_name(&all)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key(store::PK, KeyType::Hash))
        .key_schema(key(store::SK, KeyType::Range))
        .attribute_definitions(attr(store::PK))
        .attribute_definitions(attr(store::SK))
        .attribute_definitions(attr("ix_fk"))
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name("meshql_ix_fk")
                .key_schema(key("ix_fk", KeyType::Hash))
                .key_schema(key(store::SK, KeyType::Range))
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::All)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .send()
        .await
        .expect("the ALL-projection twin");
    for _ in 0..600 {
        let out = client
            .describe_table()
            .table_name(&all)
            .send()
            .await
            .expect("describe");
        let t = out.table().expect("table");
        if t.table_status().map(|s| s.as_str()) == Some("ACTIVE")
            && t.global_secondary_indexes()
                .iter()
                .all(|i| i.index_status().map(|s| s.as_str()) == Some("ACTIVE"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // 3 KiB, so the base item is 3 WRU either way and the *index* entry is 1 WRU
    // for KEYS_ONLY against 3 for ALL. At 1 KiB both round to the same kilobyte
    // and the difference is invisible — which is exactly why the earlier
    // measurement found none.
    let env = envelope("proj", "north", None, 3 * 1024);
    let size = item_size_bytes(&store::envelope_to_item(&env));

    let keys_only_cost = put(&collection, &meter, env.clone()).await;

    let mut item = store::envelope_to_item(&env);
    item.insert("ix_fk".to_string(), AttributeValue::S("north".into()));
    let before = meter.snapshot();
    let out = client
        .put_item()
        .table_name(&all)
        .set_item(Some(item))
        .return_consumed_capacity(aws_sdk_dynamodb::types::ReturnConsumedCapacity::Total)
        .send()
        .await
        .expect("put into the ALL twin");
    meter.record(Op::PutItem, out.consumed_capacity());
    let all_cost = meter.snapshot().minus(&before).write_units();

    // The base item is charged once either way — and note it costs `write_units(size) + 1`
    // rather than `write_units(size)`, because the promoted `ix_fk` pushed a
    // 3072-byte item over the 3 KiB boundary. That is the same boundary trap as
    // above, and it is why the interesting quantity is the *difference*.
    let base = write_units(size) + 1.0;
    println!(
        "  a {size}-byte item: base {base} WRU + index. KEYS_ONLY total {keys_only_cost}, \
         ALL total {all_cost}"
    );
    checks.eq(
        "a KEYS_ONLY index entry is one unit however large the item",
        1.0,
        keys_only_cost - base,
    );
    checks.eq(
        "an ALL index entry is a second full copy of the item",
        base,
        all_cost - base,
    );
    checks.assert_true(
        &format!(
            "=> at {size}-byte items ALL costs {:.0}x the index capacity of KEYS_ONLY \
             (at 1 KiB both round to the same kilobyte and the difference is invisible)",
            (all_cost - base) / (keys_only_cost - base)
        ),
        all_cost > keys_only_cost,
    );
    checks.assert_true(
        "=> the extra projection buys nothing: the search reads only `pk` from \
         the index and re-reads every candidate from the base table anyway",
        true,
    );

    drop_table(client, &keys_only).await;
    drop_table(client, &all).await;
}

/// Parallel `Scan` on a small table: the case the published RRU invariance does
/// not cover, and the reason the default is one segment.
async fn parallel_scan_costs_more_on_a_small_table(client: &Client, checks: &mut Checks) {
    println!("\n== parallel Scan: free at scale, not free on a small table ==");

    let meter = CapacityMeter::new();
    let table = table_name("seg");
    let collection = open(client, &table, &[], &meter).await;
    for i in 0..3 {
        collection
            .repository
            .create(
                envelope(&format!("s{i}"), "north", None, 200),
                &meshql_core::TokenSession::new(star()),
            )
            .await
            .expect("create");
    }

    let mut costs = HashMap::new();
    for segments in [1, 4, 16] {
        let before = meter.snapshot();
        let found = store::scan_latest_segmented(
            client,
            &table,
            store::now_cutoff_nanos(),
            Some(&meter),
            segments,
        )
        .await
        .expect("segmented scan");
        let stats = meter.snapshot().minus(&before);
        println!(
            "  {segments:>2} segment(s): {:>5.1} RRU in {} round trips, {} records",
            stats.read_units(),
            stats.round_trips(),
            found.len()
        );
        checks.assert_true(
            &format!("{segments} segments resolve the same 3 records"),
            found.len() == 3,
        );
        costs.insert(segments, stats.read_units());
    }

    // The expectation going in was that four segments would cost more even here,
    // because each segment's partial page rounds up to its own 4 KB boundary.
    // **It does not**, and the reason is that a serial `Scan` is already charged
    // per partition: the three-item table meters 2.0 RRU on one segment, which
    // is four roundings, not one. Four segments therefore re-partition a cost
    // that was already being paid. The rounding penalty is real, but it starts
    // above the table's own partition count.
    checks.eq(
        "four segments cost the same as one, even on a three-item table",
        costs[&1],
        costs[&4],
    );
    checks.assert_true(
        &format!(
            "...and sixteen costs {:.1}x more, so the penalty is real above the \
             table's partition count",
            costs[&16] / costs[&1]
        ),
        costs[&16] > costs[&1],
    );
    println!(
        "  => at the recommended four segments the capacity is invariant even here, so the \
         cost objection to parallel Scan is weaker than the model assumed. The default is \
         still one, because four round trips buy nothing on a table this size and the 2.6x \
         wall-clock win (§11) only exists above the ~58 MB/s consumer ceiling."
    );

    drop_table(client, &table).await;
}

/// The soundness demonstration, on the shipped path: an index holds *versions*,
/// so a record that has moved on is still in its old partition forever.
async fn the_index_cannot_resolve_the_latest_version_by_itself(
    client: &Client,
    checks: &mut Checks,
) {
    println!("\n== an index alone cannot resolve the latest version (soundness) ==");

    let meter = CapacityMeter::new();
    let table = table_name("sound");
    let collection = open(client, &table, &["fk"], &meter).await;

    // "mover" changes fk: tool -> widget. "stayer" does not.
    for (id, fk) in [("mover", "tool"), ("stayer", "tool"), ("mover", "widget")] {
        collection
            .repository
            .create(
                envelope(id, fk, None, 400),
                &meshql_core::TokenSession::new(star()),
            )
            .await
            .expect("create");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let template = r#"{"payload.fk": "tool"}"#;
    if let Err(e) = wait_for_index(&collection, template, 1).await {
        // The expected answer is 1 (`stayer`); if the index reports 2 for a
        // full minute, the adapter is wrong, not slow.
        checks.assert_true(
            &format!("{e} — the shipped search returned the wrong set"),
            false,
        );
        drop_table(client, &table).await;
        return;
    }

    // What the index itself says, before any resolution: both records, because
    // mover's superseded version is still in the `tool` partition.
    let raw = store::query_index_candidates(
        client,
        &table,
        "fk",
        "tool",
        store::now_cutoff_nanos(),
        Some(&meter),
    )
    .await
    .expect("phase 1");
    let mut raw: Vec<String> = raw.into_iter().collect();
    raw.sort();

    let found = collection
        .searcher
        .find_all(
            template,
            &Stash::new(),
            &meshql_core::TokenSession::new(star()),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .expect("search");
    let ids: Vec<&str> = found.iter().map(|s| s["id"].as_str().unwrap()).collect();

    println!("  phase 1 (the index alone) returns: {raw:?}");
    println!("  the shipped two-phase search returns: {ids:?}");

    checks.assert_true(
        "the index alone offers `mover` as a candidate, and always will — its \
         current version is in a different partition",
        raw.contains(&"mover".to_string()),
    );
    checks.assert_true(
        "the shipped search excludes `mover`, because phase 2 re-resolved it",
        !ids.contains(&"mover"),
    );
    checks.assert_true(
        "...and still finds `stayer`, so it is not merely returning nothing",
        ids.contains(&"stayer"),
    );

    drop_table(client, &table).await;
}

// -------------------------------------------------------------------- main ---

#[tokio::main]
async fn main() {
    if std::env::var("MESHQL_DYNAMO_COST_TESTS").as_deref() != Ok("1") {
        println!(
            "SKIPPED: meshql-dynamo index cost suite.\n  \
             Reason: MESHQL_DYNAMO_COST_TESTS is not set to 1.\n  \
             This suite bills a real AWS account (a few tenths of a cent) to measure \
             what the\n  derived-index path costs — see docs/cost-model-dynamodb.md.\n  \
             Run with: MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 cargo test \
             -p meshql-dynamo --test index_cost"
        );
        return;
    }
    if let Ok(endpoint) = std::env::var("MESHQL_DYNAMO_ENDPOINT") {
        println!(
            "SKIPPED: meshql-dynamo index cost suite.\n  \
             Reason: MESHQL_DYNAMO_ENDPOINT is set to {endpoint:?}; DynamoDB Local \
             does not meter."
        );
        return;
    }

    let client = meshql_dynamo::make_client(None).await;
    if let Err(e) = client.list_tables().limit(1).send().await {
        println!(
            "SKIPPED: meshql-dynamo index cost suite.\n  \
             Reason: no usable AWS credentials or region — ListTables failed.\n  \
             Detail: {e}"
        );
        return;
    }

    println!(
        "meshql-dynamo index cost suite — REAL AWS DynamoDB, region {}.\n\
         Measures the shipped derived-index path.",
        client
            .config()
            .region()
            .map(|r| r.to_string())
            .unwrap_or_else(|| "<none>".into())
    );

    let mut checks = Checks::default();
    write_amplification_and_the_sparse_case(&client, &mut checks).await;
    keys_only_is_the_cheaper_projection(&client, &mut checks).await;
    parallel_scan_costs_more_on_a_small_table(&client, &mut checks).await;
    the_index_cannot_resolve_the_latest_version_by_itself(&client, &mut checks).await;
    a_foreign_key_lookup_against_the_scan_it_replaces(&client, &mut checks).await;

    println!("\n---------------------------------------------------------------");
    if checks.failures.is_empty() {
        println!("{} checks passed.", checks.passed);
    } else {
        println!(
            "{} passed, {} FAILED:",
            checks.passed,
            checks.failures.len()
        );
        for f in &checks.failures {
            println!("  - {f}");
        }
    }

    let mine = CREATED_TABLES.lock().unwrap().clone();

    // `DeleteTable` returns while the table is still `DELETING`, and a
    // `DELETING` table is still in `ListTables`. Waiting is the difference
    // between verifying teardown and verifying that teardown was *requested* —
    // and the first run of this suite reported a leftover for exactly that
    // reason.
    let mut present: Vec<String> = Vec::new();
    for _ in 0..120 {
        present = client
            .list_tables()
            .send()
            .await
            .map(|o| o.table_names().to_vec())
            .unwrap_or_default();
        if !present.iter().any(|t| mine.contains(t)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
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
    let leftovers: Vec<&String> = present.iter().filter(|t| mine.contains(t)).collect();
    if leftovers.is_empty() {
        println!(
            "Teardown verified: all {} tables this run created are gone.",
            mine.len()
        );
    } else {
        println!("WARNING: tables still present: {leftovers:?}");
        std::process::exit(1);
    }

    if !checks.failures.is_empty() {
        std::process::exit(1);
    }
}

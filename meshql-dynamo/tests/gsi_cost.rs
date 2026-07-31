//! A **design probe**, not a certification: what an indexed-payload-field GSI
//! would actually cost, metered against real AWS.
//!
//! Nothing here is shipped. `meshql-dynamo` has no GSI support; this file exists
//! because `docs/cost-model-dynamodb.md` recommends building it, and a
//! recommendation with no measurement behind it is an opinion. It builds the
//! index by hand, runs the two-phase query shape the design calls for, and puts
//! the metered figures next to the `Scan` they would replace.
//!
//! It answers four questions, two of which have counterintuitive answers.
//!
//! # 1. Write amplification
//!
//! Every GSI is a second item written on every write, rounded up to its own
//! kilobyte. At 1 KiB envelopes a `KEYS_ONLY` index therefore **doubles** the
//! write bill, and it costs the same as an `ALL` index, because both round to
//! the same 1 KB. Measured on tables with 0, 1 and 2 indexes.
//!
//! # 2. Sparse indexes are free for absent fields
//!
//! A version whose payload lacks the indexed field writes no index entry and
//! costs no index capacity, so optional fields are free to index. Measured.
//!
//! # 3. The per-candidate floor, and when an index makes things *worse*
//!
//! Phase 2 resolves each candidate id with a point `Query`, and a point `Query`
//! pays a **0.5 RRU floor** — a 4 KB minimum — while a `Scan` reads in bulk at
//! 0.5 RRU per 4 KB. At 1 KiB items a point read is therefore **4× dearer per
//! byte** than the same bytes inside a scan. So for `V` versions over `M` ids
//! with a predicate selecting a fraction `f`:
//!
//! ```text
//! scan       = V·S / 8192
//! two-phase  = f·V·G / 8192  +  f·M / 2
//! ```
//!
//! With `S` = 1 KiB and `G` ≈ 78 B (`KEYS_ONLY`, **measured** below), the index wins iff
//! `f · (0.0095 + r⁻¹/2) < 0.125`, where `r = V/M` is versions per id:
//!
//! | versions/id `r` | index wins while |
//! |---|---|
//! | 1  | `f < 0.245` |
//! | 2  | `f < 0.479` |
//! | 4  | `f < 0.923` |
//! | 5  | always |
//! | 10 | always |
//!
//! meshql **event** entities are create-only — a correction is a new event, not
//! an edit — so they sit at `r = 1`, where a low-cardinality predicate
//! (`byStatus` over three values, `f ≈ ⅓`) makes the GSI **dearer than the scan
//! it replaced**. meshql **projections** are folded repeatedly, so they sit at
//! large `r`, where the index always wins. Both cases are measured below.
//!
//! # 4. A projected GSI does **not** remove phase 2 — it breaks correctness
//!
//! The tempting optimisation is a `ALL`-projection index, grouping latest-per-id
//! *inside* the index results and skipping phase 2 entirely, for a cost of
//! `f × scan` with no per-id floor. It is unsound, and
//! [`a_projected_index_cannot_resolve_the_latest_version`] demonstrates the
//! counterexample against real data rather than arguing it:
//!
//! ```text
//! id "mover":  v1 (kind = tool)   v2 (kind = widget)
//! GSI query kind = tool           → returns v1 only
//! group within the index          → "mover" resolves to v1
//! re-check predicate on v1        → kind = tool, matches → RETURNED
//! ```
//!
//! but `"mover"`'s *actual* resolved version is v2, whose kind is `widget`, so
//! it must not be returned. The index cannot see v2 — v2 lives in a different
//! index partition — so no amount of re-checking inside the result set can find
//! the error. **The two-phase shape is required, not an optimisation**, and
//! `KEYS_ONLY` is consequently the right projection: `ALL` costs more storage
//! and buys nothing.
//!
//! # Running it
//!
//! ```sh
//! MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 \
//!   cargo test -p meshql-dynamo --test gsi_cost
//! ```
//!
//! Skips and exits 0 without that opt-in. Builds six `dynamocost-gsi*` tables
//! totalling ~2,100 items and drops them all. Under a cent.

use std::collections::{HashMap, HashSet};

use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, Projection, ProjectionType, ScalarAttributeType, TableStatus,
};
use aws_sdk_dynamodb::Client;
use chrono::Utc;
use meshql_core::{Envelope, Stash};
use meshql_dynamo::metering::{item_size_bytes, read_units};
use meshql_dynamo::store;
use meshql_dynamo::{CapacityMeter, Op};
use serde_json::json;

/// The promoted attribute an indexed payload field would become. The design
/// calls it `ix_{field}`; this probe indexes one field, `kind`.
const IX_KIND: &str = "ix_kind";
const IX_ZONE: &str = "ix_zone";
const GSI_KIND: &str = "gsi_kind";
const GSI_ZONE: &str = "gsi_zone";

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
            "  {} {:<58} predicted {:>9.2}  metered {:>9.2}",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual
        );
        if ok {
            self.passed += 1;
        } else {
            self.failures
                .push(format!("{what}: predicted {predicted}, metered {actual}"));
        }
    }

    /// Scans meter 1-3% above the write-side size model — see
    /// `capacity_cost.rs::calibrate_the_read_side_of_the_size_model`. A floor
    /// plus 3% headroom still catches every modelling error that matters
    /// (per-item vs aggregate rounding is 8x; V vs M is 10x).
    fn within(&mut self, what: &str, predicted: f64, actual: f64) {
        let ok = actual >= predicted - 1e-9 && actual <= predicted * 1.03 + 1e-9;
        println!(
            "  {} {:<58} model {:>9.2}  metered {:>9.2} ({:+.2}%)",
            if ok { "PASS" } else { "FAIL" },
            what,
            predicted,
            actual,
            (actual / predicted - 1.0) * 100.0
        );
        if ok {
            self.passed += 1;
        } else {
            self.failures
                .push(format!("{what}: model {predicted}, metered {actual}"));
        }
    }

    fn assert_true(&mut self, what: &str, ok: bool) {
        println!("  {} {what}", if ok { "PASS" } else { "FAIL" });
        if ok {
            self.passed += 1;
        } else {
            self.failures.push(what.to_string());
        }
    }
}

// ------------------------------------------------------------- fixtures -----

/// Every table this run created, so teardown can be verified against exactly
/// those and not against a shared prefix.
static CREATED_TABLES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn table_name(suffix: &str) -> String {
    let name = format!("dynamocost-gsi{suffix}-{}", uuid::Uuid::new_v4().simple());
    CREATED_TABLES.lock().unwrap().push(name.clone());
    name
}

fn attr(name: &str) -> AttributeDefinition {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(ScalarAttributeType::S)
        .build()
        .unwrap()
}

fn key(name: &str, kind: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(kind)
        .build()
        .unwrap()
}

fn index(name: &str, hash: &str, projection: ProjectionType) -> GlobalSecondaryIndex {
    GlobalSecondaryIndex::builder()
        .index_name(name)
        .key_schema(key(hash, KeyType::Hash))
        // The table's own sort key as the index range key: it carries the
        // temporal cutoff into the index, so `sk < :hi` is still a key
        // condition and not a filter.
        .key_schema(key(store::SK, KeyType::Range))
        .projection(Projection::builder().projection_type(projection).build())
        .build()
        .unwrap()
}

/// Create a table with `indexes` global secondary indexes and wait for the table
/// *and every index* to report ACTIVE.
async fn create_table(client: &Client, table: &str, indexes: &[(&str, &str, ProjectionType)]) {
    let mut req = client
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key(store::PK, KeyType::Hash))
        .key_schema(key(store::SK, KeyType::Range))
        .attribute_definitions(attr(store::PK))
        .attribute_definitions(attr(store::SK));

    for (name, hash, projection) in indexes {
        req = req
            .attribute_definitions(attr(hash))
            .global_secondary_indexes(index(name, hash, projection.clone()));
    }

    req.send().await.expect("create table");

    for _ in 0..600 {
        let out = client
            .describe_table()
            .table_name(table)
            .send()
            .await
            .expect("describe");
        let t = out.table().expect("table");
        let table_active = t.table_status() == Some(&TableStatus::Active);
        let indexes_active = t
            .global_secondary_indexes()
            .iter()
            .all(|i| i.index_status().map(|s| s.as_str()) == Some("ACTIVE"));
        if table_active && indexes_active {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("{table} did not become ACTIVE");
}

/// An envelope of ~`target` bytes, carrying `kind` in its payload.
fn envelope(id: &str, kind: &str, target: u64) -> Envelope {
    envelope_at(id, kind, target, Utc::now())
}

/// As [`envelope`], but at a caller-chosen instant.
///
/// `created_at` is not incidental to the size: `DateTime::to_rfc3339` emits 0,
/// 3, 6 or 9 fractional-second digits depending on the value, so two envelopes
/// built microseconds apart can differ by 10 bytes. The padding loop measures
/// and re-measures, so the returned envelope's size is always known exactly —
/// but it lands *at or just above* `target`, not on it. Any check that needs an
/// item exactly on a kilobyte boundary must pin the instant, which is what
/// [`FIXED_INSTANT`] is for.
fn envelope_at(id: &str, kind: &str, target: u64, when: chrono::DateTime<Utc>) -> Envelope {
    let mut pad = 0usize;
    loop {
        let mut payload = Stash::new();
        payload.insert("kind".to_string(), json!(kind));
        payload.insert("pad".to_string(), json!("x".repeat(pad)));
        let env = Envelope {
            id: id.to_string(),
            payload,
            created_at: when,
            deleted: false,
            authorized_tokens: vec!["*".to_string()],
        };
        let size = item_size_bytes(&store::envelope_to_item(&env));
        if size >= target || pad > 100_000 {
            return env;
        }
        pad += (target - size) as usize;
    }
}

/// An instant whose RFC3339 rendering has no fractional part, so envelope size
/// is a deterministic function of the padding.
fn fixed_instant() -> chrono::DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap()
}

/// Write one envelope, promoting `kind` to the indexed attribute when
/// `promote` is set. Returns the metered write capacity.
async fn put(
    client: &Client,
    table: &str,
    meter: &CapacityMeter,
    env: &Envelope,
    promote: bool,
    zone: Option<&str>,
) -> f64 {
    let mut item = store::envelope_to_item(env);
    if promote {
        if let Some(kind) = env.payload.get("kind").and_then(|v| v.as_str()) {
            item.insert(IX_KIND.to_string(), AttributeValue::S(kind.to_string()));
        }
    }
    if let Some(z) = zone {
        item.insert(IX_ZONE.to_string(), AttributeValue::S(z.to_string()));
    }

    let before = meter.snapshot();
    let out = client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .return_consumed_capacity(aws_sdk_dynamodb::types::ReturnConsumedCapacity::Total)
        .send()
        .await
        .expect("put");
    meter.record(Op::PutItem, out.consumed_capacity());
    meter.snapshot().minus(&before).write_units()
}

async fn drop_table(client: &Client, table: &str) {
    if let Err(e) = meshql_dynamo::drop_table(client, table).await {
        eprintln!("WARNING: failed to drop {table}: {e} — DELETE IT BY HAND");
    }
}

// ------------------------------------------------------------ experiments ---

/// Every index is a second write, and at 1 KiB items it doubles the write bill.
async fn write_amplification_is_one_unit_per_index(client: &Client, checks: &mut Checks) {
    println!("\n== each GSI adds ~1 WRU per write; a sparse miss adds none ==");

    let t0 = table_name("0");
    let t1 = table_name("1");
    let t2 = table_name("2");
    let tall = table_name("all");
    create_table(client, &t0, &[]).await;
    create_table(
        client,
        &t1,
        &[(GSI_KIND, IX_KIND, ProjectionType::KeysOnly)],
    )
    .await;
    create_table(
        client,
        &t2,
        &[
            (GSI_KIND, IX_KIND, ProjectionType::KeysOnly),
            (GSI_ZONE, IX_ZONE, ProjectionType::KeysOnly),
        ],
    )
    .await;
    create_table(client, &tall, &[(GSI_KIND, IX_KIND, ProjectionType::All)]).await;

    let meter = CapacityMeter::new();

    // 950 bytes, so that promoting `ix_kind` (+12 B) and `ix_zone` (+13 B)
    // leaves the base item inside the same kilobyte. Sizing it at 1024 instead
    // makes the promoted item 1036 B and costs a *second* base WRU before any
    // index is written at all — which is a real effect, asserted separately
    // below, and was how this fixture was first got wrong.
    let env = envelope("amp", "tool", 950);

    let base = put(client, &t0, &meter, &env, false, None).await;
    checks.eq("a <1 KiB write with no index", 1.0, base);

    let one = put(client, &t1, &meter, &env, true, None).await;
    checks.eq("...with one KEYS_ONLY index", 2.0, one);

    let two = put(client, &t2, &meter, &env, true, Some("north")).await;
    checks.eq("...with two KEYS_ONLY indexes", 3.0, two);

    let all = put(client, &tall, &meter, &env, true, None).await;
    checks.eq(
        "...with one ALL-projection index (same: both round to 1 KB)",
        2.0,
        all,
    );

    // Sparse: the indexed attribute is absent, so no index entry is written.
    let sparse = put(
        client,
        &t1,
        &meter,
        &envelope("sparse", "tool", 950),
        false,
        None,
    )
    .await;
    checks.eq(
        "a write whose indexed field is ABSENT pays no index capacity",
        1.0,
        sparse,
    );

    // ...and on the two-index table, promoting only one field pays for only one.
    let half = put(client, &t2, &meter, &env, true, None).await;
    checks.eq(
        "two indexes, one field present: pays for one index only",
        2.0,
        half,
    );

    // The trap: an envelope sitting exactly on a kilobyte boundary costs an
    // extra WRU the moment a field is promoted, *independently* of the index
    // write, because `ix_kind` is 12 more bytes in the base item.
    let edge = envelope_at("edge", "tool", 1024, fixed_instant());
    let edge_size = item_size_bytes(&store::envelope_to_item(&edge));
    let unpromoted = put(client, &t0, &meter, &edge, false, None).await;
    let promoted = put(client, &t1, &meter, &edge, true, None).await;
    checks.eq(
        &format!("an exactly-{edge_size}-byte item, no index, unpromoted"),
        1.0,
        unpromoted,
    );
    checks.eq(
        "...the same item promoted + indexed costs 3, not 2: promotion \
         alone crossed the KB boundary",
        3.0,
        promoted,
    );

    println!(
        "  => at 1 KiB items each index is +1 WRU/write = +${:.2}/month per sustained write/sec",
        2_592_000.0 * 0.625e-6
    );

    for t in [&t0, &t1, &t2, &tall] {
        drop_table(client, t).await;
    }
}

/// The two-phase query, priced against the scan it replaces, at both ends of the
/// versions-per-id axis.
async fn the_two_phase_query_priced_against_the_scan(client: &Client, checks: &mut Checks) {
    // (label, distinct ids M, versions per id r, distinct kinds)
    let corpora = [
        (
            "projection-like (r=10, 10 kinds, f=0.1)",
            100usize,
            10usize,
            10usize,
        ),
        (
            "event-like     (r=1,   3 kinds,  f=0.33)",
            1000usize,
            1usize,
            3usize,
        ),
    ];

    for (label, m, r, kinds) in corpora {
        let table = table_name("q");
        create_table(
            client,
            &table,
            &[(GSI_KIND, IX_KIND, ProjectionType::KeysOnly)],
        )
        .await;
        let meter = CapacityMeter::new();

        println!("\n== two-phase GSI vs Scan — {label} ==");

        // `kind` is stable per id, which is what a real FK or status field is.
        let mut total_bytes = 0u64;
        let mut ids_of_kind: HashMap<String, Vec<String>> = HashMap::new();
        for i in 0..m {
            let kind = format!("k{}", i % kinds);
            let id = format!("e-{i:05}");
            ids_of_kind
                .entry(kind.clone())
                .or_default()
                .push(id.clone());
            for _ in 0..r {
                let env = envelope(&id, &kind, 1024);
                total_bytes += item_size_bytes(&store::envelope_to_item(&env));
                put(client, &table, &meter, &env, true, None).await;
            }
        }
        let v = (m * r) as u64;

        // --- the Scan the adapter does today ---
        let before = meter.snapshot();
        let scanned = store::scan_latest(client, &table, store::now_cutoff_nanos(), Some(&meter))
            .await
            .expect("scan");
        let scan = meter.snapshot().minus(&before);
        checks.within(
            &format!("Scan at V={v} costs ~the aggregate bytes"),
            read_units(total_bytes, true),
            scan.read_units(),
        );
        checks.assert_true(
            &format!("...and resolves M={m} records"),
            scanned.len() == m,
        );

        // --- phase 1: the GSI Query ---
        let target_kind = "k0";
        let before = meter.snapshot();
        let mut candidates: HashSet<String> = HashSet::new();
        let mut phase1_items = 0u64;
        let mut start = None;
        loop {
            let mut req = client
                .query()
                .table_name(&table)
                .index_name(GSI_KIND)
                .key_condition_expression("#ix = :v AND #sk < :hi")
                .expression_attribute_names("#ix", IX_KIND)
                .expression_attribute_names("#sk", store::SK)
                .expression_attribute_values(":v", AttributeValue::S(target_kind.into()))
                .expression_attribute_values(
                    ":hi",
                    AttributeValue::S(store::upper_bound(store::now_cutoff_nanos())),
                )
                .return_consumed_capacity(aws_sdk_dynamodb::types::ReturnConsumedCapacity::Total);
            if let Some(k) = start.take() {
                req = req.set_exclusive_start_key(Some(k));
            }
            let out = req.send().await.expect("gsi query");
            meter.record(Op::Query, out.consumed_capacity());
            for item in out.items() {
                phase1_items += 1;
                if let Some(AttributeValue::S(pk)) = item.get(store::PK) {
                    candidates.insert(pk.clone());
                }
            }
            match out.last_evaluated_key() {
                Some(k) if !k.is_empty() => start = Some(k.clone()),
                _ => break,
            }
        }
        let phase1 = meter.snapshot().minus(&before);

        // --- phase 2: resolve each candidate, then re-match ---
        let before = meter.snapshot();
        let mut matched = 0usize;
        for id in &candidates {
            let resolved =
                store::query_latest(client, &table, id, store::now_cutoff_nanos(), Some(&meter))
                    .await
                    .expect("resolve");
            if let Some(env) = resolved {
                if !env.deleted
                    && env.payload.get("kind").and_then(|v| v.as_str()) == Some(target_kind)
                {
                    matched += 1;
                }
            }
        }
        let phase2 = meter.snapshot().minus(&before);

        let two_phase = phase1.read_units() + phase2.read_units();
        let derived_g = phase1.read_units() * 8192.0 / phase1_items.max(1) as f64;
        println!(
            "  phase 1: {} index items -> {:.1} RRU (implies G ~= {:.0} B/entry)",
            phase1_items,
            phase1.read_units(),
            derived_g
        );
        println!(
            "  phase 2: {} candidate ids -> {:.1} RRU ({} matched after re-check)",
            candidates.len(),
            phase2.read_units(),
            matched
        );
        println!(
            "  TOTAL two-phase {:.1} RRU  vs  Scan {:.1} RRU   => {:.2}x",
            two_phase,
            scan.read_units(),
            scan.read_units() / two_phase
        );

        checks.eq(
            "phase 2 is exactly 0.5 RRU per candidate id (the floor)",
            candidates.len() as f64 * 0.5,
            phase2.read_units(),
        );

        // The counterintuitive claim, asserted in the direction the model predicts.
        let f = 1.0 / kinds as f64;
        let predicted_win = f * (derived_g / 8192.0 + 1.0 / (2.0 * r as f64)) < 0.125;
        checks.assert_true(
            &format!(
                "model predicts the index {} here, and it {}",
                if predicted_win { "WINS" } else { "LOSES" },
                if two_phase < scan.read_units() {
                    "WON"
                } else {
                    "LOST"
                }
            ),
            predicted_win == (two_phase < scan.read_units()),
        );

        if r == 1 {
            checks.assert_true(
                "an r=1 event entity with a 3-value predicate is DEARER via the index",
                two_phase > scan.read_units(),
            );
        } else {
            checks.assert_true(
                "an r=10 projection is much cheaper via the index",
                two_phase * 5.0 < scan.read_units(),
            );
        }

        drop_table(client, &table).await;
    }
}

/// The correction: an `ALL`-projection index cannot resolve latest-per-id inside
/// its own results, so it cannot replace phase 2.
async fn a_projected_index_cannot_resolve_the_latest_version(client: &Client, checks: &mut Checks) {
    let table = table_name("sound");
    create_table(client, &table, &[(GSI_KIND, IX_KIND, ProjectionType::All)]).await;
    let meter = CapacityMeter::new();

    println!("\n== an ALL-projection GSI cannot replace phase 2 (soundness) ==");

    // "mover" changes kind: tool -> widget. "stayer" does not.
    put(
        client,
        &table,
        &meter,
        &envelope("mover", "tool", 400),
        true,
        None,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    put(
        client,
        &table,
        &meter,
        &envelope("stayer", "tool", 400),
        true,
        None,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    put(
        client,
        &table,
        &meter,
        &envelope("mover", "widget", 400),
        true,
        None,
    )
    .await;
    // Give the index a moment: a GSI is updated asynchronously.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // What the index alone says, grouping latest-per-id *within* the results and
    // re-checking the predicate on what it resolved — the proposed shortcut.
    let out = client
        .query()
        .table_name(&table)
        .index_name(GSI_KIND)
        .key_condition_expression("#ix = :v")
        .expression_attribute_names("#ix", IX_KIND)
        .expression_attribute_values(":v", AttributeValue::S("tool".into()))
        .send()
        .await
        .expect("gsi query");

    let mut within_index: HashMap<String, (String, Envelope)> = HashMap::new();
    for item in out.items() {
        let sk = match item.get(store::SK) {
            Some(AttributeValue::S(s)) => s.clone(),
            _ => continue,
        };
        let env = store::item_to_envelope(item).expect("envelope");
        match within_index.get(&env.id) {
            Some((seen, _)) if *seen >= sk => {}
            _ => {
                within_index.insert(env.id.clone(), (sk, env));
            }
        }
    }
    // Re-check the predicate against what the index resolved.
    let shortcut: HashSet<String> = within_index
        .values()
        .filter(|(_, e)| e.payload.get("kind").and_then(|v| v.as_str()) == Some("tool"))
        .map(|(_, e)| e.id.clone())
        .collect();

    // What the two-phase shape says.
    let mut two_phase: HashSet<String> = HashSet::new();
    for id in within_index.keys() {
        let resolved =
            store::query_latest(client, &table, id, store::now_cutoff_nanos(), Some(&meter))
                .await
                .expect("resolve");
        if let Some(env) = resolved {
            if !env.deleted && env.payload.get("kind").and_then(|v| v.as_str()) == Some("tool") {
                two_phase.insert(env.id.clone());
            }
        }
    }

    println!("  index-only shortcut returns: {shortcut:?}");
    println!("  two-phase returns:           {two_phase:?}");

    checks.assert_true(
        "the index-only shortcut WRONGLY returns `mover` (its latest kind is widget)",
        shortcut.contains("mover"),
    );
    checks.assert_true(
        "the two-phase shape correctly excludes `mover`",
        !two_phase.contains("mover"),
    );
    checks.assert_true(
        "both agree `stayer` matches",
        shortcut.contains("stayer") && two_phase.contains("stayer"),
    );
    checks.assert_true(
        "=> phase 2 is REQUIRED; KEYS_ONLY is therefore the right projection",
        shortcut != two_phase,
    );

    drop_table(client, &table).await;
}

// -------------------------------------------------------------------- main ---

#[tokio::main]
async fn main() {
    if std::env::var("MESHQL_DYNAMO_COST_TESTS").as_deref() != Ok("1") {
        println!(
            "SKIPPED: meshql-dynamo GSI design probe.\n  \
             Reason: MESHQL_DYNAMO_COST_TESTS is not set to 1.\n  \
             This probe bills a real AWS account (well under a cent) to measure a \
             design that is\n  recommended but NOT shipped — see \
             docs/cost-model-dynamodb.md.\n  \
             Run with: MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 cargo test \
             -p meshql-dynamo --test gsi_cost"
        );
        return;
    }
    if let Ok(endpoint) = std::env::var("MESHQL_DYNAMO_ENDPOINT") {
        println!(
            "SKIPPED: meshql-dynamo GSI design probe.\n  \
             Reason: MESHQL_DYNAMO_ENDPOINT is set to {endpoint:?}; DynamoDB Local \
             does not meter."
        );
        return;
    }

    let client = meshql_dynamo::make_client(None).await;
    if let Err(e) = client.list_tables().limit(1).send().await {
        println!(
            "SKIPPED: meshql-dynamo GSI design probe.\n  \
             Reason: no usable AWS credentials or region — ListTables failed.\n  \
             Detail: {e}"
        );
        return;
    }

    println!(
        "meshql-dynamo GSI design probe — REAL AWS DynamoDB, region {}.\n\
         Measures a design that is NOT shipped.",
        client
            .config()
            .region()
            .map(|r| r.to_string())
            .unwrap_or_else(|| "<none>".into())
    );

    let mut checks = Checks::default();
    write_amplification_is_one_unit_per_index(&client, &mut checks).await;
    a_projected_index_cannot_resolve_the_latest_version(&client, &mut checks).await;
    the_two_phase_query_priced_against_the_scan(&client, &mut checks).await;

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
        std::process::exit(1);
    }

    // Check only the tables *this run* created. Filtering the whole
    // `dynamocost-gsi` namespace looks equivalent and is not: another process
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
        .filter(|t| t.starts_with("dynamocost-gsi") && !mine.contains(t))
        .collect();
    if !others.is_empty() {
        println!(
            "NOTE: {} other dynamocost-gsi* table(s) exist and are NOT this run's: \
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
        println!("WARNING: tables still present: {leftovers:?}");
        std::process::exit(1);
    }
}

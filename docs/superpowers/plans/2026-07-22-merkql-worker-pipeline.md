# merkql CDC connector + shared worker pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Execution prerequisites — read before starting a single task:**
>
> **(a) Sequencing.** This plan depends on the three farm-retrofit plans landing first: `2026-07-22-farm-retrofit-rust.md`, `2026-07-22-farm-retrofit-java.md`, `2026-07-22-farm-retrofit-ts.md` (same `docs/superpowers/plans/` directory). Those plans make `lay_report` a real create-only event and `hen_productivity` a real projection restlette/graphlette, in all three languages. Do not start Task 1 until all three have merged. If they haven't landed, stop and say so — do not improvise around their absence.
>
> **(b) Schema reconciliation — RESOLVED, 2026-07-22, before Task 1 started.** All three retrofit plans landed. Diffing this plan's assumptions against the real, merged schemas turned up three real gaps, none requiring any change to the (already-completed, already-reviewed, and in Rust's case already-pushed) retrofit branches:
>
> 1. **`processedReportIds` does not exist anywhere.** All three languages landed exactly `{henId, totalEggs, lastLaidAt}` for `hen_productivity` — confirmed by reading all three `hen_productivity.schema.json`/`.graphql` files directly. Reopening three finished, reviewed branches (one already pushed to two remotes) to bolt on a dedup-bookkeeping field is worse than fixing the one plan that hasn't started. **Fix:** Task 6 no longer accumulates onto a stored total with an id-dedup list. Instead the worker recomputes `totalEggs` from scratch every time by summing every one of a hen's `lay_report`s (fetched fresh via the already-existing `...ByHen` vector query — no new query needed on any backend). A full recompute of the same underlying data always yields the same answer, so it's idempotent by construction under at-least-once redelivery — no ledger field, no unbounded array growth, and it matches the exact landed shape on all three languages with zero reconciliation debt. `HenProductivity` in this plan is now exactly `{id, henId, totalEggs, lastLaidAt}`, nothing more.
> 2. **`timeOfDay` is not a timestamp in two of the three languages.** Rust's `lay_report.schema.json` declares `timeOfDay: {type: string, format: date-time}` (an actual ISO instant), but Java's and TS's both declare `timeOfDay: {type: string, enum: [morning, afternoon, evening]}` (a category, not a point in time) — confirmed by reading all three schema files. This plan originally set `lastLaidAt = timeOfDay` verbatim, which would silently write the literal string `"evening"` into a `format: date-time` field on Java/TS. **Fix:** `lastLaidAt` is now sourced from the `ChangeEvent`'s own `created_at` (the envelope commit timestamp already carried on every `ThinEvent`, present identically on all three backends since it's a framework-level field, not a domain one), converted to ISO-8601, and merged via `max(current.last_laid_at, event_created_at_iso)`. A monotonic max is itself idempotent under redelivery of the same event (redelivering never lowers the value, and reapplying the same input yields the same output) — no `timeOfDay` value is ever read by the worker for this purpose. `timeOfDay` is still fetched as part of `LayReport` detail (useful for logging/future use) but no longer drives any written field.
> 3. **Query names diverge by language — a real bug in this plan's own "one binary, no rebuild" goal.** Rust's farm chose the entity-named dialect (`getLayReport`, `getLayReportsByHen`, `getHenProductivityByHen`); Java's and TS's both chose the generic dialect (`getById`, `getByHen`) — confirmed by reading all three `.graphql` files (see `meshql-patterns` skill's own documented split between these two dialects, which the three retrofits picked independently and inconsistently, entity-by-entity). A worker hardcoding Rust's query names would silently 404/error against Java or TS. **Fix:** `WorkerConfig` (Task 4) gains a `query_dialect: QueryDialect` field (`EntityNamed | Generic`, env var `QUERY_DIALECT`, default `EntityNamed` matching Rust, the reference deployment this plan was drafted against). Every query-name string used anywhere in this plan is now derived from `cfg.query_dialect`, never hardcoded — see the updated Tasks 5, 7, 8, 10 below.
>
> Every place this plan makes a schema-name assumption is still flagged inline with **"Schema-name assumption, flagged for reconciliation"** for anything not covered by the three fixes above; grep for that phrase before trusting any remaining GraphQL query string in this document.
>
> **(c) Isolation.** Execute this plan in a dedicated git worktree, not on the main checkout — per `superpowers:using-git-worktrees`. `meshql-rs` is not itself a git repo at `/tank/repos/tailoredshapes/meshql-rs` per this session's environment info, so confirm the actual repo root (likely a parent directory or a differently-checked-out clone — see project memory's `tailoredshares` vs `tailoredshapes` path-confusion note) before creating the worktree, and re-verify branch + SHA before every commit.

**Goal:** Extend `meshql-changes` with a merkql-writing sink (Component 1: the connector), and build a standalone, language-agnostic Rust worker (Component 2) that consumes `lay_report` change events off a merkql topic, looks up full detail via GraphQL, folds them into `hen_productivity`, and writes the result back via REST — proving the full `restlette → database → merkql-connector → merkql topic → worker → REST` pipeline described in `docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md`.

**Architecture:** Component 1 is a new `meshql-changes/src/merkql_sink.rs` module: an *additional* `ChangeHub` subscriber (alongside the existing SSE path, never instead of it) that mirrors every `ChangeEvent` onto a merkql topic named after its entity, using the same token-free wire shape SSE already uses. Component 2 is a new crate, `examples/farm-worker/`, a headless binary with no dependency on any specific farm language: it polls the `lay_report` merkql topic, queries the source graphlette for full event detail, applies a pure idempotent fold, and read-modify-writes `hen_productivity` via ordinary GraphQL (read) + REST (write) — the same "GraphQL exposes ids, REST doesn't, discover after POST" pattern already used by the Java `ProjectionUpdater` reference implementation (`/tank/repos/tailoredshapes/meshql/examples/egg-economy/src/main/java/com/meshql/examples/egg_economy/ProjectionUpdater.java`).

**Tech Stack:** Rust 2021, `merkql` (workspace dep, `git = "https://github.com/tailoredshapes/merkql", tag = "v0.2.0"`, already pinned in the workspace root `Cargo.toml` — reuse `merkql = { workspace = true }`, do not add a new pin), `reqwest` 0.12 (`json`, `rustls-tls` features — matches `meshql-restlette`'s `ValidatorContext` and `meshql-changes`' own dev-deps), `tokio`, `serde`/`serde_json`, `axum` (dev-dependency only, for standing up real test servers — matches `meshql-changes/tests/sse_integration.rs`'s convention; no mocking crate exists anywhere in this workspace, don't introduce one).

**Read the specs first:** `docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md` (source of truth for this plan) and `docs/superpowers/specs/2026-07-22-farm-event-sourcing-retrofit-design.md` (domain context — the schemas this plan assumes). Where this plan and the specs disagree, the specs win; stop and flag it.

**Conventions for every task:** TDD (`superpowers:test-driven-development`) — write the failing test, watch it fail, implement, watch it pass, commit. Run commands from the repo root (`/tank/repos/tailoredshapes/meshql-rs`, or the worktree's copy of it).

---

## Key existing signatures you will call (verified against source, do not guess)

```rust
// meshql-changes/src/event.rs — ChangeEvent, already exists, untouched by this plan
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,       // epoch millis, the store's commit time
    pub deleted: bool,
    pub authorized_tokens: Vec<String>, // NEVER serialized — see wire_json
}
impl ChangeEvent {
    pub fn wire_json(&self) -> String; // {"entity":..,"id":..,"created_at":..,"deleted":..} — no tokens
}

// meshql-changes/src/hub.rs — ChangeHub, already exists, untouched by this plan
#[derive(Clone)]
pub struct ChangeHub { /* ... */ }
impl ChangeHub {
    pub fn new(capacity: usize) -> Self;
    pub fn publish(&self, event: ChangeEvent);
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ChangeEvent>;
}
pub async fn run_tails(hub: ChangeHub, sources: Vec<Arc<dyn ChangeSource>>, interval: Duration);

// meshql-changes/src/tail.rs — SearcherTail, already exists, untouched by this plan
impl SearcherTail {
    pub fn new(entity: impl Into<String>, searcher: Arc<dyn Searcher>, repository: Arc<dyn Repository>) -> Self;
}

// merkql — verified directly against /tank/repos/tailoredshapes/merkql/src/{broker,consumer,producer,record}.rs
pub type BrokerRef = Arc<Broker>;
pub struct BrokerConfig { pub data_dir: PathBuf, pub default_partitions: u32, pub auto_create_topics: bool, /* ... */ }
impl BrokerConfig { pub fn new(data_dir: impl Into<PathBuf>) -> Self; } // auto_create_topics: true by default
impl Broker {
    pub fn open(config: BrokerConfig) -> Result<BrokerRef>;
    pub fn consumer(broker: &BrokerRef, config: ConsumerConfig) -> Consumer;
    pub fn producer(broker: &BrokerRef) -> Producer;
}
pub struct ConsumerConfig { pub group_id: String, pub auto_commit: bool, pub offset_reset: OffsetReset }
pub enum OffsetReset { Earliest, Latest }
impl Consumer {
    pub fn subscribe(&mut self, topics: &[&str]) -> Result<()>;
    pub fn poll(&mut self, timeout: Duration) -> Result<Vec<Record>>;
    pub fn commit_sync(&mut self) -> Result<()>;
}
impl Producer { pub fn send(&self, record: &ProducerRecord) -> Result<Record>; } // auto-creates topic
pub struct ProducerRecord { /* ... */ }
impl ProducerRecord { pub fn new(topic: impl Into<String>, key: Option<String>, value: impl Into<String>) -> Self; }
pub struct Record { pub key: Option<String>, pub value: String, pub topic: String, pub partition: u32, pub offset: u64, pub timestamp: DateTime<Utc> }

// meshql-core — Auth/Repository/Searcher/config types (unchanged by this plan)
pub type Stash = serde_json::Map<String, serde_json::Value>;
pub struct Envelope { pub id: String, pub payload: Stash, pub created_at: DateTime<Utc>, pub deleted: bool, pub authorized_tokens: Vec<String> }
pub struct GraphletteConfig { pub path: String, pub schema_text: String, pub root_config: RootConfig, pub searcher: Arc<dyn Searcher> }
pub struct RestletteConfig { pub path: String, pub schema_json: serde_json::Value, pub repository: Arc<dyn Repository> }
pub struct ServerConfig { pub port: u16, pub graphlettes: Vec<GraphletteConfig>, pub restlettes: Vec<RestletteConfig> }
// RootConfig::builder().singleton(name, template).vector(name, template)...build()

// meshql-server
pub async fn run(config: ServerConfig) -> anyhow::Result<()>;
pub async fn run_ext(config: ServerConfig, extra: axum::Router) -> anyhow::Result<()>;
```

## Facts to respect (verified against source; each cost real time to find — do not rediscover the hard way)

- **`ChangeEvent::wire_json()` already IS the on-topic shape the spec asks for.** Component 1's sink does not need a new wire format — `{entity, id, created_at, deleted}`, tokens stripped by construction. Reuse it verbatim as the merkql record value.
- **Payload fields need a `"payload."` prefix in EVERY query template, on EVERY backend** — not just Mongo. Verified in `meshql-mongo/src/converters.rs` (`envelope_to_document` nests the whole payload under a `"payload"` BSON subdocument, and `MongoSearcher::build_pipeline` matches the raw document, so a bare `"henId"` key matches nothing) and independently in `meshql-sqlite/src/query.rs::build_where` (`else if let Some(field) = key.strip_prefix("payload.") { json_extract(...) } else { /* Unknown key — skip */ }` — a bare `"henId"` key is silently dropped, not an error). Any `RootConfig` vector/singleton template that filters on a payload field (e.g. `hen_productivity`'s `getHenProductivityByHen`) MUST use `"payload.henId"`, never `"henId"`. `"id"` is the one exception — it's a top-level Envelope field on both backends, so `{"id": "{{id}}"}` (as farm's existing `getLayReport`/`getFarm`/etc. queries already do) is correct as-is.
- **REST `POST` never lets the caller choose the Envelope id** — `meshql-restlette/src/routes.rs::create_handler` always does `let id = Uuid::new_v4().to_string();`, ignoring anything in the request body. There is no REST-only way to upsert a projection keyed by a natural id (`henId`). `PUT /<entity>/api/:id` (`update_handler`) is the only route that takes an id from the caller, and it does a read-merge-write under that exact id (a new Envelope *version*, not a new envelope) — this is what makes `hen_productivity` a true per-hen upsert rather than a pile of disconnected creates. The worker therefore must discover a hen's existing `hen_productivity` id via GraphQL (which *does* expose ids — "GraphQL exposes ids because it's a query interface, not a resource interface") before it knows whether to `POST` (first time) or `PUT /:id` (every time after). This is exactly the Java `ProjectionUpdater` reference pattern at `/tank/repos/tailoredshapes/meshql/examples/egg-economy/src/main/java/com/meshql/examples/egg_economy/ProjectionUpdater.java` — read it before Task 7, this plan's `rest_client.rs` is a direct Rust port of its `getProjection`/`createProjection`/`updateProjection` methods.
- **`merkql::Consumer::poll` advances its in-memory read position to the batch's tail as soon as it reads records — *before* the caller processes any of them** (verified in `merkql/src/consumer.rs::poll`: `*position = tail;` happens immediately after `read_range`, not after `commit_sync`). Consequence: if the worker fails partway through a polled batch, it must NOT call `commit_sync()` (that would durably record a position past records it never actually processed), and it must NOT simply call `poll()` again on the *same* `Consumer` next tick — that would return an empty batch forever, since the in-memory position already points past the unprocessed records that need retrying, and there is no unread API to roll it back. The correct retry unit is a **fresh `Consumer`, re-subscribed each tick**, which reads its starting position from the group's last *committed* offset (durable, on-disk, unaffected by the previous tick's abandoned in-memory advance). Task 8 implements this; get it right, it is the crux of the whole backpressure story the spec asks for.
- **A fresh `Consumer` per tick also sidesteps a startup race**: `Consumer::subscribe` looks up `self.broker.topic(topic_name)` once, at subscribe time; if the topic doesn't exist yet (the connector hasn't produced to it yet), the consumer's position map for that topic stays empty forever, even after the topic is later created — *unless* the caller subscribes again. Rebuilding the `Consumer` every tick (already required for the point above) means the worker will correctly pick up the topic the very first tick after the connector creates it, with no special-case code.
- **The worker's fold must be idempotent under at-least-once delivery, or replay corrupts `hen_productivity`.** Naively doing `total_eggs = current.total_eggs + report.eggs` on every delivery double-counts a redelivered event (a genuine risk here: a batch that fails halfway is retried in full next tick). `domain-design.md` states this as a hard requirement ("A worker fold that isn't deterministic/idempotent... breaks replay and makes at-least-once delivery unsafe"). Task 6 fixes this WITHOUT a dedup ledger: `total_eggs` is recomputed from scratch every time by summing a fresh fetch of every one of the hen's `lay_report`s (Task 5's `fetch_lay_reports_for_hen`), and `last_laid_at` is merged via `max(current, event_created_at)` — both operations are idempotent by construction under redelivery. See the schema-reconciliation note in the header above for why this replaced the plan's original accumulate-plus-`processedReportIds` design.
- **`Box::leak(Box::new(tempfile::tempdir().unwrap()))` is the established pattern** for keeping a merkql broker's backing directory alive for a whole test process — see `examples/egg-economy/tests/pipeline.rs::broker()`. Use it verbatim; a plain `tempfile::tempdir()` local variable would be dropped (and its directory deleted) while the broker still holds file handles into it.
- **No mocking crate exists in this workspace.** `meshql-changes/tests/sse_integration.rs` stands up a real `axum::serve` on `127.0.0.1:0` and drives it with a real `reqwest::Client` for its integration tests. Follow the same pattern for every test in this plan that needs an HTTP peer — do not add `wiremock`/`mockito`/`httpmock`.

---

## Task 1: `meshql-changes` — the merkql producer half (`publish_to_merkql`)

**Files:**
- Modify: `meshql-rs/meshql-changes/Cargo.toml`
- Create: `meshql-rs/meshql-changes/src/merkql_sink.rs`
- Test: same file, `#[cfg(test)] mod tests` (matches this crate's existing convention — every module in `meshql-changes` keeps its tests inline, see `hub.rs`, `sse.rs`, `tail.rs`)

- [ ] **Step 1: Add the `merkql` dependency and a `tempfile` dev-dependency**

Edit `meshql-rs/meshql-changes/Cargo.toml`:

```toml
[dependencies]
meshql-core = { version = "0.1.0", path = "../meshql-core" }
merkql = { workspace = true }
axum = { workspace = true }
tokio = { workspace = true }
tokio-stream = { version = "0.1", features = ["sync"] }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
async-trait = { workspace = true }
anyhow = "1"

[dev-dependencies]
meshql-sqlite = { version = "0.1.0", path = "../meshql-sqlite" }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
futures = "0.3"
tempfile = "3"
```

(Only two lines changed from the current file: `merkql = { workspace = true }` added to `[dependencies]`, `tempfile = "3"` added to `[dev-dependencies]`.)

- [ ] **Step 2: Write the failing test**

Create `meshql-rs/meshql-changes/src/merkql_sink.rs`:

```rust
//! The merkql-writing sink: an ADDITIONAL ChangeHub subscriber that mirrors
//! every ChangeEvent onto a merkql topic (one topic per entity), alongside —
//! never instead of — whatever's already broadcasting to SSE subscribers.
//! `ChangeHub`/`run_tails` are untouched; this is a second `hub.subscribe()`
//! consumer, per the pipeline design
//! (docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md).

use crate::ChangeEvent;

#[cfg(test)]
mod tests {
    use super::*;
    use merkql::broker::{Broker, BrokerConfig, BrokerRef};
    use merkql::consumer::{ConsumerConfig, OffsetReset};
    use std::time::Duration;

    fn broker() -> BrokerRef {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Broker::open(BrokerConfig::new(dir.path())).unwrap()
    }

    fn ev(entity: &str, id: &str, created_at: i64) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(),
            id: id.into(),
            created_at,
            deleted: false,
            authorized_tokens: vec!["farm-team".into()],
        }
    }

    #[test]
    fn publish_writes_a_token_free_record_to_the_entity_topic() {
        let broker = broker();
        publish_to_merkql(&broker, &ev("lay_report", "lr-1", 1000)).unwrap();

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: "test".into(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&["lay_report"]).unwrap();
        let records = consumer.poll(Duration::from_millis(50)).unwrap();
        assert_eq!(records.len(), 1);

        let v: serde_json::Value = serde_json::from_str(&records[0].value).unwrap();
        assert_eq!(v["entity"], "lay_report");
        assert_eq!(v["id"], "lr-1");
        assert_eq!(v["created_at"], 1000);
        assert_eq!(v["deleted"], false);
        assert!(
            !records[0].value.contains("farm-team"),
            "tokens must never reach the merkql topic"
        );
        assert_eq!(records[0].key.as_deref(), Some("lr-1"));
    }

    #[test]
    fn different_entities_route_to_different_topics() {
        let broker = broker();
        publish_to_merkql(&broker, &ev("lay_report", "lr-1", 1)).unwrap();
        publish_to_merkql(&broker, &ev("hen_productivity", "hp-1", 2)).unwrap();

        for (topic, id) in [("lay_report", "lr-1"), ("hen_productivity", "hp-1")] {
            let mut consumer = Broker::consumer(
                &broker,
                ConsumerConfig {
                    group_id: format!("test-{topic}"),
                    auto_commit: false,
                    offset_reset: OffsetReset::Earliest,
                },
            );
            consumer.subscribe(&[topic]).unwrap();
            let records = consumer.poll(Duration::from_millis(50)).unwrap();
            assert_eq!(records.len(), 1, "expected exactly one record on topic {topic}");
            assert_eq!(records[0].key.as_deref(), Some(id));
        }
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cargo test -p meshql-changes --lib merkql_sink -- --nocapture
```

Expected: compile error — `publish_to_merkql` is not defined (the module only has the doc comment and `use` so far).

- [ ] **Step 4: Write the minimal implementation**

Add above the `#[cfg(test)]` block in `merkql_sink.rs`:

```rust
use merkql::broker::{Broker, BrokerRef};
use merkql::record::ProducerRecord;

/// Publish one `ChangeEvent` onto the merkql topic named after its entity.
/// Auto-creates the topic on first write (merkql's `Producer::send` default,
/// `BrokerConfig::auto_create_topics == true`). Record key is the event's
/// `id`, so every event for the same entity instance routes to the same
/// partition and is delivered to a consumer in commit order — mirrors
/// `examples/egg-economy/src/source.rs::publish`.
pub fn publish_to_merkql(broker: &BrokerRef, event: &ChangeEvent) -> anyhow::Result<()> {
    let producer = Broker::producer(broker);
    let record = ProducerRecord::new(
        event.entity.clone(),
        Some(event.id.clone()),
        event.wire_json(),
    );
    producer
        .send(&record)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(())
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p meshql-changes --lib merkql_sink -- --nocapture
```

Expected: `test merkql_sink::tests::publish_writes_a_token_free_record_to_the_entity_topic ... ok` and `test merkql_sink::tests::different_entities_route_to_different_topics ... ok`.

- [ ] **Step 6: Commit**

```bash
git add meshql-changes/Cargo.toml meshql-changes/src/merkql_sink.rs
git commit -m "$(cat <<'EOF'
meshql-changes: add publish_to_merkql, the connector's produce side

One merkql record per ChangeEvent, keyed by id, one topic per entity,
reusing ChangeEvent::wire_json()'s existing token-free wire shape — no
new format needed. Part 1 of 2 for the merkql-writing sink; the
subscriber loop that drives this off ChangeHub lands next.
EOF
)"
```

---

## Task 2: `meshql-changes` — the subscriber loop (`run_merkql_sink`)

**Files:**
- Modify: `meshql-rs/meshql-changes/src/merkql_sink.rs`
- Modify: `meshql-rs/meshql-changes/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `merkql_sink.rs` (needs `use crate::ChangeHub;` added to the test imports):

```rust
    use crate::ChangeHub;

    #[tokio::test]
    async fn run_merkql_sink_mirrors_hub_events_by_entity_topic() {
        let broker = broker();
        let hub = ChangeHub::new(16);
        tokio::spawn(run_merkql_sink(hub.subscribe(), broker.clone()));

        hub.publish(ev("lay_report", "lr-1", 1000));
        hub.publish(ev("hen_productivity", "hp-1", 2000));
        tokio::time::sleep(Duration::from_millis(50)).await;

        for (topic, id) in [("lay_report", "lr-1"), ("hen_productivity", "hp-1")] {
            let mut consumer = Broker::consumer(
                &broker,
                ConsumerConfig {
                    group_id: format!("test-{topic}"),
                    auto_commit: false,
                    offset_reset: OffsetReset::Earliest,
                },
            );
            consumer.subscribe(&[topic]).unwrap();
            let records = consumer.poll(Duration::from_millis(50)).unwrap();
            assert_eq!(records.len(), 1, "expected one record on topic {topic}");
            assert_eq!(records[0].key.as_deref(), Some(id));
        }
    }

    #[tokio::test]
    async fn run_merkql_sink_survives_lag_and_keeps_mirroring() {
        let broker = broker();
        let hub = ChangeHub::new(2); // tiny buffer, easy to overrun before the task is scheduled
        let rx = hub.subscribe();
        tokio::spawn(run_merkql_sink(rx, broker.clone()));

        for i in 0..10 {
            hub.publish(ev("lay_report", &format!("e{i}"), i as i64));
        }
        // Published after the burst; must still land — proves the task
        // logged-and-continued past the Lagged error instead of exiting
        // (mirrors run_tails's "poll errors ... never fatal" contract).
        hub.publish(ev("lay_report", "sentinel", 999));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: "test".into(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&["lay_report"]).unwrap();
        let records = consumer.poll(Duration::from_millis(50)).unwrap();
        assert!(
            records.iter().any(|r| r.key.as_deref() == Some("sentinel")),
            "sink must still be running (and mirroring) after a broadcast lag"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p meshql-changes --lib merkql_sink -- --nocapture
```

Expected: compile error — `run_merkql_sink` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `merkql_sink.rs`, after `publish_to_merkql`:

```rust
use tokio::sync::broadcast;

/// Subscribe to the hub and mirror every event onto merkql, forever. Spawn
/// this ALONGSIDE `run_tails` — `tokio::spawn(run_merkql_sink(hub.subscribe(), broker.clone()))`
/// — never in place of it; the SSE path is untouched.
///
/// Lag handling: unlike the SSE path (a lagged client just reconnects and
/// refetches through GraphQL — the notification is lost but no data is),
/// a lagged merkql sink has no equivalent recovery: the events it missed
/// are never written to the topic, and the worker downstream has no other
/// way to learn about them. This is logged loudly rather than silently
/// skipped. Sizing `ChangeHub::new(capacity)` generously relative to write
/// burst volume is the operator's mitigation; driving the sink directly off
/// `ChangeSource::poll` instead of the broadcast hub (bypassing this whole
/// class of loss) is a real improvement but out of scope for this additive
/// change — flagged here, not solved here, per the spec's framing of the
/// sink as "simply an additional subscriber task."
pub async fn run_merkql_sink(mut rx: broadcast::Receiver<ChangeEvent>, broker: BrokerRef) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Err(e) = publish_to_merkql(&broker, &event) {
                    eprintln!("[merkql-sink {}] publish: {e}", event.entity);
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!(
                    "[merkql-sink] lagged by {n} events — those events were NEVER \
                     written to merkql; increase ChangeHub capacity"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
```

Edit `meshql-rs/meshql-changes/src/lib.rs` — add the module and export:

```rust
mod event;
mod hub;
mod merkql_sink;
mod source;
mod sse;
mod tail;
pub mod testing;

pub use event::ChangeEvent;
pub use hub::{run_tails, ChangeHub};
pub use merkql_sink::{publish_to_merkql, run_merkql_sink};
pub use source::ChangeSource;
pub use sse::{change_stream, changes_router};
pub use tail::SearcherTail;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p meshql-changes --lib merkql_sink -- --nocapture
```

Expected: all four tests in `merkql_sink::tests` pass. Then run the whole crate to confirm nothing else broke:

```bash
cargo test -p meshql-changes
```

- [ ] **Step 5: Commit**

```bash
git add meshql-changes/src/merkql_sink.rs meshql-changes/src/lib.rs
git commit -m "$(cat <<'EOF'
meshql-changes: add run_merkql_sink, the connector's subscriber loop

An additional ChangeHub subscriber (spawn alongside run_tails, never
instead of it) that mirrors every event onto its entity's merkql
topic. Completes Component 1 of the merkql worker pipeline design —
meshql-changes now feeds both SSE subscribers and a merkql-backed
worker pipeline from the same tail, unmodified.
EOF
)"
```

---

## Task 3: Wire the connector into `examples/farm`

**Files:**
- Modify: `meshql-rs/examples/farm/Cargo.toml`
- Modify: `meshql-rs/examples/farm/src/main.rs`

> **Caveat before touching this file**: the retrofit plans (prerequisite (a) above) change `examples/farm/src/main.rs` substantially — new `hen_productivity` mesh, a from-scratch manifest generator, per-restlette Casbin `Auth` instances, a `lay_report` schema/field-shape change. The code below is written against the file as it exists **today** (read in full during planning; reproduced in the "current state" block below) so you have a concrete anchor, but you must locate the *post-retrofit* equivalent of the `lay_report` repo/searcher construction — variable names may differ (e.g. if the retrofit introduces a shared `mesh()` helper like `examples/egg-economy/src/main.rs` already does). Adapt the line numbers; do not blindly paste.

**Current state (pre-retrofit, for orientation only):**

```rust
// examples/farm/src/main.rs, lines 25-26 and 35-36, as of this plan's writing:
let lay_report_repo =
    Arc::new(MongoRepository::new(MONGO_URI, DB_NAME, "lay_reports", Arc::clone(&auth)).await?);
// ...
let lay_report_searcher: Arc<dyn meshql_core::Searcher> =
    Arc::new(MongoSearcher::new(MONGO_URI, DB_NAME, "lay_reports", Arc::clone(&auth)).await?);
```

- [ ] **Step 1: Add dependencies**

Edit `meshql-rs/examples/farm/Cargo.toml`:

```toml
[package]
name = "farm"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "farm"
path = "src/main.rs"

[dependencies]
meshql-core = { path = "../../meshql-core" }
meshql-mongo = { path = "../../meshql-mongo" }
meshql-graphlette = { path = "../../meshql-graphlette" }
meshql-restlette = { path = "../../meshql-restlette" }
meshql-server = { path = "../../meshql-server" }
meshql-changes = { path = "../../meshql-changes" }
merkql = { workspace = true }
tokio = { workspace = true }
serde_json = { workspace = true }
anyhow = "1"
```

(Added `meshql-changes` and `merkql` — everything else is unchanged.)

- [ ] **Step 2: No new automated test for this step** — this is deployment wiring inside a binary's `main()`, exercised end-to-end by Task 10's integration test instead (which stands up the same wiring pattern in-process against sqlite, per this workspace's established no-mocking-crate convention). Confirm compilation instead:

```bash
cargo check -p farm
```

Expected: fails to compile until Step 3 lands (unused-import or missing-symbol errors once the imports below are added but the wiring isn't).

- [ ] **Step 3: Wire the merkql broker + `lay_report` tail + sink into `main()`**

In `examples/farm/src/main.rs`, add imports near the top:

```rust
use merkql::broker::{Broker, BrokerConfig};
use meshql_changes::{run_merkql_sink, run_tails, ChangeHub, ChangeSource, SearcherTail};
use std::time::Duration;
```

Immediately after the `lay_report_repo`/`lay_report_searcher` construction (or their post-retrofit equivalent — see caveat above) and before the `ServerConfig { ... }` is assembled, add:

```rust
    // ===== Component 1 of the merkql worker pipeline: the connector =====
    // Tails farm's lay_report Mongo collection (via the existing,
    // certified SearcherTail — no new storage code) and mirrors committed
    // writes onto a merkql topic, in addition to whatever SSE change feed
    // this deployment already runs. This is the ONLY entity this farm
    // deployment tails for merkql today — the worker pipeline only needs
    // lay_report; add more SearcherTail entries here if a future pipeline
    // needs another entity's changes on merkql too.
    let merkql_dir =
        std::env::var("MERKQL_DIR").unwrap_or_else(|_| "./farm-changes-log".to_string());
    let broker = Broker::open(BrokerConfig::new(&merkql_dir))?;

    let change_hub = ChangeHub::new(256);
    let lay_report_tail: Arc<dyn ChangeSource> = Arc::new(SearcherTail::new(
        "lay_report",
        Arc::clone(&lay_report_searcher),
        lay_report_repo.clone() as Arc<dyn meshql_core::Repository>,
    ));
    tokio::spawn(run_tails(
        change_hub.clone(),
        vec![lay_report_tail],
        Duration::from_millis(500),
    ));
    tokio::spawn(run_merkql_sink(change_hub.subscribe(), broker.clone()));
```

(`Arc` is already imported by the existing file's `use std::sync::Arc;`.)

- [ ] **Step 4: Verify it compiles and the existing example still runs**

```bash
cargo check -p farm
cargo build -p farm
```

Expected: clean build. (Running the binary requires a live MongoDB, per the existing example's own requirements — unchanged by this task; do not stand up Mongo for this step, `cargo build` is sufficient verification here. Full behavioral verification happens in Task 10 against sqlite.)

- [ ] **Step 5: Commit**

```bash
git add examples/farm/Cargo.toml examples/farm/src/main.rs
git commit -m "$(cat <<'EOF'
examples/farm: wire the merkql connector for lay_report

Tails the lay_report Mongo collection with the existing SearcherTail
and mirrors changes onto a merkql topic via run_merkql_sink, additive
to the existing SSE change feed. Deployment half of Component 1 —
gives the farm-worker crate (Component 2) something real to consume.
EOF
)"
```

---

## Task 4: `farm-worker` crate scaffold + `WorkerConfig`

**Files:**
- Create: `meshql-rs/examples/farm-worker/Cargo.toml`
- Create: `meshql-rs/examples/farm-worker/src/lib.rs`
- Create: `meshql-rs/examples/farm-worker/src/config.rs`
- Modify: `meshql-rs/Cargo.toml` (workspace members)

**Crate location, decided here:** `examples/farm-worker/`, not a bare top-level crate and not nested inside `examples/farm/`. Reasoning: `merkql-architecture.md` is explicit that "a worker is an independent process (not part of `meshql-server`)"; it must be genuinely standalone since one compiled binary is pointed at Rust, Java, or TS farm deployments purely via config (never a rebuild), so it cannot live inside any one language's example directory. But it is not a reusable library other crates depend on either (unlike `meshql-changes`, `meshql-core`, etc.) — it is a *runnable demonstration of the pattern* for the farm domain specifically, exactly like `examples/egg-economy-lambda` and `examples/egg-economy-ksql` are runnable variants of `examples/egg-economy`. `examples/` is where this workspace puts runnable deployment shapes; `farm-worker` belongs there.

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/src/config.rs`:

```rust
//! Worker configuration — one compiled binary, pointed at any farm
//! deployment (Rust, Java, or TS) purely via env vars, matching the
//! existing MONGO_URI/PLATFORM_URL env-var pattern used across this
//! workspace's examples. `from_lookup` takes a plain key->value lookup
//! (rather than reading `std::env` directly) so tests can exercise every
//! branch without mutating real process env vars — env var mutation is
//! process-global and races across parallel `cargo test` threads.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn from_lookup_applies_defaults_when_nothing_is_set() {
        let cfg = WorkerConfig::from_lookup(|_| None);
        assert_eq!(cfg.merkql_dir, PathBuf::from("./farm-changes-log"));
        assert_eq!(cfg.topic, "lay_report");
        assert_eq!(cfg.group_id, "hen-productivity-worker");
        assert_eq!(cfg.poll_interval, Duration::from_millis(500));
        assert_eq!(cfg.source_graphql_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.target_rest_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.target_graphql_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.auth_header, None);
        assert_eq!(cfg.auth_value, "worker");
        assert_eq!(cfg.query_dialect, QueryDialect::EntityNamed);
    }

    #[test]
    fn from_lookup_honors_overrides_and_defaults_target_graphql_to_target_rest() {
        let vars: HashMap<&str, &str> = [
            ("SOURCE_GRAPHQL_URL", "http://rust-farm:3033"),
            ("TARGET_REST_URL", "http://java-farm:8080"),
            ("WORKER_AUTH_HEADER", "x-worker-token"),
            ("WORKER_POLL_INTERVAL_MS", "250"),
            ("QUERY_DIALECT", "generic"),
        ]
        .into();
        let cfg = WorkerConfig::from_lookup(|k| vars.get(k).map(|s| s.to_string()));

        assert_eq!(cfg.source_graphql_base, "http://rust-farm:3033");
        assert_eq!(cfg.target_rest_base, "http://java-farm:8080");
        // TARGET_GRAPHQL_URL was not set → defaults to TARGET_REST_URL. This
        // is what lets one worker binary point at a whole farm deployment
        // (Rust, Java, or TS) with a single URL when REST and GraphQL share
        // a base, per the spec's "purely a config change, never a rebuild."
        assert_eq!(cfg.target_graphql_base, "http://java-farm:8080");
        assert_eq!(cfg.auth_header, Some("x-worker-token".to_string()));
        assert_eq!(cfg.poll_interval, Duration::from_millis(250));
        // Java's and TS's farm retrofits both landed the generic dialect
        // (getById/getByHen) rather than Rust's entity-named one — see the
        // reconciliation note at the top of this plan. QUERY_DIALECT is
        // what lets the SAME worker binary point at either.
        assert_eq!(cfg.query_dialect, QueryDialect::Generic);
    }

    #[test]
    fn from_lookup_defaults_query_dialect_to_entity_named_on_unrecognized_value() {
        let vars: HashMap<&str, &str> = [("QUERY_DIALECT", "not-a-real-dialect")].into();
        let cfg = WorkerConfig::from_lookup(|k| vars.get(k).map(|s| s.to_string()));
        assert_eq!(cfg.query_dialect, QueryDialect::EntityNamed);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p farm-worker --lib config
```

Expected: fails — the crate doesn't exist yet (no `Cargo.toml`, not a workspace member). This step's real "fail" evidence is the Step 4 build failing before Step 5's implementation; proceed to scaffold the crate first since `cargo test -p farm-worker` cannot even resolve without it.

- [ ] **Step 3: Scaffold the crate**

Create `meshql-rs/examples/farm-worker/Cargo.toml`:

```toml
[package]
name = "farm-worker"
version = "0.1.0"
edition = "2021"
description = "Shared, language-agnostic worker: consumes lay_report events off a merkql topic (written by the meshql-changes merkql sink) and folds them into hen_productivity via REST/GraphQL against any farm deployment (Rust, Java, or TS)."

[lib]
name = "farm_worker"
path = "src/lib.rs"

[[bin]]
name = "farm-worker"
path = "src/main.rs"

[dependencies]
merkql = { workspace = true }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
tokio = { workspace = true }
anyhow = "1"

[dev-dependencies]
axum = { workspace = true }
tempfile = "3"
meshql-core = { path = "../../meshql-core" }
meshql-sqlite = { path = "../../meshql-sqlite" }
meshql-graphlette = { path = "../../meshql-graphlette" }
meshql-restlette = { path = "../../meshql-restlette" }
meshql-server = { path = "../../meshql-server" }
meshql-changes = { path = "../../meshql-changes" }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite"] }
```

Create `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
```

Create a placeholder `meshql-rs/examples/farm-worker/src/main.rs` (filled in for real in Task 9):

```rust
fn main() {
    println!("farm-worker: not yet wired — see Task 9 of the merkql worker pipeline plan");
}
```

Edit `meshql-rs/Cargo.toml` — add `"examples/farm-worker"` to `members`:

```toml
[workspace]
members = [
    "meshql-core",
    "meshql-mongo",
    "meshql-graphlette",
    "meshql-restlette",
    "meshql-mcp",
    "meshql-casbin",
    "meshql-server",
    "meshql-cert",
    "meshql-merkql",
    "meshql-merksql",
    "meshql-changes",
    "meshql-sqlite",
    "meshql-postgres",
    "meshql-mysql",
    "examples/farm",
    "examples/farm-worker",
    "examples/egg-economy",
    "examples/egg-economy-sap",
    "examples/egg-economy-salesforce",
    "meshql-lambda",
    "meshql-ksql",
    "examples/egg-economy-lambda",
    "examples/egg-economy-ksql",
    "examples/farm-azure",
]
resolver = "2"
```

- [ ] **Step 4: Implement `WorkerConfig`**

Add to `meshql-rs/examples/farm-worker/src/config.rs`, above the `#[cfg(test)]` block:

```rust
use std::path::PathBuf;
use std::time::Duration;

/// The two GraphQL query-naming dialects the three farm retrofits landed on
/// (see `meshql-patterns`' documented split, and the reconciliation note at
/// the top of this plan for which language picked which). `EntityNamed` is
/// what Rust's farm uses; `Generic` is what Java's and TS's both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryDialect {
    EntityNamed,
    Generic,
}

impl QueryDialect {
    /// The lay_report singleton query: `getLayReport(id, at)` (entity-named)
    /// vs `getById(id, at)` (generic).
    pub fn lay_report_by_id(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getLayReport",
            QueryDialect::Generic => "getById",
        }
    }

    /// The lay_report-vector-by-hen query: `getLayReportsByHen(id, at)` vs
    /// `getByHen(id, at)`.
    pub fn lay_reports_by_hen(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getLayReportsByHen",
            QueryDialect::Generic => "getByHen",
        }
    }

    /// The hen_productivity-by-hen query: `getHenProductivityByHen(id, at)`
    /// vs `getByHen(id, at)`.
    pub fn hen_productivity_by_hen(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getHenProductivityByHen",
            QueryDialect::Generic => "getByHen",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub merkql_dir: PathBuf,
    pub topic: String,
    pub group_id: String,
    pub poll_interval: Duration,
    pub source_graphql_base: String,
    pub target_rest_base: String,
    pub target_graphql_base: String,
    pub auth_header: Option<String>,
    pub auth_value: String,
    pub query_dialect: QueryDialect,
}

impl WorkerConfig {
    /// Read configuration from real process env vars.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable core: takes a lookup function instead of touching
    /// `std::env` directly.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let default_base = "http://127.0.0.1:3033".to_string();
        let source_graphql_base =
            lookup("SOURCE_GRAPHQL_URL").unwrap_or_else(|| default_base.clone());
        let target_rest_base = lookup("TARGET_REST_URL").unwrap_or_else(|| default_base.clone());
        let target_graphql_base =
            lookup("TARGET_GRAPHQL_URL").unwrap_or_else(|| target_rest_base.clone());
        let query_dialect = match lookup("QUERY_DIALECT").as_deref() {
            Some("generic") => QueryDialect::Generic,
            // Anything else (unset, "entity-named", or an unrecognized
            // value) defaults to EntityNamed — Rust's dialect, the
            // deployment this plan was drafted and end-to-end tested
            // against.
            _ => QueryDialect::EntityNamed,
        };
        Self {
            merkql_dir: lookup("MERKQL_DIR")
                .unwrap_or_else(|| "./farm-changes-log".to_string())
                .into(),
            topic: lookup("WORKER_TOPIC").unwrap_or_else(|| "lay_report".to_string()),
            group_id: lookup("WORKER_GROUP_ID")
                .unwrap_or_else(|| "hen-productivity-worker".to_string()),
            poll_interval: Duration::from_millis(
                lookup("WORKER_POLL_INTERVAL_MS")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(500),
            ),
            source_graphql_base,
            target_rest_base,
            target_graphql_base,
            // Not yet reconciled against the retrofit's actual Casbin
            // wiring (CasbinAuth resolves identity via a trusted-header ->
            // Stash key -> role chain the retrofit plans build; the exact
            // header name/value that maps to the "worker" role is decided
            // there, not here). This env-var-configurable header is a
            // deliberately generic placeholder that honors the pipeline
            // spec's requirement ("the worker authenticates as the worker
            // role") without hardcoding a mechanism not yet built — set
            // WORKER_AUTH_HEADER/WORKER_AUTH_TOKEN to whatever the landed
            // retrofit's edge middleware actually expects.
            auth_header: lookup("WORKER_AUTH_HEADER"),
            auth_value: lookup("WORKER_AUTH_TOKEN").unwrap_or_else(|| "worker".to_string()),
            query_dialect,
        }
    }
}
```

Edit `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
```

(No change needed — `config` was already the only module. This step is a no-op on `lib.rs`; listed for completeness.)

- [ ] **Step 5: Run the test to verify it passes, then commit**

```bash
cargo test -p farm-worker --lib config
cargo build --workspace   # confirm adding the new member didn't break anything else
```

Expected: all three `config::tests` pass; the full workspace still builds.

```bash
git add Cargo.toml examples/farm-worker
git commit -m "$(cat <<'EOF'
Scaffold farm-worker crate with env-driven WorkerConfig

New workspace member: the language-agnostic worker that will consume
lay_report events off merkql and write hen_productivity via REST/
GraphQL. Config is env-var driven (SOURCE_GRAPHQL_URL, TARGET_REST_URL,
TARGET_GRAPHQL_URL, MERKQL_DIR, QUERY_DIALECT, WORKER_*) so one
compiled binary can point at a Rust, Java, or TS farm deployment
purely via config — QUERY_DIALECT is what lets it also match whichever
GraphQL query-naming convention that deployment's language landed on.
EOF
)"
```

---

## Task 5: Thin event decoding + source detail lookup (`fetch_lay_report`)

**Files:**
- Create: `meshql-rs/examples/farm-worker/src/event.rs`
- Create: `meshql-rs/examples/farm-worker/src/graphql.rs`
- Create: `meshql-rs/examples/farm-worker/src/detail.rs`
- Modify: `meshql-rs/examples/farm-worker/src/lib.rs`
- Test: `meshql-rs/examples/farm-worker/src/detail.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/src/event.rs`:

```rust
//! The wire shape `meshql_changes::merkql_sink::publish_to_merkql` writes
//! onto each entity's merkql topic. Deliberately NOT a dependency on
//! `meshql-changes` itself — the worker only needs to agree on the wire
//! contract (the same discipline an SSE client follows against
//! `ChangeEvent::wire_json()`'s shape, never the producer's internals).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ThinEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
}
```

Create `meshql-rs/examples/farm-worker/src/graphql.rs`:

```rust
//! Minimal GraphQL-over-HTTP client. No GraphQL client library — this
//! workspace's convention (see meshql-restlette's ProjectionUpdater-style
//! callers and the Java reference `ProjectionUpdater`) is plain
//! `{"query": "..."}"` POSTs, parsed by hand.

use anyhow::{anyhow, Context};
use serde_json::Value;

/// POST a GraphQL query and return its `data` object. Errors on a non-2xx
/// response or a non-empty `errors` array — this worker never treats a
/// partial/error GraphQL response as usable detail.
pub async fn graphql_query(
    client: &reqwest::Client,
    url: &str,
    query: &str,
    auth: Option<(&str, &str)>,
) -> anyhow::Result<Value> {
    let mut req = client.post(url).json(&serde_json::json!({ "query": query }));
    if let Some((name, value)) = auth {
        req = req.header(name, value);
    }
    let resp = req.send().await.context("GraphQL request failed")?;
    let status = resp.status();
    let body: Value = resp.json().await.context("GraphQL response was not JSON")?;
    if !status.is_success() {
        return Err(anyhow!("GraphQL request to {url} failed: {status} {body}"));
    }
    if let Some(errors) = body.get("errors") {
        return Err(anyhow!("GraphQL errors from {url}: {errors}"));
    }
    body.get("data")
        .cloned()
        .ok_or_else(|| anyhow!("GraphQL response from {url} had no 'data': {body}"))
}
```

Create `meshql-rs/examples/farm-worker/src/detail.rs`:

```rust
//! Detail lookup: given a thin ChangeEvent's id, fetch the full lay_report
//! record via GraphQL — "the same 'thin notification -> query for detail ->
//! react' shape an SSE-consuming FE client is designed for" per the pipeline
//! spec.

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    async fn echo_getlayreport(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getLayReport"), "unexpected query: {query}");
        assert!(query.contains(r#""lr-1""#), "query missing expected id: {query}");
        assert!(query.contains("at: 1000"), "query missing expected at: {query}");
        Json(json!({
            "data": {
                "getLayReport": {
                    "henId": "hen-1",
                    "eggs": 3,
                    "timeOfDay": "morning"
                }
            }
        }))
    }

    async fn echo_getbyid(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getById"), "unexpected query: {query}");
        Json(json!({
            "data": {
                "getById": {
                    "henId": "hen-1",
                    "eggs": 3,
                    "timeOfDay": "morning"
                }
            }
        }))
    }

    async fn echo_null() -> Json<Value> {
        Json(json!({ "data": { "getLayReport": null } }))
    }

    async fn start(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetch_lay_report_parses_a_successful_response_entity_named() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_getlayreport))).await;
        let client = reqwest::Client::new();

        let report = fetch_lay_report(&client, &base, "lr-1", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap();
        assert_eq!(report.hen_id, "hen-1");
        assert_eq!(report.eggs, 3);
        assert_eq!(report.time_of_day, "morning");
    }

    #[tokio::test]
    async fn fetch_lay_report_parses_a_successful_response_generic() {
        // Proves the SAME function, given QueryDialect::Generic, talks the
        // dialect Java's and TS's farm retrofits actually landed
        // (getById(id, at)) instead of Rust's getLayReport — see the
        // reconciliation note at the top of this plan.
        let base = start(Router::new().route("/lay_report/graph", post(echo_getbyid))).await;
        let client = reqwest::Client::new();

        let report = fetch_lay_report(&client, &base, "lr-1", 1000, QueryDialect::Generic)
            .await
            .unwrap();
        assert_eq!(report.hen_id, "hen-1");
        assert_eq!(report.eggs, 3);
    }

    #[tokio::test]
    async fn fetch_lay_report_errors_on_null_result() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_null))).await;
        let client = reqwest::Client::new();

        let err = fetch_lay_report(&client, &base, "missing-id", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing-id"));
    }

    async fn echo_getlayreportsbyhen(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getLayReportsByHen"), "unexpected query: {query}");
        assert!(query.contains(r#""hen-1""#), "query missing expected hen id: {query}");
        Json(json!({
            "data": {
                "getLayReportsByHen": [
                    { "eggs": 3 },
                    { "eggs": 2 }
                ]
            }
        }))
    }

    async fn echo_getbyhen_empty(Json(_body): Json<Value>) -> Json<Value> {
        Json(json!({ "data": { "getByHen": [] } }))
    }

    #[tokio::test]
    async fn fetch_lay_reports_for_hen_sums_every_report_currently_on_record() {
        let base =
            start(Router::new().route("/lay_report/graph", post(echo_getlayreportsbyhen))).await;
        let client = reqwest::Client::new();

        let eggs = fetch_lay_reports_for_hen(&client, &base, "hen-1", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap();
        assert_eq!(eggs, vec![3, 2]);
    }

    #[tokio::test]
    async fn fetch_lay_reports_for_hen_returns_empty_for_a_hen_with_no_reports() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_getbyhen_empty))).await;
        let client = reqwest::Client::new();

        let eggs = fetch_lay_reports_for_hen(&client, &base, "hen-1", 1000, QueryDialect::Generic)
            .await
            .unwrap();
        assert_eq!(eggs, Vec::<i64>::new());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p farm-worker --lib detail
```

Expected: compile errors — `fetch_lay_report` and `LayReport` are not defined; `event`/`graphql`/`detail` modules aren't registered in `lib.rs` yet either.

- [ ] **Step 3: Write the minimal implementation**

Add to `meshql-rs/examples/farm-worker/src/detail.rs`, above the `#[cfg(test)]` block:

```rust
use crate::config::QueryDialect;
use crate::graphql::graphql_query;
use anyhow::{anyhow, Context};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LayReport {
    #[serde(rename = "henId")]
    pub hen_id: String,
    pub eggs: i64,
    #[serde(rename = "timeOfDay")]
    pub time_of_day: String,
}

/// Fetch the full lay_report detail for `id`, as of `at` (epoch millis — the
/// ChangeEvent's commit time), from the source farm's lay_report graphlette.
/// The field shape (`henId`/`eggs`/`timeOfDay`) is identical across all
/// three retrofits, but the QUERY NAME is not — `dialect` picks between
/// Rust's `getLayReport` and Java/TS's `getById`. See the reconciliation
/// note at the top of this plan.
pub async fn fetch_lay_report(
    client: &reqwest::Client,
    graphql_base: &str,
    id: &str,
    at: i64,
    dialect: QueryDialect,
) -> anyhow::Result<LayReport> {
    let url = format!("{}/lay_report/graph", graphql_base.trim_end_matches('/'));
    let query_name = dialect.lay_report_by_id();
    let query =
        format!(r#"{{ {query_name}(id: "{id}", at: {at}) {{ henId eggs timeOfDay }} }}"#);
    let data = graphql_query(client, &url, &query, None).await?;
    let report = data
        .get(query_name)
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            anyhow!(
                "{query_name}({id}) at {at} returned null — detail not yet visible or id unknown"
            )
        })?;
    serde_json::from_value(report.clone())
        .context("lay_report detail response did not match the assumed LayReport shape")
}

/// Fetch every eggs count currently on record for `hen_id`, as of `at` —
/// the input to `productivity::recompute`'s `report_eggs` parameter. Fresh
/// on every call, never cached: this is what makes the worker's fold
/// idempotent under redelivery (see the reconciliation note at the top of
/// this plan and `productivity::recompute`'s doc comment) instead of
/// relying on a stored dedup ledger. Only `eggs` is deserialized — the
/// hen's identity and the rest of each report's fields aren't needed here.
pub async fn fetch_lay_reports_for_hen(
    client: &reqwest::Client,
    graphql_base: &str,
    hen_id: &str,
    at: i64,
    dialect: QueryDialect,
) -> anyhow::Result<Vec<i64>> {
    #[derive(Deserialize)]
    struct EggsOnly {
        eggs: i64,
    }

    let url = format!("{}/lay_report/graph", graphql_base.trim_end_matches('/'));
    let query_name = dialect.lay_reports_by_hen();
    let query = format!(r#"{{ {query_name}(id: "{hen_id}", at: {at}) {{ eggs }} }}"#);
    let data = graphql_query(client, &url, &query, None).await?;
    let list = data
        .get(query_name)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    list.into_iter()
        .map(|v| {
            serde_json::from_value::<EggsOnly>(v)
                .map(|r| r.eggs)
                .context("lay_report list response did not match the assumed {eggs} shape")
        })
        .collect()
}
```

Edit `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
pub mod detail;
pub mod event;
pub mod graphql;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p farm-worker --lib detail
```

Expected: all five tests in `detail::tests` pass — `fetch_lay_report_parses_a_successful_response_entity_named`, `fetch_lay_report_parses_a_successful_response_generic`, `fetch_lay_report_errors_on_null_result`, `fetch_lay_reports_for_hen_sums_every_report_currently_on_record`, `fetch_lay_reports_for_hen_returns_empty_for_a_hen_with_no_reports`.

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/src/event.rs examples/farm-worker/src/graphql.rs examples/farm-worker/src/detail.rs examples/farm-worker/src/lib.rs
git commit -m "$(cat <<'EOF'
farm-worker: decode thin events, add GraphQL detail lookups

ThinEvent mirrors meshql-changes' wire_json shape without depending on
the crate itself. fetch_lay_report is the "thin notification -> query
for detail" half of the worker (single report, by id); 
fetch_lay_reports_for_hen fetches a hen's FULL current report set
(eggs only), feeding productivity::recompute's idempotent-by-recompute
design. Both are dialect-aware (QueryDialect::EntityNamed vs Generic)
since Rust's and Java/TS's farm retrofits landed different GraphQL
query names for the same queries.
EOF
)"
```

---

## Task 6: `HenProductivity` model + idempotent fold

**Files:**
- Create: `meshql-rs/examples/farm-worker/src/productivity.rs`
- Modify: `meshql-rs/examples/farm-worker/src/lib.rs`

This is pure logic — no I/O, fast unit tests, no test server needed.

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/src/productivity.rs`:

```rust
//! `hen_productivity` model + the pure recompute that derives it from a
//! hen's current lay_report set. Exact landed shape on all three
//! languages: `{id, henId, totalEggs, lastLaidAt}` — confirmed by reading
//! all three retrofits' `hen_productivity.schema.json`/`.graphql` directly.
//! No dedup-ledger field: see the reconciliation note at the top of this
//! plan for why a full recompute replaced the originally-planned
//! accumulate-plus-processedReportIds design.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_report_on_a_hen_creates_fresh_state() {
        let next = recompute(None, "hen-1", &[3], "2026-07-22T08:00:00Z");
        assert_eq!(next.hen_id, "hen-1");
        assert_eq!(next.total_eggs, 3);
        assert_eq!(next.last_laid_at, "2026-07-22T08:00:00Z");
        assert_eq!(next.id, None);
    }

    #[test]
    fn second_distinct_report_recomputes_the_full_total_and_preserves_known_id() {
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        // Both reports' eggs, fetched fresh from the source — this is what
        // makes the recompute safe under redelivery: it never depends on
        // what was summed last time, only on what's true right now.
        let next = recompute(Some(&current), "hen-1", &[3, 2], "2026-07-23T08:00:00Z");
        assert_eq!(next.id, Some("hp-99".to_string()), "known id must be preserved");
        assert_eq!(next.total_eggs, 5);
        assert_eq!(next.last_laid_at, "2026-07-23T08:00:00Z");
    }

    #[test]
    fn redelivering_the_same_event_over_unchanged_data_is_a_true_no_op() {
        // Proves the idempotency requirement domain-design.md demands for
        // at-least-once delivery: recomputing over the SAME report set with
        // the SAME (already-applied) event timestamp must reproduce
        // identical state, not double-count anything — no ledger needed.
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        let next = recompute(Some(&current), "hen-1", &[3], "2026-07-22T08:00:00Z");
        assert_eq!(next, current, "redelivery over unchanged source data must be a pure no-op");
    }

    #[test]
    fn last_laid_at_never_regresses_when_an_older_event_is_redelivered() {
        let current = HenProductivity {
            id: Some("hp-99".to_string()),
            hen_id: "hen-1".to_string(),
            total_eggs: 5,
            last_laid_at: "2026-07-23T08:00:00Z".to_string(),
        };
        // An older event (e.g. the FIRST report, redelivered after the
        // second has already landed) must not move last_laid_at backwards.
        let next = recompute(Some(&current), "hen-1", &[3, 2], "2026-07-22T08:00:00Z");
        assert_eq!(next.last_laid_at, "2026-07-23T08:00:00Z");
        assert_eq!(next.total_eggs, 5);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p farm-worker --lib productivity
```

Expected: compile error — `HenProductivity` and `fold` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `productivity.rs`, above the `#[cfg(test)]` block:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HenProductivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "henId")]
    pub hen_id: String,
    #[serde(rename = "totalEggs")]
    pub total_eggs: i64,
    #[serde(rename = "lastLaidAt")]
    pub last_laid_at: String,
}

/// Pure recompute: derive the next hen_productivity state from `current`
/// (for its known id and last_laid_at baseline — `None` means this hen has
/// no productivity record yet) and `report_eggs`, the hen's FULL, freshly
/// fetched set of lay_report egg counts (never an incremental delta).
///
/// Idempotent by construction, which is what makes this safe under
/// at-least-once merkql delivery (domain-design.md: "A worker fold that
/// isn't deterministic/idempotent ... breaks ... at-least-once delivery"):
/// `total_eggs` is a sum over the CURRENT report set, so redelivering any
/// event recomputes the same total as long as the underlying lay_report
/// data hasn't changed — no dedup ledger needed. `last_laid_at` is
/// `max(current.last_laid_at, event_created_at_iso)`, a monotonic merge
/// that's idempotent for the same reason (never regresses, reapplying the
/// same input changes nothing). Both `event_created_at_iso` and any stored
/// `last_laid_at` must be fixed-offset ISO-8601 (`...Z`) for the string
/// comparison to agree with chronological order — the worker only ever
/// produces this format (see Task 8), so this holds by construction.
pub fn recompute(
    current: Option<&HenProductivity>,
    hen_id: &str,
    report_eggs: &[i64],
    event_created_at_iso: &str,
) -> HenProductivity {
    let total_eggs: i64 = report_eggs.iter().sum();
    let last_laid_at = match current {
        Some(c) if c.last_laid_at.as_str() >= event_created_at_iso => c.last_laid_at.clone(),
        _ => event_created_at_iso.to_string(),
    };
    HenProductivity {
        id: current.and_then(|c| c.id.clone()),
        hen_id: hen_id.to_string(),
        total_eggs,
        last_laid_at,
    }
}
```

Edit `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
pub mod detail;
pub mod event;
pub mod graphql;
pub mod productivity;
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p farm-worker --lib productivity
```

Expected: all three tests pass.

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/src/productivity.rs examples/farm-worker/src/lib.rs
git commit -m "$(cat <<'EOF'
farm-worker: add HenProductivity model + idempotent recompute

Pure, I/O-free recompute over a hen's full current lay_report set —
idempotent by construction under at-least-once redelivery, no dedup
ledger needed. Matches the exact {henId, totalEggs, lastLaidAt} shape
all three farm retrofits landed, with zero schema reconciliation debt.
EOF
)"
```

---

## Task 7: REST/GraphQL upsert client (`get_current` + `write`)

**Files:**
- Create: `meshql-rs/examples/farm-worker/src/rest_client.rs`
- Modify: `meshql-rs/examples/farm-worker/src/lib.rs`

This is a direct Rust port of the Java reference `ProjectionUpdater`'s `getProjection`/`createProjection`/`updateProjection` — read that file before starting if you haven't already (`/tank/repos/tailoredshapes/meshql/examples/egg-economy/src/main/java/com/meshql/examples/egg_economy/ProjectionUpdater.java`).

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/src/rest_client.rs`:

```rust
//! Read-modify-write against `hen_productivity`, entirely over REST/GraphQL
//! — never a direct database call, per the "single writer" invariant (the
//! worker is just another authorized REST caller). Mirrors the Java
//! `ProjectionUpdater` reference pattern: GraphQL exposes ids, REST doesn't,
//! so a fresh create is discovered afterward via the same GraphQL query used
//! to read current state.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkerConfig;
    use axum::extract::State;
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeStore(Arc<Mutex<Option<HenProductivity>>>);

    async fn graph_handler(
        State(store): State<FakeStore>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getHenProductivityByHen"), "unexpected query: {query}");
        let current = store.0.lock().unwrap().clone();
        let list = match current {
            Some(hp) => vec![serde_json::to_value(&hp).unwrap()],
            None => vec![],
        };
        Json(json!({ "data": { "getHenProductivityByHen": list } }))
    }

    async fn graph_handler_generic(
        State(store): State<FakeStore>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getByHen"), "unexpected query: {query}");
        let current = store.0.lock().unwrap().clone();
        let list = match current {
            Some(hp) => vec![serde_json::to_value(&hp).unwrap()],
            None => vec![],
        };
        Json(json!({ "data": { "getByHen": list } }))
    }

    async fn post_handler(State(store): State<FakeStore>, Json(body): Json<Value>) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some("hp-generated".to_string());
        *store.0.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn put_handler(
        axum::extract::Path(id): axum::extract::Path<String>,
        State(store): State<FakeStore>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some(id);
        *store.0.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn start() -> (String, FakeStore) {
        let store = FakeStore::default();
        let router = Router::new()
            .route("/hen_productivity/graph", post(graph_handler))
            .route("/hen_productivity/api", post(post_handler))
            .route("/hen_productivity/api/:id", put(put_handler))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}"), store)
    }

    async fn start_generic() -> (String, FakeStore) {
        let store = FakeStore::default();
        let router = Router::new()
            .route("/hen_productivity/graph", post(graph_handler_generic))
            .route("/hen_productivity/api", post(post_handler))
            .route("/hen_productivity/api/:id", put(put_handler))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}"), store)
    }

    fn cfg(base: &str) -> WorkerConfig {
        cfg_dialect(base, QueryDialect::EntityNamed)
    }

    fn cfg_dialect(base: &str, dialect: QueryDialect) -> WorkerConfig {
        let base = base.to_string();
        let mut c = WorkerConfig::from_lookup(move |k| match k {
            "SOURCE_GRAPHQL_URL" | "TARGET_REST_URL" | "TARGET_GRAPHQL_URL" => {
                Some(base.to_string())
            }
            _ => None,
        });
        c.query_dialect = dialect;
        c
    }

    #[tokio::test]
    async fn get_current_returns_none_when_the_hen_has_no_record_yet() {
        let (base, _store) = start().await;
        let client = reqwest::Client::new();
        let result = get_current(&client, &cfg(&base), "hen-1").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn get_current_uses_the_generic_dialect_query_name_when_configured() {
        // Java's and TS's farm retrofits expose getByHen, not
        // getHenProductivityByHen — see the reconciliation note at the top
        // of this plan.
        let (base, _store) = start_generic().await;
        let client = reqwest::Client::new();
        let c = cfg_dialect(&base, QueryDialect::Generic);
        let result = get_current(&client, &c, "hen-1").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn write_posts_when_the_hen_has_no_known_id_then_a_later_write_puts() {
        let (base, store) = start().await;
        let client = reqwest::Client::new();
        let c = cfg(&base);

        let first = HenProductivity {
            id: None,
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        write(&client, &c, &first).await.unwrap();
        let stored = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(stored.id, Some("hp-generated".to_string()));
        assert_eq!(stored.total_eggs, 3);

        // Discover the id the way the worker's own loop would, then PUT.
        let discovered = get_current(&client, &c, "hen-1").await.unwrap().unwrap();
        let second = HenProductivity {
            total_eggs: 5,
            ..discovered
        };
        write(&client, &c, &second).await.unwrap();
        let stored = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(stored.id, Some("hp-generated".to_string()), "PUT must keep the same id");
        assert_eq!(stored.total_eggs, 5);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p farm-worker --lib rest_client
```

Expected: compile errors — `get_current`/`write` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `rest_client.rs`, above the `#[cfg(test)]` block:

```rust
use crate::config::WorkerConfig;
use crate::graphql::graphql_query;
use crate::productivity::HenProductivity;
use anyhow::{anyhow, Context};

fn auth_header(cfg: &WorkerConfig) -> Option<(&str, &str)> {
    cfg.auth_header
        .as_deref()
        .map(|h| (h, cfg.auth_value.as_str()))
}

/// GET the current hen_productivity for `hen_id` via GraphQL, discovering
/// its MeshQL id in the same call (GraphQL exposes ids; REST deliberately
/// doesn't — see meshql-patterns' REST ID model). `None` means this is the
/// hen's first lay_report. Query name is dialect-aware — Rust's farm
/// exposes `getHenProductivityByHen`, Java's and TS's both expose
/// `getByHen` — see the reconciliation note at the top of this plan. Its
/// `RootConfig` query template filters on `"payload.henId"` on every
/// landed backend (Mongo, SQLite both require the `payload.` prefix — see
/// "Facts to respect" at the top of this plan), which is an implementation
/// detail of the target deployment's config, invisible from here.
pub async fn get_current(
    client: &reqwest::Client,
    cfg: &WorkerConfig,
    hen_id: &str,
) -> anyhow::Result<Option<HenProductivity>> {
    let url = format!(
        "{}/hen_productivity/graph",
        cfg.target_graphql_base.trim_end_matches('/')
    );
    let now_ms = chrono::Utc::now().timestamp_millis();
    let query_name = cfg.query_dialect.hen_productivity_by_hen();
    let query = format!(
        r#"{{ {query_name}(id: "{hen_id}", at: {now_ms}) {{ id henId totalEggs lastLaidAt }} }}"#
    );
    let data = graphql_query(client, &url, &query, auth_header(cfg)).await?;
    let list = data
        .get(query_name)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match list.into_iter().next() {
        None => Ok(None),
        Some(v) => Ok(Some(serde_json::from_value(v).context(
            "hen_productivity-by-hen result did not match the assumed HenProductivity shape",
        )?)),
    }
}

/// Write `next` via REST: `PUT` to its known id (an update-as-new-version)
/// if one exists, or `POST` if this is the hen's first productivity record.
/// A fresh `POST` does not need to re-discover its id here — the caller's
/// next `get_current` call (the next time an event for this hen is
/// processed) will find it, and within a single batch `write` always
/// `await`s the REST call to completion before returning, so a same-batch
/// second event for the same hen sees the first write's result.
pub async fn write(
    client: &reqwest::Client,
    cfg: &WorkerConfig,
    next: &HenProductivity,
) -> anyhow::Result<()> {
    let rest_base = format!(
        "{}/hen_productivity/api",
        cfg.target_rest_base.trim_end_matches('/')
    );
    let body = serde_json::to_value(next)?;

    if let Some(id) = &next.id {
        let url = format!("{rest_base}/{id}");
        let mut req = client.put(&url).json(&body);
        if let Some((name, value)) = auth_header(cfg) {
            req = req.header(name, value);
        }
        let resp = req.send().await.context("PUT hen_productivity failed")?;
        if !resp.status().is_success() {
            return Err(anyhow!("PUT {url} failed: {}", resp.status()));
        }
        return Ok(());
    }

    let mut req = client.post(&rest_base).json(&body);
    if let Some((name, value)) = auth_header(cfg) {
        req = req.header(name, value);
    }
    let resp = req.send().await.context("POST hen_productivity failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("POST {rest_base} failed: {}", resp.status()));
    }
    Ok(())
}
```

Edit `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
pub mod detail;
pub mod event;
pub mod graphql;
pub mod productivity;
pub mod rest_client;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p farm-worker --lib rest_client
```

Expected: all three tests pass (`get_current_returns_none_when_the_hen_has_no_record_yet`, `get_current_uses_the_generic_dialect_query_name_when_configured`, `write_posts_when_the_hen_has_no_known_id_then_a_later_write_puts`).

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/src/rest_client.rs examples/farm-worker/src/lib.rs
git commit -m "$(cat <<'EOF'
farm-worker: add REST/GraphQL upsert client for hen_productivity

get_current (dialect-aware GraphQL read + id discovery) + write (REST
POST for a fresh record, PUT /:id thereafter) — a direct Rust port of
the Java ProjectionUpdater reference pattern. Enforces the
single-writer invariant: hen_productivity is only ever touched over
the network, exactly like any other REST caller.
EOF
)"
```

---

## Task 8: Consumer loop — `process_batch` + `run_forever`

**Files:**
- Create: `meshql-rs/examples/farm-worker/src/worker.rs`
- Modify: `meshql-rs/examples/farm-worker/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/src/worker.rs`:

```rust
//! The consumer loop: poll the lay_report merkql topic, fold, write,
//! commit — with the offset-commit discipline the pipeline spec's
//! backpressure guidance asks for. See the module-level comment on
//! `process_batch` for the merkql `Consumer::poll` gotcha this is built
//! around.

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
            .send(&ProducerRecord::new("lay_report", Some(id.to_string()), value))
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

    async fn lay_report_graph(State(farm): State<FakeFarm>, Json(body): Json<Value>) -> Json<Value> {
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
        assert_eq!(n2, 0, "committed offset must not be replayed by a fresh consumer");
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
        assert_eq!(records.len(), 1, "the un-committed event must still be there to retry");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p farm-worker --lib worker
```

Expected: compile errors — `process_batch` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `worker.rs`, above the `#[cfg(test)]` block:

```rust
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
fn to_iso8601(created_at_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(created_at_ms)
        .expect("ChangeEvent::created_at is always a valid epoch-millis timestamp")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
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
        let event_created_at_iso = to_iso8601(thin.created_at);
        let next = recompute(current.as_ref(), &report.hen_id, &report_eggs, &event_created_at_iso);
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
```

Edit `meshql-rs/examples/farm-worker/src/lib.rs`:

```rust
pub mod config;
pub mod detail;
pub mod event;
pub mod graphql;
pub mod productivity;
pub mod rest_client;
pub mod worker;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p farm-worker --lib worker
cargo test -p farm-worker --lib   # confirm every earlier unit test still passes
```

Expected: `process_batch_folds_and_writes_then_commits_the_offset` and `process_batch_does_not_commit_when_detail_lookup_fails` both pass; the full `--lib` suite (config, detail, productivity, rest_client, worker) is green.

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/src/worker.rs examples/farm-worker/src/lib.rs
git commit -m "$(cat <<'EOF'
farm-worker: add the consumer loop (process_batch + run_forever)

Whole-batch commit discipline: commit_sync() only fires after every
record in a poll's batch recomputes and writes successfully, and a
failed batch is retried by a FRESH Consumer next tick — never by
re-polling the one that failed, since merkql::Consumer::poll advances
its in-memory position before the caller processes anything. This is
the backpressure behavior the pipeline spec asks for. Whole-batch
redelivery is safe here because productivity::recompute is idempotent
by construction, not because of any per-record dedup bookkeeping.
EOF
)"
```

---

## Task 9: Binary wiring (`main.rs`)

**Files:**
- Modify: `meshql-rs/examples/farm-worker/src/main.rs`

- [ ] **Step 1: N/A — this task wires existing, already-tested modules into a binary entry point; there is no new unit to TDD.** Verification is Step 4 (build) and Task 10 (behavioral, end-to-end).

- [ ] **Step 2: N/A** (see Step 1)

- [ ] **Step 3: Replace the placeholder `main.rs`**

Replace the contents of `meshql-rs/examples/farm-worker/src/main.rs`:

```rust
//! farm-worker — the shared, language-agnostic worker half of the merkql
//! CDC pipeline. Consumes lay_report events off a merkql topic (written by
//! meshql-changes' merkql sink, wired into examples/farm), looks up full
//! event detail via GraphQL, folds into hen_productivity, and writes the
//! result back via REST/GraphQL. Every endpoint this binary talks to is
//! config (env vars) — the SAME compiled binary points at a Rust, Java, or
//! TS farm deployment with no rebuild.
//!
//! Config: docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md
//! Env vars: see farm_worker::config::WorkerConfig::from_lookup.

use farm_worker::config::WorkerConfig;
use farm_worker::worker::run_forever;
use merkql::broker::{Broker, BrokerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = WorkerConfig::from_env();
    println!(
        "[farm-worker] topic={} group={} source_graphql={} target_rest={} target_graphql={} merkql_dir={}",
        cfg.topic,
        cfg.group_id,
        cfg.source_graphql_base,
        cfg.target_rest_base,
        cfg.target_graphql_base,
        cfg.merkql_dir.display(),
    );

    let broker = Broker::open(BrokerConfig::new(&cfg.merkql_dir))?;
    let client = reqwest::Client::new();
    run_forever(broker, client, cfg).await;
    Ok(())
}
```

- [ ] **Step 4: Verify it builds and runs against a freshly-created (empty) merkql dir**

```bash
cargo build -p farm-worker
MERKQL_DIR=/tmp/farm-worker-smoke-test WORKER_POLL_INTERVAL_MS=200 timeout 2 cargo run -p farm-worker
```

Expected: prints the `[farm-worker] topic=... ` startup line, then loops silently (no lay_report topic exists yet, so `process_batch` returns `Ok(0)` every tick — no crash, no panic) until `timeout` kills it after 2 seconds. Clean up: `rm -rf /tmp/farm-worker-smoke-test`.

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/src/main.rs
git commit -m "$(cat <<'EOF'
farm-worker: wire the binary entry point

main() reads WorkerConfig::from_env(), opens the merkql broker at
MERKQL_DIR, and runs the consumer loop forever. One binary, pointed
at any farm deployment purely via env vars.
EOF
)"
```

---

## Task 10: End-to-end pipeline test

**Files:**
- Create: `meshql-rs/examples/farm-worker/tests/pipeline.rs`

Runs the FULL chain in-process: `POST /lay_report/api` (real REST) → `SearcherTail` (real, polling a real in-memory sqlite mesh) → `run_merkql_sink` (real, Component 1) → merkql topic (real, tempdir-backed broker) → `farm_worker::worker::process_batch` (real, Component 2) → `GET /hen_productivity/graph` (real GraphQL, proving the write landed). No mocks anywhere except the sqlite backend standing in for farm's production Mongo — `SearcherTail` is certified against any `Searcher`+`Repository` pair (invariant 5), so this is a faithful substitution, not a shortcut, and matches `examples/egg-economy/tests/pipeline.rs`'s own precedent of running its full pipeline against sqlite with no Mongo/Kafka.

- [ ] **Step 1: Write the failing test**

Create `meshql-rs/examples/farm-worker/tests/pipeline.rs`:

```rust
//! End-to-end proof of the full pipeline described in
//! docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md:
//!
//!   POST /lay_report/api (REST)
//!     -> SearcherTail (storage-layer CDC, no restlette hook)
//!       -> run_merkql_sink (Component 1: the connector)
//!         -> merkql topic "lay_report"
//!           -> farm_worker::worker::process_batch (Component 2: the worker)
//!             -> GET /lay_report/graph (detail lookup)
//!             -> GET /hen_productivity/graph (read current)
//!             -> POST or PUT /hen_productivity/api (write)
//!               -> GET /hen_productivity/graph confirms the result
//!
//! Also proves idempotency: redelivering the same lay_report id onto the
//! merkql topic must not double-count its eggs.

use farm_worker::config::WorkerConfig;
use farm_worker::worker::process_batch;
use merkql::broker::{Broker, BrokerConfig, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use meshql_changes::{publish_to_merkql, run_tails, ChangeEvent, ChangeHub, ChangeSource, SearcherTail};
use meshql_core::{
    GraphletteConfig, Repository, RestletteConfig, RootConfig, Searcher, ServerConfig,
};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
async fn start_farm() -> (String, Arc<SqliteRepository>, Arc<SqliteRepository>, Arc<dyn Searcher>) {
    let lay_pool = sqlite_pool().await;
    let lay_repo = Arc::new(SqliteRepository::new_with_pool(lay_pool.clone()).await.unwrap());
    let lay_searcher: Arc<dyn Searcher> =
        Arc::new(SqliteSearcher::new_with_pool(lay_pool).await.unwrap());

    let hp_pool = sqlite_pool().await;
    let hp_repo = Arc::new(SqliteRepository::new_with_pool(hp_pool.clone()).await.unwrap());
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

/// Drives one full tick of Component 1: poll the lay_report tail once, mirror
/// whatever it finds onto merkql. Calling this directly (rather than
/// spawning run_tails/run_merkql_sink and sleeping) keeps the test
/// deterministic instead of racing a background poll interval.
async fn tick_connector(tail: &SearcherTail, broker: &BrokerRef) {
    let events = tail.poll().await.unwrap();
    for ev in events {
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
    let tail = SearcherTail::new("lay_report", lay_searcher, lay_repo.clone() as Arc<dyn Repository>);
    let cfg = worker_cfg(&base);

    // timeOfDay is written but deliberately never asserted on below — two
    // of the three landed farm retrofits treat it as a morning/afternoon/
    // evening enum, not a timestamp (see the reconciliation note at the
    // top of this plan), so lastLaidAt is sourced from the ChangeEvent's
    // own created_at instead, not from this field.

    // --- First report ---
    post_lay_report(&base, "hen-1", 3, "morning").await;
    tick_connector(&tail, &broker).await;
    let n = tick_worker(&broker, &cfg).await;
    assert_eq!(n, 1);

    let hp = read_hen_productivity(&base, "hen-1").await.expect("hen_productivity created");
    assert_eq!(hp["totalEggs"], json!(3));
    let first_laid_at = hp["lastLaidAt"].as_str().unwrap().to_string();
    assert!(!first_laid_at.is_empty());
    let hp_id = hp["id"].as_str().unwrap().to_string();

    // --- Second report, same hen: must accumulate, must keep the same id ---
    post_lay_report(&base, "hen-1", 2, "evening").await;
    tick_connector(&tail, &broker).await;
    let n = tick_worker(&broker, &cfg).await;
    assert_eq!(n, 1);

    let hp = read_hen_productivity(&base, "hen-1").await.unwrap();
    assert_eq!(hp["totalEggs"], json!(5));
    let second_laid_at = hp["lastLaidAt"].as_str().unwrap().to_string();
    assert!(
        second_laid_at >= first_laid_at,
        "lastLaidAt must advance forward as later reports land"
    );
    assert_eq!(hp["id"], json!(hp_id), "PUT must version the SAME record, not create a new one");

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
    assert_eq!(n, 1, "the redelivered event is still processed (a no-op recompute)");

    let hp = read_hen_productivity(&base, "hen-1").await.unwrap();
    assert_eq!(hp["totalEggs"], json!(5), "redelivery must NOT double-count eggs");
    assert_eq!(
        hp["lastLaidAt"], json!(second_laid_at),
        "redelivering an OLDER event must NOT regress lastLaidAt"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p farm-worker --test pipeline
```

Expected: fails to compile initially if any helper signature drifted from Tasks 1-9 (e.g. `meshql_server::build_app` visibility — confirm it's `pub async fn build_app` in `meshql-server/src/lib.rs`, verified during planning). Fix any mismatches against the real signatures (do not guess); once it compiles, it should PASS on the first run if Tasks 1-9 were implemented correctly — this task is integration proof, not new production code. If it fails at runtime instead of compile time, that is a real bug in one of Tasks 1-9; use `superpowers:systematic-debugging` rather than patching the test to hide it.

- [ ] **Step 3: N/A** — there is no new implementation to write for this task; it exercises Tasks 1-9's code as-is. If Step 2 revealed a genuine bug, fix it in the relevant Task's file (not here), re-run that task's own unit tests, then return to this test.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p farm-worker --test pipeline -- --nocapture
```

Expected: `test full_pipeline_accumulates_across_reports_and_is_idempotent_under_redelivery ... ok`.

- [ ] **Step 5: Commit**

```bash
git add examples/farm-worker/tests/pipeline.rs
git commit -m "$(cat <<'EOF'
farm-worker: add end-to-end pipeline test

POST lay_report -> SearcherTail -> run_merkql_sink -> merkql topic ->
worker::process_batch -> hen_productivity, all in-process against
sqlite (no Mongo/Kafka needed, matching examples/egg-economy's own
pipeline test precedent). Proves accumulation across two reports for
the same hen AND idempotency under redelivery of an already-applied
event — via recompute-from-source, not a dedup ledger.
EOF
)"
```

---

## Task 11: Full-workspace verification

**Files:** none (verification only)

- [ ] **Step 1: N/A**
- [ ] **Step 2: N/A**
- [ ] **Step 3: N/A**

- [ ] **Step 4: Run the entire workspace test suite and confirm nothing outside this plan's scope broke**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: everything from before this plan started still passes (existing `meshql-changes`, `meshql-mongo`, `meshql-sqlite`, `examples/farm`, `examples/egg-economy` suites, etc.), plus every test added in Tasks 1, 2, 4, 5, 6, 7, 8, and 10. If `cargo test --workspace` picks up ignored/env-gated suites (e.g. `meshql-ksql`'s `CONFLUENT_KAFKA_REST_URL`-gated cert tests), that's pre-existing and out of scope — confirm they're *skipped*, not failing.

Per `superpowers:verification-before-completion`: do not report this plan complete without having actually run these two commands and read their output — a clean build and a passing `cargo test --workspace` are the evidence, not an assumption.

- [ ] **Step 5: Final commit (only if Step 4 required fixes; otherwise this task produces no commit of its own — the prior ten tasks' commits ARE the deliverable)**

If Step 4 required any fix-up commits, use `superpowers:finishing-a-development-branch` to decide how this work integrates (merge, PR, or further review) — do not merge to `main` unilaterally as part of this plan.

---

## Summary of what this plan intentionally leaves open (carried over from the spec, not resolved here)

- **Exact retry/backoff cadence** beyond "retry the whole batch next tick, forever" — the spec calls this "a direction, not a full spec." `WORKER_POLL_INTERVAL_MS` is the only tunable; no exponential backoff, no dead-letter topic, no alerting on a stuck batch. A hen with a permanently-unreachable lay_report detail (e.g. the record itself was somehow removed between the connector's read and the worker's query) will retry that one record forever without making progress on nothing else in the SAME batch, every tick, indefinitely, until a human intervenes. Flagged, not solved, matching the spec's own scoping.
- **The `run_merkql_sink` lag-loss gap** documented in Task 2 — sized generously (`ChangeHub::new(256)`, matching `examples/egg-economy`'s own choice) but not eliminated. A future hardening pass could drive the sink directly off `ChangeSource::poll` instead of the broadcast hub.
- **The worker's auth header** (`WORKER_AUTH_HEADER`/`WORKER_AUTH_TOKEN`) is a generic placeholder pending the retrofit's actual Casbin wiring — reconcile at execution time, per prerequisite (b) at the top of this plan.
- **`examples/egg-economy`'s existing pipeline is untouched** — this plan only ever modifies `meshql-changes` (additively) and adds `examples/farm-worker`; it does not touch `examples/egg-economy/src/worker.rs` or `source.rs`, which remain their own, separate (Kafka/Debezium-inspired, full-replay) implementation of the same pattern, per the spec's explicit out-of-scope note.

# Streamlette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `streamlette` — a per-meshlette SSE subscription surface alongside restlette and graphlette — so clients get real-time change delivery instead of polling.

**Architecture:** Mostly repackaging existing `meshql-changes` machinery from one deployment-level `/changes` feed into a per-entity surface declared on `ServerConfig`. Two sources feed a per-meshlette `ChangeHub`: `Tail` (existing `SearcherTail`, every backend, no resume) and `MerkqlTopic` (merkql log, supports `Last-Event-ID` resume and payload delivery). Ships in five independently-green steps.

**Tech Stack:** Rust, axum SSE (`axum::response::sse`), `tokio::sync::broadcast`, `merkql` consumer API, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-07-24-streamlette-design.md` — read it before starting. It went through two review rounds and records *why* several non-obvious constraints exist (single-partition, topic pre-creation, fresh `group_id`, unusable-cursor policy). Violating them produces silent failures that pass happy-path tests.

---

## Crate placement — decided here, not in the spec

**`MerkqlTopic` goes in `meshql-merkql`, not `meshql-changes`.** Verified: `meshql-changes/Cargo.toml` depends on `meshql-core` and `meshql-sqlite` — it has no `merkql` dependency and should stay backend-agnostic. `meshql-merkql` already has both `meshql-core` and `merkql`. Adding `meshql-changes` as a dependency of `meshql-merkql` introduces no cycle. This mirrors where `MerkqlRepository`/`MerkqlSearcher` already live.

**`ServerConfig` does not gain a `streamlettes` field**, contrary to the spec — it would be a dependency cycle. See Task 6, which resolves it and is the one deliberate deviation from the spec in this plan.

Note: `run_merkql_sink` referenced in the spec's prose **does not exist on `main`** — it appears only in `docs/superpowers/plans/2026-07-22-merkql-worker-pipeline.md` for an unmerged branch. Do not import it or assume it.

## File structure

| File | Responsibility | Step |
|---|---|---|
| `meshql-changes/src/sse.rs` | `change_stream` lag frame; `ready` frame helper (used by the streamlette handler, NOT by `change_stream`) | 1, 3 |
| `meshql-changes/src/event.rs` | `ChangeEvent`/`WireEvent` + `cursor`/`payload` | 2 |
| `meshql-changes/src/streamlette.rs` | **new** — `StreamletteConfig`, `StreamSource`, per-streamlette pump, handler, router | 3 |
| `meshql-core/src/config.rs` | `ServerConfig.streamlettes` + `Default` | 3 |
| `meshql-server/src/lib.rs` | mount streamlettes in `build_app*` | 3 |
| `meshql-merkql/src/stream.rs` | **new** — `MerkqlTopicSource` (`ChangeSource` impl), cursor, backfill, resume | 4 |
| `meshql-iron` skill docs × 5 repos | `references/streaming.md`, three `SKILL.md` edits | 5 |

---

# Step 1 — Lag frame on the existing `/changes`

Independently valuable: fixes a live bug in a shipped route.

### Task 1: `change_stream` announces lag instead of vanishing

**Files:**
- Modify: `meshql-changes/src/sse.rs` (`change_stream`, ~line 30-52; test `lagged_subscriber_stream_closes`, ~line 153)

- [ ] **Step 1: Update the existing test to demand the lagged frame**

Replace `lagged_subscriber_stream_closes` in `meshql-changes/src/sse.rs`. The stream must still *close* — that behavior is correct and existing consumers depend on it — but must say why first.

```rust
    #[tokio::test]
    async fn lagged_subscriber_gets_a_lagged_frame_then_close() {
        let hub = ChangeHub::new(2); // tiny buffer
        let rx = hub.subscribe();
        for i in 0..10 {
            hub.publish(ev("hen", &format!("e{i}"), &[]));
        }
        let stream = change_stream(rx, vec!["*".into()], None);
        tokio::pin!(stream);

        let mut frames = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(format!("{:?}", item.unwrap()));
            assert!(frames.len() < 12, "stream must end, not hang");
        }

        // The stream still closes (last frame is terminal)...
        let last = frames.last().expect("at least one frame");
        // ...but it announces the lag rather than vanishing silently.
        assert!(last.contains("lagged"), "final frame must be the lagged event, got: {last}");
        assert!(last.contains("skipped"), "lagged frame must carry the skipped count");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p meshql-changes --lib sse::tests::lagged_subscriber_gets_a_lagged_frame_then_close`
Expected: FAIL — the current `take_while` drops the `Lagged` item, so the final frame is a `change`, not `lagged`.

- [ ] **Step 3: Replace `take_while` with an announcing terminator**

In `meshql-changes/src/sse.rs`, replace the `.take_while(...)` line and the `filter_map` closure's `expect`:

```rust
pub fn change_stream(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    subscriber_tokens: Vec<String>,
    entities: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    // `scan` lets us emit one final frame for the lag and THEN end, which
    // `take_while` cannot do. `done` guards against a broadcast stream that
    // yields further items after a Lagged.
    let mut done = false;
    BroadcastStream::new(rx).scan((), move |_, item| {
        if done {
            return None;
        }
        match item {
            Err(BroadcastStreamRecvError::Lagged(skipped)) => {
                done = true;
                // Terminal frame: the client MUST resync (refetch), because
                // `skipped` events were dropped and are unrecoverable here.
                Some(Some(Ok(Event::default()
                    .event("lagged")
                    .data(format!(r#"{{"skipped":{skipped}}}"#)))))
            }
            Ok(ev) => {
                if let Some(wanted) = &entities {
                    if !wanted.contains(&ev.entity) {
                        return Some(None);
                    }
                }
                if !tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens) {
                    return Some(None);
                }
                Some(Some(Ok(Event::default()
                    .event("change")
                    .id(ev.created_at.to_string())
                    .data(ev.wire_json()))))
            }
        }
    })
    .filter_map(|x| x)
}
```

Note `scan` yields `Option<Option<_>>`: outer `None` ends the stream, inner `None` skips a filtered event. The trailing `.filter_map(|x| x)` flattens it.

- [ ] **Step 4: Run the whole crate's tests**

Run: `cargo test -p meshql-changes`
Expected: PASS, including the pre-existing `delivers_visible_events_and_filters_invisible` and entity-filter tests — the filtering behavior must be unchanged.

- [ ] **Step 5: Verify the fix bites**

Temporarily revert just the `Err(...)` arm to `return None` (silent close), re-run the lag test, confirm it FAILS, then restore. Per this project's established practice — a cert that can't fail isn't a cert.

- [ ] **Step 6: Commit**

```bash
cd /tank/repos/tailoredshapes/meshql-rs
cargo fmt && cargo clippy -p meshql-changes --all-targets
git add meshql-changes/src/sse.rs
git commit -m "fix(changes): announce broadcast lag instead of silently closing the stream"
```

---

# Step 2 — `ChangeEvent` gains `cursor` and `payload`

Isolates the breaking struct change from any behavior change. Both fields stay `None` everywhere after this step.

### Task 2: Add the fields, keep tokens off the wire

**Files:**
- Modify: `meshql-changes/src/event.rs`
- Modify: `meshql-changes/src/tail.rs` (construction site), `meshql-changes/src/hub.rs` + `src/sse.rs` (test constructors), `meshql-changes/src/testing.rs`, `meshql-changes/tests/sse_integration.rs`

- [ ] **Step 1: Write failing tests for the new wire fields**

Add to `meshql-changes/src/event.rs`'s test module:

```rust
    #[test]
    fn wire_json_includes_cursor_and_payload_when_present() {
        let mut e = event();
        e.cursor = Some("0:42".into());
        e.payload = Some(serde_json::json!({"eggs": 3}));
        let v: serde_json::Value = serde_json::from_str(&e.wire_json()).unwrap();
        assert_eq!(v["cursor"], "0:42");
        assert_eq!(v["payload"]["eggs"], 3);
    }

    #[test]
    fn wire_json_omits_cursor_and_payload_when_absent() {
        let v: serde_json::Value = serde_json::from_str(&event().wire_json()).unwrap();
        assert!(v.get("cursor").is_none(), "absent cursor must be omitted, not null");
        assert!(v.get("payload").is_none(), "absent payload must be omitted, not null");
    }

    #[test]
    fn wire_json_never_leaks_tokens_even_with_payload() {
        let mut e = event();
        e.payload = Some(serde_json::json!({"note": "fine"}));
        let wire = e.wire_json();
        assert!(!wire.contains("secret-team"));
        assert!(!wire.contains("authorized_tokens"));
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p meshql-changes --lib event::`
Expected: FAIL to compile — `ChangeEvent` has no `cursor`/`payload` field.

- [ ] **Step 3: Add the fields**

In `meshql-changes/src/event.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
    /// Filtering input only — NEVER serialized. See `wire_json`.
    pub authorized_tokens: Vec<String>,
    /// Resume cursor, `"{partition}:{offset}"`. `Some` only for sources that
    /// can seek (merkql); `None` for tail-based sources, which don't resume.
    pub cursor: Option<String>,
    /// The changed payload, when the streamlette is configured to carry it.
    /// Only sound on merkql-backed sources — see the spec's Payload section
    /// on `SearcherTail`'s token staleness.
    pub payload: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct WireEvent<'a> {
    entity: &'a str,
    id: &'a str,
    created_at: i64,
    deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<&'a serde_json::Value>,
}
```

Update `wire_json` to pass `cursor: self.cursor.as_deref()` and `payload: self.payload.as_ref()`.

- [ ] **Step 4: Fix every construction site**

`ChangeEvent` is constructed with struct literals in `src/tail.rs`, test helpers in `src/hub.rs` and `src/sse.rs`, `src/testing.rs`, and `tests/sse_integration.rs`. Add `cursor: None, payload: None` to each. Let the compiler find them:

Run: `cargo build -p meshql-changes --all-targets 2>&1 | grep "missing field"`

- [ ] **Step 5: Run the full crate**

Run: `cargo test -p meshql-changes`
Expected: PASS, including the pre-existing `wire_json_never_leaks_tokens` and `tests/sse_integration.rs`'s wire-shape assertions (both additive-safe).

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p meshql-changes --all-targets
git add meshql-changes/
git commit -m "feat(changes): ChangeEvent carries optional cursor and payload"
```

---

# Step 3 — `ServerConfig.streamlettes`, `Tail` source, `ready` frame

A working per-entity stream on every backend. No resume, no payload yet.

### Task 3: `StreamletteConfig` and `StreamSource`

**Files:**
- Create: `meshql-changes/src/streamlette.rs`
- Modify: `meshql-changes/src/lib.rs` (add `pub mod streamlette;` + re-exports)

- [ ] **Step 1: Define the types**

Create `meshql-changes/src/streamlette.rs`. `MerkqlTopic` is deliberately **not** here — it lives in `meshql-merkql` (see "Crate placement"), so `StreamSource` carries a boxed `ChangeSource` for the seekable case rather than naming merkql.

```rust
//! A per-meshlette SSE surface: one entity, one hub, one pump.
//!
//! Distinct from `changes_router`'s deployment-level `/changes` feed, which
//! stays exactly as it is — this adds a pump model, it does not replace one.

use crate::{ChangeEvent, ChangeHub, ChangeSource};
use std::sync::Arc;
use std::time::Duration;

/// A source that can replay history from a cursor. Implemented by
/// `meshql-merkql`'s `MerkqlTopicSource`; kept as a trait here so
/// `meshql-changes` needs no merkql dependency.
#[async_trait::async_trait]
pub trait SeekableSource: ChangeSource {
    /// Events strictly after `cursor`, in log order.
    async fn backfill(&self, cursor: &str) -> anyhow::Result<Vec<ChangeEvent>>;
    /// Whether `cursor` is usable. An unusable cursor degrades the
    /// connection to `resume: false` — never a silent skip.
    fn cursor_is_valid(&self, cursor: &str) -> bool;
}

pub enum StreamSource {
    /// Poll-diff an existing store via `SearcherTail`. Every backend.
    /// No resume, no payload.
    Tail {
        source: Arc<dyn ChangeSource>,
        poll_interval: Duration,
    },
    /// A log-backed source that supports resume and may carry payloads.
    Seekable {
        source: Arc<dyn SeekableSource>,
        poll_interval: Duration,
    },
}

impl StreamSource {
    pub fn poll_interval(&self) -> Duration {
        match self {
            Self::Tail { poll_interval, .. } | Self::Seekable { poll_interval, .. } => *poll_interval,
        }
    }
    pub fn supports_resume(&self) -> bool {
        matches!(self, Self::Seekable { .. })
    }
}

pub struct StreamletteConfig {
    /// e.g. "/message_posted/stream"
    pub path: String,
    /// the `ChangeEvent.entity` this stream carries
    pub entity: String,
    pub source: StreamSource,
    /// Broadcast buffer. Payload-carrying streams lag sooner at the same
    /// capacity, so size this up when payloads are on.
    pub hub_capacity: usize,
}
```

- [ ] **Step 2: Export from `lib.rs`**

Add to `meshql-changes/src/lib.rs`:

```rust
pub mod streamlette;
pub use streamlette::{SeekableSource, StreamSource, StreamletteConfig};
```

- [ ] **Step 3: Build**

Run: `cargo build -p meshql-changes`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy -p meshql-changes --all-targets
git add meshql-changes/
git commit -m "feat(changes): StreamletteConfig and StreamSource types"
```

### Task 4: Per-streamlette pump

**Files:**
- Modify: `meshql-changes/src/streamlette.rs`

- [ ] **Step 1: Write the failing test**

Add to `meshql-changes/src/streamlette.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeEvent;
    use std::sync::Mutex;

    struct FakeSource {
        entity: String,
        batches: Mutex<Vec<Vec<ChangeEvent>>>,
    }

    #[async_trait::async_trait]
    impl ChangeSource for FakeSource {
        fn entity(&self) -> &str { &self.entity }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(self.batches.lock().unwrap().pop().unwrap_or_default())
        }
    }

    fn ev(entity: &str, id: &str) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(), id: id.into(), created_at: 1, deleted: false,
            authorized_tokens: vec![], cursor: None, payload: None,
        }
    }

    #[tokio::test]
    async fn pump_publishes_polled_events_to_the_hub() {
        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(FakeSource {
            entity: "hen".into(),
            batches: Mutex::new(vec![vec![ev("hen", "e1")]]),
        });

        tokio::spawn(run_pump(source, hub.clone(), Duration::from_millis(5)));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await.expect("pump must publish within 2s").unwrap();
        assert_eq!(got.id, "e1");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p meshql-changes --lib streamlette::`
Expected: FAIL — `run_pump` doesn't exist.

- [ ] **Step 3: Implement the pump**

Add to `meshql-changes/src/streamlette.rs`. **One task per streamlette** — deliberately not `run_tails`, which is a single round-robin task with one shared interval, so a slow `SearcherTail::find_all` would block an aggressive merkql poll (see spec, "Two sources").

```rust
/// Drive one source into one hub. One task per streamlette, so a slow
/// source never blocks a fast one — the reason this is not `run_tails`.
pub async fn run_pump(source: Arc<dyn ChangeSource>, hub: ChangeHub, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match source.poll().await {
            Ok(events) => {
                for ev in events {
                    hub.publish(ev);
                }
            }
            // A transient source error must not kill the pump — the next
            // tick retries. Subscribers see a gap, which the lagged/refetch
            // path already covers.
            Err(e) => tracing::warn!(entity = source.entity(), error = %e, "streamlette poll failed"),
        }
    }
}
```

If `tracing` isn't already a dependency of `meshql-changes`, use `eprintln!` rather than adding one for this.

- [ ] **Step 4: Run**

Run: `cargo test -p meshql-changes --lib streamlette::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p meshql-changes --all-targets
git add meshql-changes/
git commit -m "feat(changes): per-streamlette pump task"
```

### Task 5: Streamlette handler with the `ready` frame

**Files:**
- Modify: `meshql-changes/src/streamlette.rs`

- [ ] **Step 1: Write failing tests**

```rust
    #[tokio::test]
    async fn ready_frame_declares_live_only_for_a_tail_source() {
        let frames = collect_ready(StreamSource::Tail {
            source: Arc::new(FakeSource { entity: "hen".into(), batches: Mutex::new(vec![]) }),
            poll_interval: Duration::from_millis(50),
        }, None).await;
        assert!(frames.contains("ready"));
        assert!(frames.contains(r#""resume":false"#));
        assert!(frames.contains(r#""cursor":null"#));
    }

    #[tokio::test]
    async fn tail_source_ignores_last_event_id_rather_than_erroring() {
        // A 400 would make the browser reconnect in a loop; ignoring plus an
        // honest `ready` frame lets the client detect the rejection.
        let frames = collect_ready(StreamSource::Tail {
            source: Arc::new(FakeSource { entity: "hen".into(), batches: Mutex::new(vec![]) }),
            poll_interval: Duration::from_millis(50),
        }, Some("0:99")).await;
        assert!(frames.contains(r#""resume":false"#));
    }
```

Write `collect_ready` as a helper that builds the handler's stream and returns the first frame's debug string.

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p meshql-changes --lib streamlette::`
Expected: FAIL — no handler yet.

- [ ] **Step 3: Implement the `ready` frame and router**

`ready` is emitted **by the streamlette handler, not by `change_stream`** — both this and the deployment-level `/changes` call `change_stream`, and putting `ready` there would silently change `/changes`'s wire contract for existing consumers including meshobj's TS client.

```rust
/// First frame on every streamlette connection: declares the mode actually
/// honoured. `cursor` is the position resume STARTED FROM (never the log
/// tail), so a client can detect that the cursor it sent was rejected.
fn ready_frame(resume: bool, cursor: Option<&str>) -> Event {
    let cursor_json = match cursor {
        Some(c) => format!("\"{c}\""),
        None => "null".to_string(),
    };
    Event::default()
        .event("ready")
        .data(format!(r#"{{"resume":{resume},"cursor":{cursor_json}}}"#))
    // Deliberately no `.id(...)` — a `ready` frame must not clobber the
    // browser's Last-Event-ID tracking.
}

pub fn streamlette_router(config: StreamletteConfig, auth: Arc<dyn Auth>) -> Router { /* ... */ }
```

The handler: read `Last-Event-ID` from headers; if the source isn't `Seekable` or the cursor is invalid, emit `ready_frame(false, None)` then `change_stream`. Resume is Task 9.

- [ ] **Step 4: Run**

Run: `cargo test -p meshql-changes --lib streamlette::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p meshql-changes --all-targets
git add meshql-changes/
git commit -m "feat(changes): streamlette handler with ready frame"
```

### Task 6: Mount streamlettes from `ServerConfig`

**Files:**
- Modify: `meshql-core/src/config.rs` (~line 177)
- Modify: `meshql-server/src/lib.rs` (`build_app`, `build_app_ext`, `build_app_with_auth`)
- Modify: ~20 construction sites across 19 files

**The spec's `ServerConfig.streamlettes` is not buildable — this task deviates deliberately, and the deviation is an improvement.**

`StreamletteConfig` lives in `meshql-changes`, and `meshql-changes` already depends on `meshql-core` (verified: `meshql-core` + `meshql-sqlite`). So `meshql-core::ServerConfig` cannot name `StreamletteConfig` without a dependency cycle. Moving `StreamletteConfig` into `meshql-core` isn't an option either — it references `ChangeSource`/`ChangeHub`, which live in `meshql-changes`.

**Resolution: streamlettes are passed to `meshql-server`, not stored on `ServerConfig`.** Verified safe: `meshql-server` depends on `meshql-core`/`meshql-graphlette`/`meshql-restlette` and **not** on `meshql-changes`, while `meshql-changes` does **not** depend on `meshql-server` — so adding `meshql-server → meshql-changes` creates no cycle.

This is strictly better than the spec's shape: it also avoids the ~20 breaking construction-site edits the spec budgeted for, since `ServerConfig` is untouched.

- [ ] **Step 1: Add the dependency and the new entry point**

Add to `meshql-server/Cargo.toml`:

```toml
meshql-changes = { version = "0.1.0", path = "../meshql-changes" }
```

Add to `meshql-server/src/lib.rs` — a new function rather than changing existing signatures, so every current caller keeps compiling:

```rust
/// Like `build_app_ext`, plus per-meshlette SSE surfaces. Each streamlette
/// gets its own hub and its own pump task (see meshql-changes::streamlette
/// for why this is not the shared `run_tails` round-robin).
pub async fn build_app_with_streams(
    config: ServerConfig,
    extra: Router,
    streamlettes: Vec<StreamletteConfig>,
    auth: Arc<dyn Auth>,
) -> anyhow::Result<Router> {
    let mut extra = extra;
    for sl in streamlettes {
        let hub = ChangeHub::new(sl.hub_capacity);
        let source = match &sl.source { /* Tail | Seekable */ };
        tokio::spawn(meshql_changes::streamlette::run_pump(
            source, hub.clone(), sl.source.poll_interval(),
        ));
        extra = extra.merge(meshql_changes::streamlette::streamlette_router(sl, hub, auth.clone()));
    }
    build_app_ext(config, extra).await
}
```

- [ ] **Step 2: Update the spec to match**

Edit `docs/superpowers/specs/2026-07-24-streamlette-design.md`'s Configuration section: remove `streamlettes` from the `ServerConfig` snippet, describe the `build_app_with_streams` entry point, and delete the "Breaking-change churn" paragraph (no longer applicable). Note the cycle as the reason. A plan that silently diverges from its spec leaves the next reader trusting the wrong document.

- [ ] **Step 2: Write an integration test**

Create `meshql-changes/tests/streamlette_integration.rs`: boot a server with one `Tail`-sourced streamlette, connect, assert the `ready` frame arrives, write via the repository, assert a `change` frame follows.

- [ ] **Step 3: Run**

Run: `cargo test -p meshql-changes --test streamlette_integration`
Expected: PASS.

- [ ] **Step 4: Full workspace build**

Run: `cargo build --workspace --all-targets`
Expected: clean — confirms no construction site was missed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets
git add -A
git commit -m "feat(server): mount per-meshlette streamlettes"
```

---

# Step 4 — `MerkqlTopic` source, resume, payload

### Task 7: `MerkqlTopicSource` with its three hard constraints

**Files:**
- Create: `meshql-merkql/src/stream.rs`
- Modify: `meshql-merkql/Cargo.toml` (add `meshql-changes`), `meshql-merkql/src/lib.rs`

Three constraints from the spec, each of which produces a *silent* failure if skipped. Implement all three in this task, with a test each.

- [ ] **Step 1: Write the three failing tests**

```rust
    #[tokio::test]
    async fn rejects_a_multi_partition_topic() {
        // A single Last-Event-ID cannot address N partitions: history outside
        // the cursor's partition is silently dropped or duplicated.
        let broker = /* broker with topic "multi" created with 2 partitions */;
        let err = MerkqlTopicSource::new(broker, "multi", false).unwrap_err();
        assert!(err.to_string().contains("single-partition"));
    }

    #[tokio::test]
    async fn delivers_the_first_write_when_started_before_any_write() {
        // Consumer::subscribe silently skips topics that don't exist, and
        // `positions` is only populated there — so without pre-creation this
        // stream is dead PERMANENTLY, not just until the first write.
        let broker = /* fresh broker, NO topic yet */;
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        /* produce one record to "hen" */
        let events = source.poll().await.unwrap();
        assert_eq!(events.len(), 1, "first write after a cold start must be delivered");
    }

    #[tokio::test]
    async fn each_source_uses_a_fresh_group_id() {
        // Consumer::subscribe prefers a COMMITTED offset over offset_reset
        // unconditionally, so a reused group_id starts from the wrong place.
        let broker = /* broker with 3 records on "hen" */;
        let a = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        let b = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        assert_eq!(a.poll().await.unwrap().len(), 3);
        assert_eq!(b.poll().await.unwrap().len(), 3, "a second source must also see full history");
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p meshql-merkql --lib stream::`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement**

```rust
impl MerkqlTopicSource {
    pub fn new(broker: BrokerRef, topic: &str, include_payload: bool) -> anyhow::Result<Self> {
        // 1. Pre-create the topic, idempotently. merkql creates topics lazily
        //    on produce, and Consumer::subscribe silently skips a topic that
        //    doesn't exist yet — permanently, since `positions` is only
        //    populated inside subscribe.
        broker.create_topic(topic, 1)?;   // idempotent; check merkql's exact signature

        // 2. Refuse multi-partition: one Last-Event-ID cannot address N.
        let partitions = /* broker.topic(topic).num_partitions() */;
        anyhow::ensure!(
            partitions == 1,
            "streamlette topic {topic} must be single-partition (found {partitions}); \
             a single Last-Event-ID cannot address multiple partitions"
        );

        // 3. Fresh group_id per source — committed offsets beat offset_reset.
        let group_id = format!("streamlette-{}", uuid::Uuid::new_v4());
        // ... build Consumer with OffsetReset::Earliest, wrap in Mutex
        //     (Consumer::poll takes &mut self and is synchronous)
    }
}
```

`Consumer::poll(&mut self, _timeout)` is synchronous and **ignores its timeout, returning immediately** — hold it behind a `std::sync::Mutex` and call it from the pump's tick. Do not busy-loop.

- [ ] **Step 4: Run**

Run: `cargo test -p meshql-merkql --lib stream::`
Expected: PASS (3/3).

- [ ] **Step 5: Verify each constraint bites**

Break each of the three (skip `create_topic`; drop the partition assert; use a constant `group_id`), confirm the matching test goes red, restore. All three failures are silent in production — this is the only proof the guards work.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p meshql-merkql --all-targets
git add meshql-merkql/
git commit -m "feat(merkql): MerkqlTopicSource with single-partition, pre-creation, fresh group_id guards"
```

### Task 8: Cursor and payload emission

**Files:**
- Modify: `meshql-merkql/src/stream.rs`

- [ ] **Step 1: Write failing tests** — `poll()` sets `cursor: Some("0:{offset}")` from `Record.offset`/`Record.partition`; `payload` is `Some` iff `include_payload`, and `None` otherwise.
- [ ] **Step 2:** Run, watch fail.
- [ ] **Step 3:** Implement — map each `Record` to a `ChangeEvent`, deserializing the envelope from `record.value`.
- [ ] **Step 4:** Run, PASS.
- [ ] **Step 5:** Commit — `feat(merkql): emit resume cursor and optional payload`

### Task 9: Resume, backfill, and the handover

**Files:**
- Modify: `meshql-merkql/src/stream.rs` (`SeekableSource` impl), `meshql-changes/src/streamlette.rs` (handler)

- [ ] **Step 1: Write failing tests**

```rust
    // Backfill returns exactly what follows the cursor.
    async fn backfill_returns_events_strictly_after_the_cursor()
    // The race the spec calls out: subscribe BEFORE reading history, buffer
    // live events during backfill, emit history then buffer, dedupe by cursor.
    async fn a_write_during_backfill_is_delivered_exactly_once()
    // Lag during backfill degrades like any other lag.
    async fn lag_during_backfill_yields_a_lagged_frame()
```

`a_write_during_backfill_is_delivered_exactly_once` is the important one — inject a write mid-backfill and assert the client sees it once, not zero or twice.

- [ ] **Step 2:** Run, watch fail.
- [ ] **Step 3: Implement** — v1 seek is subscribe-from-`Earliest` and skip records at-or-before the cursor (merkql has no seek API; **do not modify `/tank/repos/tailoredshapes/merkql/`**, it's a separate repo pinned at `v0.2.0`). Handler: subscribe to hub → read history → emit history → emit buffer, deduping by `cursor`.
- [ ] **Step 4:** Run, PASS.
- [ ] **Step 5: Verify** — break the dedupe, confirm `a_write_during_backfill_is_delivered_exactly_once` goes red, restore.
- [ ] **Step 6:** Commit — `feat(merkql): Last-Event-ID resume with backfill handover`

### Task 10: Unusable-cursor policy

**Files:**
- Modify: `meshql-merkql/src/stream.rs` (`cursor_is_valid`), `meshql-changes/src/streamlette.rs`

- [ ] **Step 1: Write failing tests** — four cases, each currently yielding a silently empty stream: malformed cursor; partition != 0; offset beyond the log tail; offset below the retention floor. All must produce `ready` with `resume:false`, then a live stream.
- [ ] **Step 2:** Run, watch fail.
- [ ] **Step 3:** Implement `cursor_is_valid`; handler degrades to `ready_frame(false, None)`.
- [ ] **Step 4:** Run, PASS.
- [ ] **Step 5:** Commit — `feat(merkql): unusable resume cursors degrade to live-only, never a silent skip`

### Task 11: Manifest conformance

**Files:**
- Modify: `examples/egg-economy/src/manifest.rs`, `examples/egg-economy/tests/manifest_conformance.rs`

- [ ] **Step 1: Write the failing test** — a surface advertising `"resume": true` must not be `Tail`-sourced. Model on the existing REST-honesty assertions in that file.
- [ ] **Step 2-4:** Run/implement/run. Emit per-entity `{"kind":"sse","path":"/{entity}/stream","resume":bool}` from the deployment's `StreamletteConfig`s. No schema change needed — `$defs/surface` is `additionalProperties: true`.
- [ ] **Step 5:** Commit — `feat(manifest): advertise streamlette surfaces with resume capability`

---

# Step 5 — `meshql-iron` guidance across five repos

### Task 12: `references/streaming.md`

**Files:**
- Create: `meshql-rs/.claude/skills/meshql-iron/references/streaming.md`

- [ ] **Step 1: Write the doc.** Cover: discover the `sse` surface and its `resume` flag from `/manifest`; `EventSource` subscribe; read the `ready` frame for the mode actually honoured; **fetch-on-connect and on every reconnect**; handle `lagged` by resyncing; `Last-Event-ID` only where advertised.

  Two rules that are easy to get wrong and cause duplicate messages:
  1. **After a full refetch, abandon the cursor.** Browser `EventSource` auto-reconnects *and* auto-resends `Last-Event-ID` with no API to clear it — so `close()` it and construct a **new** one. Letting it auto-reconnect after a refetch replays events the refetch already returned.
  2. **Payload-consuming clients dedupe locally by `cursor`** — delivery is at-least-once.

  Also: the connect-time-token caveat (a long-lived stream won't see a mid-connection privilege change; products needing hard revocation must force a disconnect) and `SearcherTail`'s token-staleness window.

- [ ] **Step 2: Commit** — `docs(iron): add streaming.md`

### Task 13: Three `SKILL.md` edits, synced to five repos

**Files:**
- Modify: `.claude/skills/meshql-iron/SKILL.md` in **`meshql-rs`, `meshql`, `meshobj`, `cms`, `teamchat`**

All five copies are byte-identical (same md5, regular files, not symlinks). Updating only `meshql-rs` leaves `teamchat` — the first intended consumer — still telling agents SSE is out of scope.

- [ ] **Step 1: Make three edits in `meshql-rs`'s copy**
  1. **Decision guide** (lines ~18-23): *add* an SSE entry — it currently has none.
  2. **Non-goals line ~27** ("No SSE/`/changes` stream consumption…"): replace with a pointer to `streaming.md`.
  3. **Non-goals line ~29** ("No reactive store, no subscribe/notify machinery"): rewrite — streaming is now in scope, though a *reactive store abstraction* still isn't.

- [ ] **Step 2: Verify the edit**

Run: `grep -n "streaming.md" /tank/repos/tailoredshapes/meshql-rs/.claude/skills/meshql-iron/SKILL.md`

- [ ] **Step 3: Sync to the other four and verify identical**

```bash
cd /tank/repos/tailoredshapes
for r in meshql meshobj cms teamchat; do
  cp meshql-rs/.claude/skills/meshql-iron/SKILL.md $r/.claude/skills/meshql-iron/SKILL.md
  cp meshql-rs/.claude/skills/meshql-iron/references/streaming.md $r/.claude/skills/meshql-iron/references/streaming.md
done
md5sum */.claude/skills/meshql-iron/SKILL.md   # all five must match
```

- [ ] **Step 4: Commit in each repo** (five separate commits — separate repos)

```bash
for r in meshql-rs meshql meshobj cms teamchat; do
  (cd /tank/repos/tailoredshapes/$r && git add .claude/skills/meshql-iron && \
   git commit -m "docs(iron): teach SSE streaming consumption")
done
```

---

## Final verification

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets` zero warnings
- [ ] `cargo test --workspace` green
- [ ] Docker-backed adapters: `cargo test -p meshql-mongo -p meshql-postgres` (the pre-push hook deliberately skips these). MySQL needs `-- --test-threads=3` per test target; the flag is rejected on whole-package runs because the cucumber targets use `harness = false`.
- [ ] All five `SKILL.md` copies identical (`md5sum`)

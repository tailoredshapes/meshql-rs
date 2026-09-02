//! A per-meshlette SSE surface: one entity, one hub, one pump.
//!
//! Distinct from `changes_router`'s deployment-level `/changes` feed, which
//! stays exactly as it is — this adds a pump model, it does not replace one.

use crate::sse::{change_frame, change_stream_skipping};
use crate::{ChangeEvent, ChangeHub, ChangeSource};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Extension, Router};
use meshql_core::{Auth, AuthContext, Operation};
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::Stream;

/// A source that can replay history from a cursor. Implemented by
/// `meshql-merkql`'s log-backed source; kept as a trait here so
/// `meshql-changes` needs no merkql dependency.
#[async_trait::async_trait]
pub trait SeekableSource: ChangeSource {
    /// Events strictly after `cursor`, in log order.
    async fn backfill(&self, cursor: &str) -> anyhow::Result<Vec<ChangeEvent>>;
    /// Whether `cursor` is usable. An unusable cursor degrades the
    /// connection to `resume: false` — never a silent skip.
    fn cursor_is_valid(&self, cursor: &str) -> bool;
}

/// Where a streamlette's events come from, and how often we go looking.
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
    /// How long the pump sleeps between polls. Both variants poll — the
    /// difference between them is resume, not cadence.
    pub fn poll_interval(&self) -> Duration {
        match self {
            Self::Tail { poll_interval, .. } => *poll_interval,
            Self::Seekable { poll_interval, .. } => *poll_interval,
        }
    }

    /// Whether a client may reconnect with `Last-Event-ID` and be backfilled.
    pub fn supports_resume(&self) -> bool {
        matches!(self, Self::Seekable { .. })
    }
}

/// One entity's SSE surface.
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

/// Drive one source into one hub. One task per streamlette, so a slow
/// source never blocks a fast one — the reason this is not `run_tails`,
/// which shares a single round-robin task and one interval across every
/// source it owns. A log-backed source is cheap enough to poll
/// aggressively (an in-memory offset compare); a `SearcherTail` is not
/// (a full `find_all` plus payload-hash diff). Sharing a loop between
/// them makes the per-source interval inert.
///
/// Loops forever by design — the caller owns the `tokio::spawn` handle.
pub async fn run_pump(source: Arc<dyn ChangeSource>, hub: ChangeHub, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match source.poll().await {
            Ok(events) => {
                for event in events {
                    hub.publish(event);
                }
            }
            // A transient source error must not kill the pump — the next
            // tick retries. Subscribers see a gap, which the lagged/refetch
            // path already covers.
            Err(e) => eprintln!("[streamlette {}] poll: {e}", source.entity()),
        }
    }
}

/// First frame on every streamlette connection: declares the mode actually
/// honoured. `cursor` is the position resume STARTED FROM (never the log
/// tail), so a client comparing it against the `Last-Event-ID` it sent can
/// tell that its cursor was rejected. `null` whenever `resume` is false.
///
/// Deliberately carries NO SSE `id:` — an id here would overwrite the
/// browser's `Last-Event-ID` tracking with a non-event position.
///
/// Emitted HERE and not inside `change_stream`, because `change_stream` is
/// shared with the deployment-level `/changes` route: adding a frame there
/// would silently change that route's wire contract for existing consumers.
fn ready_frame(resume: bool, cursor: Option<&str>) -> Event {
    let cursor = match cursor {
        Some(c) => serde_json::Value::String(c.to_string()),
        None => serde_json::Value::Null,
    };
    let data = serde_json::json!({ "resume": resume, "cursor": cursor });
    Event::default().event("ready").data(data.to_string())
}

#[derive(Clone)]
struct StreamletteState {
    hub: ChangeHub,
    auth: Arc<dyn Auth>,
    entity: String,
    /// `Some` only for a `Seekable` source — the resume path's whole
    /// precondition, so a `Tail` streamlette cannot accidentally take it.
    seekable: Option<Arc<dyn SeekableSource>>,
}

/// Mount one streamlette. The hub is created by the caller
/// (`build_app_with_streams`), which also spawns the pump feeding it —
/// hence three arguments rather than a hub built in here.
///
/// Pass the SAME `Arc<dyn Auth>` the lettes use, so the stream and the
/// query surfaces agree on caller identity.
pub fn streamlette_router(
    config: StreamletteConfig,
    hub: ChangeHub,
    auth: Arc<dyn Auth>,
) -> Router {
    let StreamletteConfig {
        path,
        entity,
        source,
        ..
    } = config;
    // Only a Seekable source can be resumed from, so only a Seekable source
    // is carried into the handler. The pump owns its own clone (see
    // `build_app_with_streams`); this one exists purely for `backfill`, which
    // opens its own reader and never touches the pump's position.
    let seekable = match source {
        StreamSource::Seekable { source, .. } => Some(source),
        StreamSource::Tail { .. } => None,
    };
    Router::new()
        .route(&path, get(streamlette_handler))
        .with_state(StreamletteState {
            hub,
            auth,
            entity,
            seekable,
        })
}

async fn streamlette_handler(
    State(state): State<StreamletteState>,
    auth_ctx: Option<Extension<AuthContext>>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stash = auth_ctx.map(|e| e.0 .0).unwrap_or_default();
    let session = state.auth.authenticate(&AuthContext::new(stash));

    // An unusable or unsupported Last-Event-ID is NOT a 400: SSE auto-reconnect
    // would turn an error response into a reconnect loop. It degrades to
    // live-only and the `ready` frame tells the client the truth.
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    // A streamlette is one entity's surface: never leak a sibling's events.
    let entities: HashSet<String> = std::iter::once(state.entity).collect();

    // ── The handover. Order is the whole correctness argument. ──
    //
    // 1. Subscribe FIRST, before a single byte of history is read. Anything
    //    committed while the backfill runs then lands in this receiver's
    //    buffer instead of falling down the gap between "history ends" and
    //    "live begins".
    let rx = state.hub.subscribe();

    // 2. Read history — only when there is a seekable source AND a cursor it
    //    accepts.
    let resume_from = last_event_id.filter(|cursor| {
        state
            .seekable
            .as_ref()
            .is_some_and(|s| s.cursor_is_valid(cursor))
    });

    // 3. Render the history frames, remembering every cursor delivered.
    let mut prefix: Vec<Result<Event, Infallible>> = Vec::new();
    let mut already_delivered: HashSet<String> = HashSet::new();
    let mut resumed = None;

    if let (Some(source), Some(cursor)) = (&state.seekable, &resume_from) {
        match source.backfill(cursor).await {
            Ok(history) => {
                for event in history {
                    // Recorded before filtering, so the skip set drains on
                    // exactly what the backfill covered — not on the subset
                    // this caller happens to be allowed to see.
                    if let Some(c) = &event.cursor {
                        already_delivered.insert(c.clone());
                    }
                    if !entities.contains(&event.entity) {
                        continue;
                    }
                    // Replayed history goes to the SAME plugin as live events.
                    // A resume must not become a way to read history you were
                    // never allowed to see.
                    if !session.is_authorized(Operation::Read, &event.as_envelope()) {
                        continue;
                    }
                    prefix.push(Ok(change_frame(&event)));
                }
                resumed = Some(cursor.clone());
            }
            // A failed backfill degrades to live-only rather than killing the
            // connection — and says so, so the client refetches.
            Err(e) => eprintln!("[streamlette] backfill from '{cursor}': {e}"),
        }
    }

    // The `ready` frame reports the position resume actually STARTED FROM —
    // what the client sent, never the current log tail — so a client can
    // detect rejection by comparing it against the id it sent.
    prefix.insert(0, Ok(ready_frame(resumed.is_some(), resumed.as_deref())));

    // 4. Then the live buffer, minus anything step 3 already delivered.
    let stream = tokio_stream::StreamExt::chain(
        tokio_stream::iter(prefix),
        change_stream_skipping(rx, session, Some(entities), already_delivered),
    );

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeHub;
    use async_trait::async_trait;

    fn ev(id: &str) -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: id.into(),
            created_at: 1,
            deleted: false,
            auth: Default::default(),
            cursor: None,
            payload: None,
        }
    }

    /// A log-backed event: cursor-carrying, so it is dedupable.
    fn at(id: &str, cursor: &str) -> ChangeEvent {
        ChangeEvent {
            cursor: Some(cursor.into()),
            ..ev(id)
        }
    }

    struct StubSource;

    #[async_trait]
    impl ChangeSource for StubSource {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    struct StubSeekable;

    #[async_trait]
    impl ChangeSource for StubSeekable {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl SeekableSource for StubSeekable {
        async fn backfill(&self, _cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
        fn cursor_is_valid(&self, cursor: &str) -> bool {
            cursor.contains(':')
        }
    }

    fn tail() -> StreamSource {
        StreamSource::Tail {
            source: Arc::new(StubSource),
            poll_interval: Duration::from_millis(250),
        }
    }

    fn seekable() -> StreamSource {
        StreamSource::Seekable {
            source: Arc::new(StubSeekable),
            poll_interval: Duration::from_millis(50),
        }
    }

    #[test]
    fn tail_does_not_support_resume() {
        assert!(!tail().supports_resume());
    }

    #[test]
    fn seekable_supports_resume() {
        assert!(seekable().supports_resume());
    }

    #[test]
    fn poll_interval_reads_through_both_variants() {
        assert_eq!(tail().poll_interval(), Duration::from_millis(250));
        assert_eq!(seekable().poll_interval(), Duration::from_millis(50));
    }

    /// Task 6 needs to hand a `Seekable`'s source to the shared pump, which
    /// takes `Arc<dyn ChangeSource>`. Pin the upcast here so a regression
    /// shows up as a compile failure in this crate, not downstream.
    #[test]
    fn seekable_source_upcasts_to_change_source() {
        let seekable: Arc<dyn SeekableSource> = Arc::new(StubSeekable);
        let base: Arc<dyn ChangeSource> = seekable;
        assert_eq!(base.entity(), "hen");
    }

    /// Drive the REAL router and read the REAL wire bytes off the response
    /// body, stopping as soon as `pred` is satisfied.
    ///
    /// Unlike `sse.rs`'s `wire_frames`, this cannot read the body to
    /// completion: the production handler DOES set `.keep_alive()`, so the
    /// body never ends. So it streams chunks and stops on a predicate,
    /// under a timeout — a regression shows up as a failed assertion, not a
    /// hung test.
    async fn wire_until(
        source: StreamSource,
        last_event_id: Option<&str>,
        hub: ChangeHub,
        pred: impl Fn(&str) -> bool,
    ) -> String {
        use futures::StreamExt as _;
        use tower::ServiceExt as _;

        let config = StreamletteConfig {
            path: "/hen/stream".into(),
            entity: "hen".into(),
            source,
            hub_capacity: 16,
        };
        let app = streamlette_router(config, hub, Arc::new(meshql_core::NoAuth));

        let mut req = axum::http::Request::builder()
            .uri("/hen/stream")
            .method("GET");
        if let Some(id) = last_event_id {
            req = req.header("last-event-id", id);
        }
        let resp = app
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let mut stream = resp.into_body().into_data_stream();
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let chunk = tokio::time::timeout_at(deadline, stream.next())
                .await
                .unwrap_or_else(|_| panic!("timed out; frames so far:\n{buf}"))
                .expect("stream ended early")
                .unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if pred(&buf) {
                return buf;
            }
        }
    }

    /// The parsed JSON of the FIRST frame's `data:` line — real wire bytes,
    /// not `Debug` output (`Event`'s Debug goes through a `BytesMut`, which
    /// escapes quotes, so substring matches on it can never match).
    async fn collect_ready(source: StreamSource, last_event_id: Option<&str>) -> serde_json::Value {
        let wire = wire_until(source, last_event_id, ChangeHub::new(16), |b| {
            b.contains("\n\n")
        })
        .await;
        ready_data(&wire)
    }

    fn ready_data(wire: &str) -> serde_json::Value {
        let first = wire.split("\n\n").next().expect("a first frame");
        assert!(
            first
                .lines()
                .any(|l| l.trim() == "event:ready" || l.trim() == "event: ready"),
            "the first frame must be `event: ready`, got:\n{wire}"
        );
        assert!(
            !first.lines().any(|l| l.starts_with("id:")),
            "the ready frame must carry NO id — it would clobber the browser's \
             Last-Event-ID tracking. Got:\n{wire}"
        );
        let data = first
            .lines()
            .find_map(|l| l.trim_end().strip_prefix("data:"))
            .expect("a data line on the ready frame")
            .trim_start();
        serde_json::from_str(data).expect("ready data is JSON")
    }

    #[tokio::test]
    async fn ready_frame_declares_live_only_for_a_tail_source() {
        let data = collect_ready(tail(), None).await;
        assert_eq!(data["resume"], false);
        assert_eq!(data["cursor"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn tail_source_ignores_last_event_id_rather_than_erroring() {
        let data = collect_ready(tail(), Some("0:99")).await;
        assert_eq!(data["resume"], false);
    }

    /// No cursor to resume from means no resume, even on a Seekable source.
    #[tokio::test]
    async fn seekable_source_without_a_cursor_is_live_only() {
        let data = collect_ready(seekable(), None).await;
        assert_eq!(data["resume"], false);
        assert_eq!(data["cursor"], serde_json::Value::Null);
    }

    /// A cursor the source rejects degrades to live-only — never a silent
    /// skip, and never a 400 (an error response would turn SSE auto-reconnect
    /// into a reconnect loop).
    #[tokio::test]
    async fn seekable_source_with_an_unusable_cursor_falls_back_to_live_only() {
        let data = collect_ready(seekable(), Some("garbage")).await;
        assert_eq!(data["resume"], false);
        assert_eq!(data["cursor"], serde_json::Value::Null);
    }

    /// A source that rejects every cursor, and blows up if asked to backfill
    /// one anyway. `backfill` must be unreachable once `cursor_is_valid` says
    /// no — reaching it is how a rejected cursor turns into a silent skip.
    struct RejectsEveryCursor;

    #[async_trait]
    impl ChangeSource for RejectsEveryCursor {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl SeekableSource for RejectsEveryCursor {
        async fn backfill(&self, cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
            panic!("backfill must not be called for the rejected cursor {cursor:?}");
        }
        fn cursor_is_valid(&self, _cursor: &str) -> bool {
            false
        }
    }

    /// The whole unusable-cursor policy at the handler: an unusable cursor
    /// yields `resume: false` AND a working live stream.
    ///
    /// The live half is the half that is easy to get wrong. A connection that
    /// honestly reports `resume: false` and then delivers nothing is the same
    /// silent failure by another route — the client falls back to
    /// fetch-on-connect and then waits forever for updates that never arrive.
    #[tokio::test]
    async fn an_unusable_cursor_degrades_to_a_live_stream_not_a_dead_one() {
        let hub = ChangeHub::new(16);
        let publisher = hub.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                publisher.publish(at("live-1", "0:7"));
            }
        });

        let source = StreamSource::Seekable {
            source: Arc::new(RejectsEveryCursor),
            poll_interval: Duration::from_millis(50),
        };

        // A well-formed cursor the SOURCE rejects (beyond-tail and
        // below-retention both look like this on the wire) — so the fallback
        // cannot be attributed to the handler failing to parse it.
        let wire = wire_until(source, Some("0:9999"), hub, |b| b.contains("live-1")).await;

        let data = ready_data(&wire);
        assert_eq!(data["resume"], false, "got:\n{wire}");
        assert_eq!(data["cursor"], serde_json::Value::Null, "got:\n{wire}");
        assert_eq!(
            change_ids(&wire),
            vec!["live-1"],
            "the connection must keep streaming live events after degrading, \
             got:\n{wire}"
        );
    }

    /// A source whose `backfill` returns whatever `history` says, and which
    /// publishes `during_backfill` into the hub *from inside* `backfill`.
    ///
    /// That is how the handover race is forced rather than raced: the handler
    /// has already subscribed by the time `backfill` is awaited, so publishing
    /// there IS "a write landed mid-backfill" — a fixed sequence, not a timing
    /// coincidence that might or might not reproduce on a loaded CI box.
    struct ScriptedSeekable {
        history: Vec<ChangeEvent>,
        during_backfill: Vec<ChangeEvent>,
        hub: ChangeHub,
    }

    #[async_trait]
    impl ChangeSource for ScriptedSeekable {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl SeekableSource for ScriptedSeekable {
        async fn backfill(&self, _cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
            for event in &self.during_backfill {
                self.hub.publish(event.clone());
            }
            // A real backfill reads a log and yields; yielding here makes the
            // interleaving a genuine await-point crossing rather than a
            // straight-line call the compiler could reorder away.
            tokio::task::yield_now().await;
            Ok(self.history.clone())
        }
        fn cursor_is_valid(&self, cursor: &str) -> bool {
            cursor.contains(':')
        }
    }

    fn scripted(
        hub: &ChangeHub,
        history: Vec<ChangeEvent>,
        during_backfill: Vec<ChangeEvent>,
    ) -> StreamSource {
        StreamSource::Seekable {
            source: Arc::new(ScriptedSeekable {
                history,
                during_backfill,
                hub: hub.clone(),
            }),
            poll_interval: Duration::from_millis(50),
        }
    }

    /// The `id` of every `event: change` frame on the wire, in order — parsed
    /// out of the real bytes, so duplicates and drops both show up.
    fn change_ids(wire: &str) -> Vec<String> {
        wire.split("\n\n")
            .filter(|frame| {
                frame
                    .lines()
                    .any(|l| l.trim() == "event:change" || l.trim() == "event: change")
            })
            .filter_map(|frame| {
                let data = frame
                    .lines()
                    .find_map(|l| l.trim_end().strip_prefix("data:"))?
                    .trim_start();
                let v: serde_json::Value = serde_json::from_str(data).ok()?;
                Some(v["id"].as_str()?.to_string())
            })
            .collect()
    }

    /// THE handover test.
    ///
    /// `0:3` is in history AND is published live while backfill runs — the
    /// duplicate a naive "history then buffer" emits twice. `0:4` is published
    /// live during backfill but is NOT in history — the event a naive
    /// "backfill then subscribe" loses entirely. Exactly-once means both.
    #[tokio::test]
    async fn a_write_during_backfill_is_delivered_exactly_once() {
        let hub = ChangeHub::new(64);
        let source = scripted(
            &hub,
            vec![at("e2", "0:2"), at("e3", "0:3")],
            vec![at("e3", "0:3"), at("e4", "0:4")],
        );

        let wire = wire_until(source, Some("0:1"), hub, |b| b.contains("e4")).await;

        assert_eq!(
            change_ids(&wire),
            vec!["e2", "e3", "e4"],
            "a write landing mid-backfill must be delivered exactly once — no \
             duplicate of the history overlap, no loss of the live-only tail. \
             Got:\n{wire}"
        );
    }

    /// The `ready` cursor is the position resume STARTED FROM — what the
    /// client sent — never the log tail. A client compares the two to detect
    /// that its cursor was rejected, which only works if we echo it.
    #[tokio::test]
    async fn resume_ready_frame_echoes_the_clients_cursor() {
        let hub = ChangeHub::new(16);
        // History runs well past the client's cursor, so echoing the tail
        // instead would be a visibly different value.
        let source = scripted(&hub, vec![at("e2", "0:2"), at("e3", "0:3")], vec![]);

        let wire = wire_until(source, Some("0:1"), hub, |b| b.contains("e3")).await;
        let data = ready_data(&wire);
        assert_eq!(data["resume"], true, "got:\n{wire}");
        assert_eq!(data["cursor"], "0:1", "got:\n{wire}");
    }

    /// Backfilled history goes out BEFORE the live buffer, so a client
    /// applying frames in order never sees the log out of order.
    #[tokio::test]
    async fn history_frames_precede_the_live_buffer() {
        let hub = ChangeHub::new(16);
        let source = scripted(&hub, vec![at("e2", "0:2")], vec![at("e3", "0:3")]);

        let wire = wire_until(source, Some("0:1"), hub, |b| b.contains("e3")).await;
        assert_eq!(change_ids(&wire), vec!["e2", "e3"], "got:\n{wire}");
        let ready = wire
            .find("event:ready")
            .or_else(|| wire.find("event: ready"));
        let first_change = wire
            .find("event:change")
            .or_else(|| wire.find("event: change"));
        assert!(ready < first_change, "ready must come first:\n{wire}");
    }

    /// A slow backfill can overrun the bounded broadcast buffer. That must
    /// degrade exactly like any other lag — the existing terminal `lagged`
    /// frame and a closed stream — not through a second, bespoke mechanism.
    #[tokio::test]
    async fn lag_during_backfill_yields_a_lagged_frame() {
        let hub = ChangeHub::new(2); // tiny buffer
        let flood: Vec<ChangeEvent> = (0..10)
            .map(|i| at(&format!("live{i}"), &format!("0:{}", 100 + i)))
            .collect();
        let source = scripted(&hub, vec![at("e2", "0:2")], flood);

        let wire = wire_until(source, Some("0:1"), hub, |b| b.contains("lagged")).await;

        assert!(
            wire.contains("event:lagged") || wire.contains("event: lagged"),
            "expected a terminal lagged frame, got:\n{wire}"
        );
        // Buffer 2, 10 published => 8 unrecoverable.
        assert!(
            wire.contains(r#"{"skipped":8}"#),
            "the lagged frame must carry the true skipped count, got:\n{wire}"
        );
        // The resume still happened and still announced itself honestly.
        let data = ready_data(&wire);
        assert_eq!(data["resume"], true, "got:\n{wire}");
    }

    #[tokio::test]
    async fn change_frames_follow_the_ready_frame() {
        let hub = ChangeHub::new(16);
        let publisher = hub.clone();

        // The handler subscribes while the router is being driven, so publish
        // from a task that fires shortly after the response head is produced.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                publisher.publish(ev("e1"));
            }
        });

        let wire = wire_until(tail(), None, hub, |b| b.contains("e1")).await;
        let ready_at = wire
            .find("event:ready")
            .or_else(|| wire.find("event: ready"));
        let change_at = wire
            .find("event:change")
            .or_else(|| wire.find("event: change"));
        assert!(ready_at.is_some(), "no ready frame in:\n{wire}");
        assert!(change_at.is_some(), "no change frame in:\n{wire}");
        assert!(
            ready_at < change_at,
            "ready must precede the first change frame, got:\n{wire}"
        );
    }

    /// A streamlette is one entity's surface. Hubs may carry siblings (a
    /// shared hub, or a pump misconfigured onto the wrong entity); the
    /// stream must still only ever show its own.
    #[tokio::test]
    async fn a_streamlette_never_leaks_a_sibling_entitys_events() {
        let hub = ChangeHub::new(16);
        let publisher = hub.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut sibling = ev("sibling");
                sibling.entity = "farm".into();
                publisher.publish(sibling);
                publisher.publish(ev("e1"));
            }
        });

        let wire = wire_until(tail(), None, hub, |b| b.contains("e1")).await;
        assert!(
            !wire.contains("sibling"),
            "a farm event leaked into the hen streamlette:\n{wire}"
        );
    }

    #[tokio::test]
    async fn pump_publishes_polled_events_to_the_hub() {
        use std::sync::Mutex;

        struct BatchSource {
            batches: Mutex<Vec<Vec<ChangeEvent>>>,
        }
        #[async_trait]
        impl ChangeSource for BatchSource {
            fn entity(&self) -> &str {
                "hen"
            }
            async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
                Ok(self.batches.lock().unwrap().pop().unwrap_or_default())
            }
        }

        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(BatchSource {
            batches: Mutex::new(vec![vec![ev("e1")]]),
        });

        tokio::spawn(run_pump(source, hub.clone(), Duration::from_millis(5)));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("pump must publish within 2s")
            .unwrap();
        assert_eq!(got.id, "e1");
    }

    /// A transient storage blip must not silently stop a stream forever.
    #[tokio::test]
    async fn pump_survives_a_source_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FlakySource {
            polls: AtomicUsize,
        }
        #[async_trait]
        impl ChangeSource for FlakySource {
            fn entity(&self) -> &str {
                "hen"
            }
            async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
                match self.polls.fetch_add(1, Ordering::SeqCst) {
                    0 => Err(anyhow::anyhow!("transient poll failure")),
                    1 => Ok(vec![ev("after-error")]),
                    _ => Ok(vec![]),
                }
            }
        }

        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(FlakySource {
            polls: AtomicUsize::new(0),
        });

        tokio::spawn(run_pump(source, hub.clone(), Duration::from_millis(5)));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("pump must keep polling after an error")
            .unwrap();
        assert_eq!(got.id, "after-error");
    }
}

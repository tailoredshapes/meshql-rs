//! The SSE surface: GET /changes streams thin change notifications.
//!
//! - `event: change`, `id:` = the event's resume cursor when it has one
//!   (omitted entirely for sources that can't seek, so a client never sends
//!   back a Last-Event-ID the server can't honour),
//!   `data:` = ChangeEvent::wire_json() (tokens stripped by construction).
//! - Per-subscriber filtering by asking the caller's auth session, exactly as
//!   the lettes do. The session is established once, at connect.
//! - Reconnect contract: no replay. The hub is in-memory; on (re)connect a
//!   client must treat all cached state as stale. Last-Event-ID is ignored
//!   in v1 (a log-backed source may honor it later).
//! - Lag: a subscriber that overruns the broadcast buffer gets a terminal
//!   `event: lagged` frame (`data: {"skipped":N}`) and then its stream is
//!   CLOSED (never silent drops), forcing the reconnect-refetch path.

use crate::{ChangeEvent, ChangeHub};
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Extension, Router};
use meshql_core::{Auth, AuthContext, Operation, Session};
use serde::Deserialize;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// One `event: change` frame.
///
/// Shared with the streamlette resume path, which renders backfilled history
/// with this same function: a replayed change and a live one describe the same
/// log record, so they must be byte-identical on the wire. The SSE `id` IS the
/// resume cursor — sources that can't seek emit no cursor and therefore no id,
/// so a browser never sends back a Last-Event-ID the server can't honour.
pub(crate) fn change_frame(ev: &ChangeEvent) -> Event {
    let mut event = Event::default().event("change").data(ev.wire_json());
    if let Some(cursor) = &ev.cursor {
        event = event.id(cursor);
    }
    event
}

/// The filtered notification stream for one subscriber. On broadcast lag it
/// emits a terminal `event: lagged` frame and then ends (closing the SSE
/// connection), so the client knows it must resync rather than guessing.
pub fn change_stream(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    session: Arc<dyn Session>,
    entities: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    // An empty skip set is a no-op, so `/changes` keeps its exact behaviour.
    change_stream_skipping(rx, session, entities, HashSet::new())
}

/// `change_stream`, plus a set of cursors already delivered.
///
/// The resume path subscribes BEFORE reading history, so anything committed
/// during the backfill lands in this receiver's buffer — including records the
/// backfill also returned. `already_delivered` holds those cursors; each is
/// consumed on its first (and, since a log offset is unique, only) match, so
/// the set drains rather than growing for the life of the connection.
///
/// Only meaningful on the `Seekable` path: `Tail` events carry `cursor: None`
/// and can never match.
pub(crate) fn change_stream_skipping(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    session: Arc<dyn Session>,
    entities: Option<HashSet<String>>,
    already_delivered: HashSet<String>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let mut already_delivered = already_delivered;
    // `map_while` lets us emit one final frame for the lag and THEN end,
    // which `take_while` cannot do.
    //
    // `done` is not defensive — it is what actually terminates the stream,
    // so do not remove it. `BroadcastStream` always resumes delivery after
    // a Lagged (tokio guarantees the receiver's position is advanced to the
    // oldest buffered value, so the next recv succeeds), which means the
    // Lagged arm alone can never end anything. `done` makes the following
    // poll return None. Disabling it makes the stream run forever.
    let mut done = false;
    BroadcastStream::new(rx)
        .map_while(move |item| {
            if done {
                return None;
            }
            match item {
                Err(BroadcastStreamRecvError::Lagged(skipped)) => {
                    done = true;
                    // Terminal frame: the client MUST resync (refetch) —
                    // `skipped` events were dropped and are unrecoverable.
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
                    // The handover skip. Checked BEFORE the token filter so
                    // the set drains on exactly the events that were
                    // backfilled, whatever the caller can see.
                    if let Some(cursor) = &ev.cursor {
                        if already_delivered.remove(cursor) {
                            return Some(None);
                        }
                    }
                    // The plugin decides, here as everywhere else.
                    if !session.is_authorized(Operation::Read, &ev.as_envelope()) {
                        return Some(None);
                    }
                    Some(Some(Ok(change_frame(&ev))))
                }
            }
        })
        .filter_map(|x| x)
}

#[derive(Clone)]
struct SseState {
    hub: ChangeHub,
    auth: Arc<dyn Auth>,
}

#[derive(Deserialize)]
struct ChangesParams {
    entities: Option<String>,
}

/// Build the /changes router. Merge into a deployment via `run_ext`
/// (in-process form) or serve from a standalone sidecar binary attached to
/// the same storage — same code, two deployment weights.
///
/// Pass the SAME `Arc<dyn Auth>` you pass to `build_app_with_auth` so the
/// stream and the lettes agree on caller identity.
pub fn changes_router(path: &str, hub: ChangeHub, auth: Arc<dyn Auth>) -> Router {
    Router::new()
        .route(path, get(changes_handler))
        .with_state(SseState { hub, auth })
}

async fn changes_handler(
    State(state): State<SseState>,
    auth_ctx: Option<Extension<AuthContext>>,
    Query(params): Query<ChangesParams>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stash = auth_ctx.map(|e| e.0 .0).unwrap_or_default();
    let session = state.auth.authenticate(&AuthContext::new(stash));
    let entities = params
        .entities
        .map(|s| {
            s.split(',')
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty())
                .collect::<HashSet<_>>()
        })
        // `?entities=` (empty after filtering) means "no filter", not
        // "filter everything" — a typo shouldn't yield a silently dead
        // stream that receives only heartbeats.
        .filter(|set| !set.is_empty());

    Sse::new(change_stream(state.hub.subscribe(), session, entities)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeEvent;
    use meshql_core::token_session;
    use tokio_stream::StreamExt;

    fn ev(entity: &str, id: &str, tokens: &[&str]) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(),
            id: id.into(),
            created_at: 42,
            deleted: false,
            auth: tokens
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .into(),
            cursor: None,
            payload: None,
        }
    }

    /// The REAL SSE wire text for one event — rendered by driving an actual
    /// `Sse` response to completion. Asserting on `Debug` output would not
    /// prove the `id:` line is (or isn't) on the wire; this does. Dropping
    /// the hub closes the broadcast channel, so the body terminates.
    /// Render one event as the bytes an SSE client would actually receive.
    ///
    /// Deliberately builds a bare `Sse` with NO `.keep_alive()`: a keep-alive
    /// makes the body infinite, so this helper would never return. Dropping
    /// the hub drops the only Sender, and tokio's broadcast contract is that
    /// a receiver drains buffered values and then sees Closed — which is what
    /// terminates the body here.
    async fn wire_frames(event: ChangeEvent) -> String {
        use axum::response::IntoResponse;
        let hub = ChangeHub::new(16);
        let stream = change_stream(hub.subscribe(), token_session(&["*".to_string()]), None);
        hub.publish(event);
        drop(hub);
        let body = Sse::new(stream).into_response().into_body();
        let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn delivers_visible_events_and_filters_invisible() {
        let hub = ChangeHub::new(16);
        let stream = change_stream(
            hub.subscribe(),
            token_session(&["farm-team".to_string()]),
            None,
        );
        tokio::pin!(stream);

        hub.publish(ev("hen", "visible", &["farm-team"]));
        hub.publish(ev("hen", "hidden", &["other-team"]));
        hub.publish(ev("hen", "public", &[]));

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        let texts = format!("{first:?}{second:?}");
        assert!(texts.contains("visible"));
        assert!(texts.contains("public"));
        assert!(!texts.contains("hidden"));
    }

    #[tokio::test]
    async fn entity_filter_drops_other_entities() {
        let hub = ChangeHub::new(16);
        let wanted: std::collections::HashSet<String> = ["hen".to_string()].into();
        let stream = change_stream(
            hub.subscribe(),
            token_session(&["*".to_string()]),
            Some(wanted),
        );
        tokio::pin!(stream);

        hub.publish(ev("farm", "nope", &[]));
        hub.publish(ev("hen", "yep", &[]));

        let first = stream.next().await.unwrap().unwrap();
        assert!(format!("{first:?}").contains("yep"));
    }

    #[tokio::test]
    async fn sse_id_is_the_resume_cursor_when_present() {
        let mut e = ev("hen", "abc", &[]);
        e.cursor = Some("0:7".into());
        let wire = wire_frames(e).await;

        assert!(
            wire.lines().any(|l| l == "id:0:7" || l == "id: 0:7"),
            "the SSE id must BE the cursor, got frames:\n{wire}"
        );
    }

    #[tokio::test]
    async fn sse_has_no_id_when_the_event_has_no_cursor() {
        // A tail-based source can't seek. Emitting any id would invite the
        // browser to send back a Last-Event-ID the server cannot honour.
        let wire = wire_frames(ev("hen", "abc", &[])).await;

        assert!(
            !wire.lines().any(|l| l.starts_with("id:")),
            "a cursor-less event must emit NO id line, got frames:\n{wire}"
        );
        // Sanity: the frame itself was actually emitted.
        assert!(
            wire.contains("event:change") || wire.contains("event: change"),
            "expected a change frame, got:\n{wire}"
        );
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_a_lagged_frame_then_close() {
        let hub = ChangeHub::new(2); // tiny buffer
        let rx = hub.subscribe();
        for i in 0..10 {
            hub.publish(ev("hen", &format!("e{i}"), &[]));
        }
        let stream = change_stream(rx, token_session(&["*".to_string()]), None);
        tokio::pin!(stream);

        // The drain is wrapped in a timeout because a regression here is a
        // *hang*, not runaway emission — removing the `done` guard makes the
        // stream poll forever. libtest has no per-test timeout, so without
        // this a broken implementation blocks CI indefinitely instead of
        // going red.
        let frames = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut frames = Vec::new();
            while let Some(item) = stream.next().await {
                frames.push(format!("{:?}", item.unwrap()));
                assert!(
                    frames.len() < 12,
                    "stream emitted far more frames than published"
                );
            }
            frames
        })
        .await
        .expect("stream must end, not hang");

        // The stream still closes (last frame is terminal)...
        let last = frames.last().expect("at least one frame");
        // ...but it announces the lag rather than vanishing silently.
        assert!(
            last.contains("lagged"),
            "final frame must be the lagged event, got: {last}"
        );
        // Assert the real count, not just the field's presence: buffer 2 with
        // 10 published means 8 dropped, and a hardcoded 0 would otherwise pass.
        assert!(
            last.contains(r#"skipped\":8"#) || last.contains(r#"skipped":8"#),
            "lagged frame must carry the true skipped count (8), got: {last}"
        );
    }
}

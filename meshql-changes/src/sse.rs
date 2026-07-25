//! The SSE surface: GET /changes streams thin change notifications.
//!
//! - `event: change`, `id:` = the event's resume cursor when it has one
//!   (omitted entirely for sources that can't seek, so a client never sends
//!   back a Last-Event-ID the server can't honour),
//!   `data:` = ChangeEvent::wire_json() (tokens stripped by construction).
//! - Per-subscriber filtering with the same token rule as the lettes
//!   (meshql_core::tokens_visible_to); tokens are captured once at connect.
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
use meshql_core::{tokens_visible_to, Auth, AuthContext};
use serde::Deserialize;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// The filtered notification stream for one subscriber. On broadcast lag it
/// emits a terminal `event: lagged` frame and then ends (closing the SSE
/// connection), so the client knows it must resync rather than guessing.
pub fn change_stream(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    subscriber_tokens: Vec<String>,
    entities: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
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
                    if !tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens) {
                        return Some(None);
                    }
                    let mut event = Event::default().event("change").data(ev.wire_json());
                    // The SSE id IS the resume cursor. Sources that can't seek
                    // emit no cursor and therefore no id, so a browser never
                    // sends back a Last-Event-ID the server can't honour.
                    if let Some(cursor) = &ev.cursor {
                        event = event.id(cursor);
                    }
                    Some(Some(Ok(event)))
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
    let tokens = state.auth.get_auth_token(&stash);
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

    Sse::new(change_stream(state.hub.subscribe(), tokens, entities)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeEvent;
    use tokio_stream::StreamExt;

    fn ev(entity: &str, id: &str, tokens: &[&str]) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(),
            id: id.into(),
            created_at: 42,
            deleted: false,
            authorized_tokens: tokens.iter().map(|s| s.to_string()).collect(),
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
        let stream = change_stream(hub.subscribe(), vec!["*".into()], None);
        hub.publish(event);
        drop(hub);
        let body = Sse::new(stream).into_response().into_body();
        let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn delivers_visible_events_and_filters_invisible() {
        let hub = ChangeHub::new(16);
        let stream = change_stream(hub.subscribe(), vec!["farm-team".into()], None);
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
        let stream = change_stream(hub.subscribe(), vec!["*".into()], Some(wanted));
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
        let stream = change_stream(rx, vec!["*".into()], None);
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

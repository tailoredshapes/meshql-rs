//! The SSE surface: GET /changes streams thin change notifications.
//!
//! - `event: change`, `id:` = the notification's created_at millis,
//!   `data:` = ChangeEvent::wire_json() (tokens stripped by construction).
//! - Per-subscriber filtering with the same token rule as the lettes
//!   (meshql_core::tokens_visible_to); tokens are captured once at connect.
//! - Reconnect contract: no replay. The hub is in-memory; on (re)connect a
//!   client must treat all cached state as stale. Last-Event-ID is ignored
//!   in v1 (a log-backed source may honor it later).
//! - Lag: a subscriber that overruns the broadcast buffer gets its stream
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

/// The filtered notification stream for one subscriber. Ends (closing the
/// SSE connection) on broadcast lag.
pub fn change_stream(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    subscriber_tokens: Vec<String>,
    entities: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx)
        .take_while(|item| !matches!(item, Err(BroadcastStreamRecvError::Lagged(_))))
        .filter_map(move |item| {
            let ev = item.expect("non-lag items are Ok");
            if let Some(wanted) = &entities {
                if !wanted.contains(&ev.entity) {
                    return None;
                }
            }
            if !tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens) {
                return None;
            }
            Some(Ok(Event::default()
                .event("change")
                .id(ev.created_at.to_string())
                .data(ev.wire_json())))
        })
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
    let entities = params.entities.map(|s| {
        s.split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect::<HashSet<_>>()
    });

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
        }
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
    async fn lagged_subscriber_stream_closes() {
        let hub = ChangeHub::new(2); // tiny buffer
        let rx = hub.subscribe();
        for i in 0..10 {
            hub.publish(ev("hen", &format!("e{i}"), &[]));
        }
        let stream = change_stream(rx, vec!["*".into()], None);
        tokio::pin!(stream);

        // Drain whatever survives; the stream must END (None), not hang.
        let mut n = 0;
        while let Some(item) = stream.next().await {
            assert!(item.is_ok());
            n += 1;
            assert!(n < 10, "expected lag closure before all 10");
        }
        // Reaching here = stream closed. Buffer is 2, so at most 2 delivered.
        assert!(n <= 2);
    }
}

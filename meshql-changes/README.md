# meshql-changes

Thin change notifications for meshql deployments, served over SSE.

A `ChangeSource` observes committed writes at the storage layer (the CDC
model: derived from the committed store, never the request path — no dual
write). `SearcherTail` is the portable poll-based source that works against
any certified `Searcher`+`Repository` pair; native change-stream sources
(merkql tail, Mongo change streams, Postgres LISTEN/NOTIFY) slot in behind
the same trait later. A `ChangeHub` broadcasts events to `GET /changes`,
which streams thin facts — `{entity, id, created_at, deleted}` — filtered
per subscriber by the same token rule as the lettes. Clients respond by
refetching through the normal graphlette: reads never bypass GraphQL, so
the CQRS, temporal, and authorization invariants are untouched.

## Wiring (in-process form)

```rust
use meshql_changes::{changes_router, run_tails, ChangeHub, ChangeSource, SearcherTail};

let hub = ChangeHub::new(256);
let sources: Vec<Arc<dyn ChangeSource>> = vec![
    Arc::new(SearcherTail::new("hen", hen_searcher.clone(), hen_repo.clone())),
    // ... one per entity ...
];
tokio::spawn(run_tails(hub.clone(), sources, Duration::from_millis(500)));

let extra = changes_router("/changes", hub, Arc::clone(&auth)); // same Auth as build_app_with_auth
meshql_server::run_ext(config, extra).await
```

The same pieces compose into a standalone sidecar binary attached to the
same storage — identical behavior, different deployment weight. See
`examples/egg-economy/src/main.rs` for a complete deployment (20 entities,
manifest route included).

## The client contract

- `event: change`, `id:` = the notification's `created_at` millis, `data:`
  = the thin JSON. `?entities=hen,farm` filters the stream.
- **On (re)connect, treat all cached state as stale.** The hub is
  in-memory: there is no replay, and `Last-Event-ID` is ignored in v1. A
  subscriber that falls behind the broadcast buffer has its stream closed
  rather than silently skipped — reconnect and refetch.
- Notifications are at-least-once; duplicates are harmless because the
  response to any notification is an idempotent refetch.

## Caveats

- A byte-identical rewrite, or a token-only ACL change with an identical
  payload, is invisible to the poll-based tail (see `src/tail.rs` docs).
  Note that `hash_row` deliberately excludes the opt-in `createdAt` field
  some searchers now inject (see the "honesty" section of the top-level
  skill/README) — that field changes on every write by definition, so
  hashing it would defeat this dedup entirely.

## Design

Spec: `docs/superpowers/specs/2026-07-07-meshql-changes-design.md`.
Deployment manifest spec (how clients discover `/changes`):
`schemas/README.md`.

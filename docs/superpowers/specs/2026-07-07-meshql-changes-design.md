# meshql-changes: change notifications and the deployment manifest

**Date:** 2026-07-07
**Status:** Approved design, pre-implementation
**Depends on:** nothing (no changes to meshql-core traits, adapters, lettes, or meshql-server)
**Unblocks:** the TypeScript client store (separate project), which consumes both deliverables

## Motivation

Frontend developers asked for a Redux-like client for meshql deployments: read
domain objects, send queries, submit events. That client (a separate project)
needs two things the server does not provide today:

1. **A way to learn when data changes.** Without push, the client must poll
   after every dispatched event to achieve read-your-writes — a workaround for
   a missing server capability.
2. **A way to learn the shape of a deployment** — its entities, endpoints, and
   schemas — so the client can auto-derive typed queries and event submitters
   with zero per-entity boilerplate.

This project delivers both:

- **`meshql-changes`**, a new workspace crate: thin change notifications over
  Server-Sent Events, fed by a storage-tailing change source.
- **`manifest.schema.json`**, a published document spec describing a
  deployment. A spec, not a feature: deployments serve a conforming static
  JSON however they like.

## Decisions and their reasons

| Decision | Alternatives rejected | Reason |
|---|---|---|
| Thin notifications (`entity, id, created_at, deleted`) | Full GraphQL subscriptions; envelope streams | Reads stay on the graphlette (CQRS, temporal, auth invariants untouched). Envelope streams would re-implement token filtering at the socket — the exact bug invariant 4 warns about. |
| SSE transport | WebSocket | Notifications are strictly one-way. SSE is plain HTTP: proxy-friendly, auto-reconnect and `Last-Event-ID` built into `EventSource`, trivial in axum. |
| Storage-tailing `ChangeSource` | Restlette `post_create` hook | The CDC argument, verbatim from `examples/egg-economy/src/source.rs`: a post-write hook is a dual write (crash between commit and publish loses the event). Deriving from the committed store guarantees at-least-once after commit, with the store's order and time. |
| Poll-based `SearcherTail` in v1 | Native change streams per backend | One portable impl works against the certified `Searcher` surface (see the Mongo wildcard caveat below). The trait is the seam; native impls (merkql tail, Mongo change streams, Postgres LISTEN/NOTIFY) slot in later — invariant 6, two scales of one seam. Polling is acknowledged as a first pass. |
| Per-subscriber token filtering from day one | Public-only v1 | An SSE stream is a read path; invariant 4 applies. Retrofitting filtering would change stream semantics under existing consumers. |
| No server-side replay on reconnect | Replay from storage via `Last-Event-ID` | Replay cannot reconstruct deletes (tombstones are invisible to a Searcher), so a replaying client could resurrect ghosts. Contract instead: on (re)connect, all cached state is stale. `Last-Event-ID` stays in the protocol for a future log-backed source to honor. |
| Manifest is a published schema + static document | Manifest endpoint auto-derived from `ServerConfig`; a `Manifest` builder API | `build_app` sees a slice of the deployment, not the deployment (MCP, search indexes, and sidecars never pass through `ServerConfig`). And the manifest is configuration-time data — static. The author declares it; anything can serve it. |

## Architecture

```
  writes                ┌─────────────────────────────────────────────┐
  POST /hen/api ──► store◄──poll── SearcherTail ──► ChangeHub ──► SSE route
                    (any          (ChangeSource     (broadcast)   GET /changes
                     backend)      impl)                 │        per-subscriber
                                                         │        token filter
                                                         ▼
                                               client EventSource
                                               {entity, id, created_at, deleted}
                                                         │
                                               refetch via /hen/graph
```

The crate has four pieces. It deliberately does **not** serve data, implement
GraphQL subscriptions, or touch the write path.

### `ChangeEvent`

```rust
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,          // epoch millis, the store's commit time
    pub deleted: bool,
    pub authorized_tokens: Vec<String>,  // internal: filtering only, never sent
}
```

### `ChangeSource` trait

Promotion of egg-economy's `EventSource` from example code to library:

```rust
#[async_trait]
pub trait ChangeSource: Send + Sync {
    fn entity(&self) -> &str;
    /// Changes committed since the last poll. At-least-once; consumers
    /// tolerate duplicates because the client response is an idempotent
    /// refetch.
    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>>;
}
```

### `SearcherTail` — the portable v1 impl

One `find_all("{}")` per poll with `["*"]` credentials, diffed against kept
state. Two facts about the search surface drive the design: `find_all`
returns the latest **non-deleted** version per id, and each row is
**payload + `id` only** — no Envelope metadata (`created_at` and
`authorized_tokens` are not in the row; verified across all five backends).
So:

| Change | Manifestation | Detection |
|---|---|---|
| Create | New `id` appears | `id` not in state map |
| Update | Same `id`, different payload (PUT appends a version) | Payload hash ≠ hash in state map |
| Delete | `id` disappears (tombstone is filtered out) | Presence diff against state map |

State per entity: `id → (payload_hash, last_known_tokens)`. The hash is over
a deterministic serialization of the payload row. Memory cost is one entry
per live envelope: acceptable at the in-process scale, absent entirely in a
native change-stream impl. Known blind spot: a PUT with a byte-identical
payload produces no observable change and is not notified — acceptable,
since no refetch would show anything new.

**Envelope metadata recovery:** because the row carries neither `created_at`
nor `authorized_tokens`, `SearcherTail` takes both a `Searcher`
(listing/diffing) and a `Repository`: for each envelope the diff marks as
created or updated, one point `read` fetches the full Envelope for its
commit `created_at` and tokens — a handful of reads per poll, not N+1 over
the table. For deletes there is no envelope to read; the tail uses the last
known tokens from the state map (the people who could see the envelope are
the ones who should learn it is gone) and the poll's wall-clock as
`created_at`. Race edge: an envelope updated then deleted between polls
diffs as changed but its point `read` returns nothing — emit the delete
notification in that case (cert suite covers it).

**Backend caveat:** the `["*"]`-credential poll relies on searchers honoring
the `meshql-core::auth` convention that a caller holding `"*"` sees
everything. The SQL, merkql, and sqlite searchers post-filter via
`envelope_visible_to`, which implements this; the Mongo searcher instead
filters `authorizedTokens $in [creds]` in the query with no wildcard
special-case, so under real auth a `["*"]` poll would silently miss
envelopes. That is a pre-existing Mongo adapter inconsistency with the core
convention, tracked as a separate fix; until it lands, `SearcherTail` on
Mongo is correct only for `NoAuth` deployments.

**Delivery contract:** at-least-once, per-entity ordered by `created_at`.
Duplicates are harmless by design.

**Runner:** `run_tails(hub, sources, interval)` polls sources round-robin —
the shape of egg-economy's `run_connector`. Poll errors are logged and
retried next interval, never fatal.

### `ChangeHub` and the SSE route

`ChangeHub` wraps a `tokio::sync::broadcast` channel: the tail task publishes,
each SSE connection subscribes.

**Endpoint:** `GET /changes`, one stream per client, multiplexing all
entities. Optional `entities=hen,farm` query param filters; omitted means all.

**Wire format:**

```
event: change
id: 1751892345123
data: {"entity":"hen","id":"abc-123","created_at":1751892345123,"deleted":false}

: heartbeat
```

- `event: change` is the only event type in v1.
- `id:` is the notification's `created_at`, so `EventSource` sends
  `Last-Event-ID` on reconnect automatically. The server ignores it in v1
  (see reconnect contract); the field exists for a future log-backed source.
- `data:` is the thin notification with tokens stripped. Nothing in it is
  invisible to this subscriber through GraphQL.
- Comment-line heartbeat every ~15s keeps proxies from idling the connection.

**Auth:** the handler uses the identical mechanism as the lettes — the
request-scoped `AuthContext` extension populated by edge middleware, passed to
`Auth::get_auth_token` (the route is therefore constructed with the same
`Arc<dyn Auth>` the author passes to `build_app_with_auth`). Tokens are
captured once at connect time; every notification is checked with the same
token-overlap rule as `envelope_visible_to` — extracted into a shared helper
in `meshql-core` that both call, so the visibility logic is written once.
Revocation takes effect on next reconnect — the same freshness model as a
long-lived JWT.

**Reconnect contract:** the hub is in-memory; events during a disconnect are
gone. On (re)connect the client must treat all cached state as stale and
refetch its active queries. The same rule covers lag: a subscriber that falls
behind the broadcast buffer gets its stream **closed** (never silent drops),
forcing the reconnect-refetch path. Slow clients get correctness, not gaps.

**Deployment forms** (invariant 6, same code, two weights):
- in-process: merge the router into the main binary via the existing `run_ext`;
- sidecar: a standalone binary composing tail + hub + route, attached to the
  same storage.

## The deployment manifest

**Deliverable: `schemas/manifest.schema.json`** — a JSON Schema at the repo
root, versioned via its `$id` (`…/manifest-v1.schema.json`); breaking changes
ship as a new `-v2` file, and manifest documents declare which they conform
to via the `meshql` field. Not an endpoint, not a builder API. Deployments serve a conforming
document however they like: hand-written and committed next to
`config/graph/` and `config/json/` (it is config; it lives with config),
served by a one-line `run_ext` static route, nginx, S3, or the sidecar.
Clients are constructed with a manifest URL.

```json
{
  "meshql": "0.x",
  "entities": {
    "hen": {
      "surfaces": {
        "graph": { "kind": "graphql", "path": "/hen/graph", "schema": "type Hen {...}" },
        "api":   { "kind": "rest",    "path": "/hen/api",   "schema": { "type": "object" } }
      }
    }
  },
  "surfaces": {
    "changes":     { "kind": "sse", "path": "/changes" },
    "catalog-mcp": { "kind": "mcp", "transport": "stdio" },
    "search":      { "kind": "elastic", "config": { "index": "hens" } }
  }
}
```

- `kind` is an open string. `graphql`, `rest`, and `sse` are the kinds the
  TS client understands; unknown kinds pass through for other tooling. The
  manifest describes; consumers pick what they can use.
- The author declares the whole document — including surfaces `ServerConfig`
  never sees (MCP, search indexes, sidecars). Honest by construction.
- Absence of a `changes` surface tells a client to degrade to
  refetch-on-dispatch.
- Optional convenience: a `manifest_json(&config)` free function emitting the
  graph/api portion from a `ServerConfig`, for authors who prefer generating
  to hand-writing. The contract is the document, not the function.

**Drift is the one real risk** — a static file lies when someone adds an
entity and forgets it. Mitigations:
1. Clients fail loudly: a manifest naming a query that 404s or a schema that
   does not parse surfaces immediately.
2. CI validates each example's manifest against `manifest.schema.json` *and*
   asserts every graphlette/restlette in the example's `ServerConfig` appears
   in its manifest. Drift breaks CI, not production.

## Testing

Three layers, matching how the workspace already tests:

1. **`ChangeSource` certification** (invariant 5): a reusable suite in
   `meshql-cert` style. Drive a repository through creates, updates, and
   deletes; assert the source emits the right `ChangeEvent`s (create, update
   via changed payload, delete via disappearance, tokens carried, duplicates
   tolerated, update-then-delete between polls yields a delete). `SearcherTail`
   passes it against in-memory SQLite in v1; any future native impl must pass
   the same suite before merging.
2. **SSE integration tests** in `meshql-changes`: real axum app, tail + hub +
   route over a real repository, consumed by a test client. Cases:
   notification after write; `deleted: true` after DELETE; per-subscriber
   filtering (subscriber A sees the envelope, B does not); heartbeats;
   lagged-consumer stream closure; `entities=` filtering.
3. **Manifest conformance in CI**: schema validation plus the drift test
   above, for each example that ships a manifest.

## Rollout order

1. `manifest.schema.json` + conformance/drift tests — unblocks the TS client's
   config work immediately, independent of the crate.
2. `meshql-changes` crate: `ChangeEvent`, `ChangeSource`, cert suite.
3. `SearcherTail` (passes cert).
4. `ChangeHub` + SSE router + integration tests.
5. Wire into `examples/egg-economy` (manifest + `/changes` via `run_ext`) —
   the living reference for both deployment forms.
6. TS client project begins, consuming the manifest and the stream.

## Out of scope

- GraphQL subscriptions (`subscription { ... }` operations).
- Server-side replay of missed notifications.
- Native change-stream `ChangeSource` impls (merkql tail, Mongo change
  streams, Postgres LISTEN/NOTIFY) — future work behind the existing trait.
- The TypeScript client itself — separate project, separate spec. Decisions
  already made for it: vanilla TS core only, query-keyed cache,
  auto-derivation from the manifest, invalidation driven by this stream.

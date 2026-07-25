# Streamlette: a per-meshlette SSE subscription surface

**Date:** 2026-07-24
**Status:** Approved design, pre-implementation
**Depends on:** `meshql-changes` (`ChangeHub`, `ChangeEvent`, `change_stream`, `SearcherTail`, `changes_router`) on `meshql-rs` main. Also depends on the canonical result-ordering work (`5d5cbdb`) — the resume cursor and backfill dedupe below are only coherent because every adapter now has a defined total order. And on the Repository auth fix (`e823ce0`), which made `authorized_tokens` filtering trustworthy on every read path; this design reuses that same envelope-level rule rather than inventing a second authorization surface.
**Unblocks:** real-time delivery for `teamchat` (currently polls, which is the single gap disqualifying it as a chat product) and for every later product.

## Motivation

`teamchat` polls. The worker re-reads every event from offset zero each tick and the UI refetches a channel's whole history each poll — the build's own architecture doc names this "the clearest scalability limit in the build." That wasn't a builder mistake: real-time was explicitly scoped out of its brief, because `meshql-iron` doesn't teach SSE consumption and inventing an integration would have been worse than not having one.

But the backend capability substantially exists. `meshql-changes` already ships `ChangeEvent`, a `ChangeHub` over `tokio::sync::broadcast`, `change_stream` (already filtering by `tokens_visible_to` *and* by entity), `SearcherTail` for poll-based CDC, and an axum SSE route. What's missing is packaging: `changes_router` mounts **one deployment-level `/changes`** feed, not a surface an entity declares for itself the way it declares a restlette or graphlette.

This project closes that gap: **a third surface type, `streamlette`, declared per meshlette.** It is the enabling fix for real-time in every product, not a teamchat patch.

## Non-Goals

- **Java (`meshql`) and TypeScript (`meshobj`) parity.** A separate project. Note per `merkql-architecture` that a Java/TS deployment streaming from a merkql-backed entity needs the Rust connector sidecar regardless, so their parity story is genuinely different, not a copy.
- **A new transport.** SSE only. The name `streamlette` is deliberately transport-agnostic so a future WebSocket transport doesn't force a rename, but nothing here builds one.
- **A second read path for querying.** A streamlette delivers change notifications (optionally with the changed payload — see below). It is not a query surface: no filtering by predicate, no field selection, no `at`. Those are the graphlette's job.
- **Retrofitting `teamchat` in this project.** This ships the capability and the guidance; converting `teamchat` off polling is separate follow-on work.
- **Adding a seek API to `merkql`.** See "Resume" — v1 deliberately works within merkql's current consumer API so this project needs no change to a separate, tag-pinned repo.

## Naming

`restlette` = REST + *lette*; `graphlette` = graph + *lette*. SSE yields `ssette`, which is unpronounceable. **`streamlette`**, mounted at `/{entity}/stream`, named for the capability rather than the transport. The manifest surface `kind` stays `"sse"` — that describes the wire protocol accurately, and the manifest schema already lists `'sse'` as a understood kind (`schemas/manifest.schema.json`), so no schema change is needed to advertise one.

## Configuration

A third vec on `ServerConfig`, beside the two that exist today (`meshql-core/src/config.rs:177`):

```rust
pub struct ServerConfig {
    pub port: u16,
    pub graphlettes: Vec<GraphletteConfig>,
    pub restlettes: Vec<RestletteConfig>,
    pub streamlettes: Vec<StreamletteConfig>,   // new
}

pub struct StreamletteConfig {
    pub path: String,                  // "/message_posted/stream"
    pub entity: String,                // the ChangeEvent.entity this stream carries
    pub source: StreamSource,
    pub include_payload: bool,
    pub searcher: Arc<dyn Searcher>,   // backfill on resume; also lets the surface self-describe
}

pub enum StreamSource {
    /// merkql *is* the log: subscribe the topic directly, no polling.
    MerkqlTopic { broker: BrokerRef, topic: String },
    /// Poll-diff an existing store (Mongo, Postgres, …) via the existing SearcherTail.
    Tail { searcher: Arc<dyn Searcher>, repository: Arc<dyn Repository>, interval: Duration },
}
```

`build_app*` mounts each streamlette, exactly as it already mounts graphlettes. Adding a field to `ServerConfig` is a breaking change for anyone constructing it literally — every in-repo construction site (examples, tests, the `cms`/`teamchat` products) needs updating, and `StreamletteConfig`/`ServerConfig` should get a builder or `Default` so a deployment with no streams isn't forced to write `streamlettes: vec![]`. Confirm which during implementation; prefer whatever keeps existing call sites compiling.

## Two sources, one contract

Both variants feed a **per-meshlette `ChangeHub`**; everything downstream of the hub is identical, which is what keeps behavior certifiable across backends.

- **`MerkqlTopic`** — a `merkql::consumer` subscribed to the entity's topic. No `SearcherTail`, no polling: a restlette `POST` against a merkql-backed entity already *is* the log append (see `domain-design.md`'s note on this), so the log is the change feed. This is the low-latency path.
- **`Tail`** — wraps the existing, already-certified `SearcherTail`, which diffs `find_all` by payload hash and recovers the Envelope via a point `Repository::read`. Higher latency (bounded by `interval`), works on every backend.

Per invariant 6 ("pick your scale"), having both is the design working, not a compromise — same contract, different deployment weight.

## Gap handling

Three layers, because SSE gaps have three distinct causes.

**1. Fix the silent-lag bug (prerequisite, not optional).** `change_stream` currently does `.take_while(|item| !matches!(item, Err(BroadcastStreamRecvError::Lagged(_))))` (`meshql-changes/src/sse.rs:36`) — on broadcast lag it *silently terminates the stream*. The browser auto-reconnects, so it looks like it works, but the client is never told it missed events and has no way to learn what. For chat that is silently dropped messages. Replace with an explicit `event: lagged` frame (carrying the skipped count `tokio` gives us) before closing, so a client can always distinguish "stream ended, resync" from "stream ended, nothing missed."

**2. Live-only baseline + fetch-on-connect.** The universally-correct mode, available on every backend. A client fetches current state via the graphlette on connect *and on every reconnect*, then streams. Gaps heal by refetch, so the server needs no history. This is the honesty pattern extended to streams and `meshql-iron` must teach it as the default.

**3. `Last-Event-ID` resume, where the source can seek.** SSE's standard reconnect header. The event `id:` field carries a cursor; on reconnect the server backfills from it, then hands over to live.

- **Cursor format:** for `MerkqlTopic`, `{partition}:{offset}` — `merkql::record::Record` carries both. For `Tail`, no cursor is emitted (no seekable position), so resume is simply unavailable and the client falls back to layer 2.
- **The handover race:** subscribe to the hub **before** reading history, buffer live events during backfill, then emit history followed by the buffer, deduping by the canonical order key. This is only coherent because `5d5cbdb` gave every adapter a defined total order — without it there is no stable notion of "already seen."
- **v1 seek mechanism:** merkql's consumer has **no seek API** — only `OffsetReset::{Earliest, Latest}` at subscribe time (`merkql/src/consumer.rs`). So v1 subscribes from `Earliest` and skips records at-or-before the cursor. Correct, and needs no change to `merkql` (a separate repo pinned at `v0.2.0`). It re-reads the log prefix on every resume, which is acceptable at the volumes this targets and is the documented cost. **Adding a real seek to merkql is the obvious efficiency follow-up** and would drop in behind the same cursor format.

**Capability is advertised, never assumed.** The manifest's `sse` surface declares whether resume is supported, so a client reads it rather than guessing. That resolves the tension with "behavior is certified identical across adapters": the *contract* is identical where a capability exists, and the manifest is how a consumer discovers which exist — which is exactly the manifest's existing job.

## Payload

`include_payload` per streamlette, keyed to the event-vs-projection distinction that is already central to this architecture:

- **Event meshes → `true`.** An immutable fact *is* its content. Being told `message_posted/abc happened` and then refetching it is pure waste, and on a busy channel it turns one message into N refetches from N subscribers.
- **Projections → `false`.** A projection may already have been superseded by the time a client renders it; notification-only forces a read through the graphlette, which returns current state. This is the same reasoning behind the honesty pattern.

**No new authorization surface.** `change_stream` already filters each event by `tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens)`, and envelope-level visibility is precisely what gates a read — so a subscriber allowed the event is, by construction, allowed its payload. `ChangeEvent`'s existing split between the internal struct and the serialized `WireEvent` (which omits `authorized_tokens` by construction, `meshql-changes/src/event.rs`) must be preserved: tokens are an input to filtering, never output on the wire.

One open detail for implementation: subscriber tokens are currently captured **once at connect** (documented in `sse.rs`'s header comment). A long-lived stream therefore won't observe a mid-connection privilege change — someone banned from a channel keeps receiving it until they reconnect. For v1 this is acceptable and should be *documented* rather than silently shipped; note it in the streaming reference doc, since a product with hard revocation requirements (teamchat's ban semantics are exactly this) needs to know to force a disconnect on revocation.

## `meshql-iron`: `references/streaming.md`

The consumption gap is why `teamchat` polls, so the skill work is not optional follow-up — it ships with the capability. A new reference doc covering: discover the `sse` surface (and whether it advertises resume) from the manifest; `EventSource` subscribe; **fetch-on-connect, and again on every reconnect**; handle the `lagged` event by resyncing; use `Last-Event-ID` only where advertised; and when *not* to stream (a page that reads once doesn't need a subscription). `SKILL.md`'s decision guide and its "Non-goals — don't reach for these" section both currently tell agents SSE is out of scope; both need updating, or the new doc will be contradicted by the file that points at it.

## Testing

- **Unit:** the lagged frame is emitted (not silently swallowed); token filtering excludes invisible events; `include_payload` toggles payload presence; `WireEvent` never serializes `authorized_tokens`.
- **Cursor/resume:** backfill returns exactly the events after a cursor; the handover emits no duplicates and drops nothing when events arrive *during* backfill (the race above — test it deliberately, with a write injected mid-backfill); a `Tail`-sourced stream advertises no resume and rejects/ignores `Last-Event-ID` rather than half-honouring it.
- **Certification, across sources:** the same assertions run against both `MerkqlTopic` and `Tail`, following the `repo_auth_cert.rs` / `searcher_ordering_cert.rs` pattern established this week — a shared suite in `meshql-changes/src/testing.rs` (the crate already has a `testing` module) invoked per source.
- **Break-it verification:** per the practice this project has settled into, deliberately break the lag frame and the dedupe and confirm the certs go red before shipping. Both prior fixes this week (`e823ce0`, `5d5cbdb`) shipped only after this check, and in both cases the pre-existing suite stayed green — which is the whole reason the check exists.
- **End-to-end:** a real server with a streamlette, a real `EventSource`-equivalent client, a write, and an assertion the event arrives without polling.

## Summary of what this design intentionally leaves open

- **A real seek API in `merkql`** — v1 skips from `Earliest` instead; the cursor format is chosen so a seek drops in behind it unchanged.
- **Mid-connection privilege changes** — tokens are captured at connect; documented, not solved. Products needing hard revocation must force a disconnect.
- **Java/TS parity** — separate project, genuinely different because of merkql's Rust-only constraint.
- **Converting `teamchat` off polling** — follow-on work once this lands; it is the natural first real consumer and therefore the real test of whether the guidance is sufficient.
- **Whether `ServerConfig` grows a builder** to avoid the breaking-change churn of a new field — an implementation call, resolved by whatever keeps existing call sites compiling.

# Streamlette: a per-meshlette SSE subscription surface

**Date:** 2026-07-24 (revised same day after spec review — see "Revision history")
**Status:** Approved design, pre-implementation
**Depends on:** `meshql-changes` (`ChangeHub`, `ChangeEvent`, `change_stream`, `SearcherTail`, `ChangeSource`, `changes_router`) on `meshql-rs` main. Also depends on the Repository auth fix (`e823ce0`), which made `authorized_tokens` filtering trustworthy on every read path; this design reuses that same envelope-level rule rather than inventing a second authorization surface.
**Unblocks:** real-time delivery for `teamchat` (currently polls, which is the single gap disqualifying it as a chat product) and for every later product.

## Motivation

`teamchat` polls. The worker re-reads every event from offset zero each tick and the UI refetches a channel's whole history each poll — the build's own architecture doc names this "the clearest scalability limit in the build." That wasn't a builder mistake: real-time was explicitly scoped out of its brief, because `meshql-iron` doesn't teach SSE consumption and inventing an integration would have been worse than not having one.

But the backend capability substantially exists. `meshql-changes` already ships `ChangeEvent`, a `ChangeHub` over `tokio::sync::broadcast`, `change_stream` (already filtering by `tokens_visible_to` *and* by entity), `SearcherTail` for poll-based CDC, a `ChangeSource` trait with a cert suite in `meshql-changes/src/testing.rs`, and an axum SSE route. What's missing is packaging: `changes_router` mounts **one deployment-level `/changes`** feed, not a surface an entity declares for itself the way it declares a restlette or graphlette.

This project closes that gap: **a third surface type, `streamlette`, declared per meshlette.** It is the enabling fix for real-time in every product, not a teamchat patch.

## Non-Goals

- **Java (`meshql`) and TypeScript (`meshobj`) parity.** A separate project. Note per `merkql-architecture` that a Java/TS deployment streaming from a merkql-backed entity needs the Rust connector sidecar regardless, so their parity story is genuinely different, not a copy.
- **A new transport.** SSE only. The name `streamlette` is deliberately transport-agnostic so a future WebSocket transport doesn't force a rename, but nothing here builds one.
- **A second read path for querying.** A streamlette delivers change notifications (optionally with the changed payload). It is not a query surface: no filtering by predicate, no field selection, no `at`. Those are the graphlette's job.
- **Retrofitting `teamchat` off polling.** This ships the capability and the guidance; converting `teamchat` is follow-on work, and is the real test of whether the guidance is sufficient.
- **Adding a seek API to `merkql`.** `merkql` is a separate repo pinned at `tag = "v0.2.0"`. See "Resume" — v1 works within its current consumer API deliberately, so this project needs no cross-repo change. Do not modify `/tank/repos/tailoredshapes/merkql/`.
- **Removing the deployment-level `/changes` route.** `changes_router` survives unchanged; it serves cross-entity consumers a per-entity surface can't. Both paths call `change_stream`, so the lag fix below benefits both.

## Naming

`restlette` = REST + *lette*; `graphlette` = graph + *lette*. SSE yields `ssette`, which is unpronounceable. **`streamlette`**, mounted at `/{entity}/stream`, named for the capability rather than the transport. The manifest surface `kind` stays `"sse"` — that describes the wire protocol accurately, and the manifest schema already lists `'sse'` as understood (`schemas/manifest.schema.json:38`) with `$defs/surface` set `additionalProperties: true`, so advertising a resume flag needs no schema change.

## Configuration

**Streamlettes are passed to `meshql-server`, not stored on `ServerConfig`.** An earlier draft of this section put a third vec on `ServerConfig` (`meshql-core/src/config.rs`, exactly `port`/`graphlettes`/`restlettes`). **That is not buildable.** `StreamletteConfig` lives in `meshql-changes`, which depends on `meshql-core` — so a `meshql-core::ServerConfig` naming it would be a dependency cycle (`meshql-core → meshql-changes → meshql-core`).

The resolution is an entry point on `meshql-server`, which depends on `meshql-core`/`meshql-graphlette`/`meshql-restlette` and *not* on `meshql-changes`, while `meshql-changes` does not depend on `meshql-server`. Adding `meshql-server → meshql-changes` therefore creates no cycle:

```rust
pub async fn build_app_with_streams(
    config: ServerConfig,          // unchanged — meshql-core is untouched
    extra: Router,
    streamlettes: Vec<StreamletteConfig>,
    auth: Arc<dyn Auth>,
) -> anyhow::Result<Router>
```

It is a *new* function, so every existing caller of `build_app`/`build_app_ext`/`build_app_with_auth` keeps compiling. It creates one `ChangeHub` and spawns one `run_pump` task per streamlette, merges each `streamlette_router` into `extra`, then delegates to **`build_app_with_auth`** — never to `build_app_ext`, which hardcodes `NoAuth` and would hand the streamlettes the caller's real `Auth` while every restlette and graphlette silently ran unauthenticated. That split-auth hole has its own regression test (`meshql-server/tests/streamlette_integration.rs`).

```rust
pub struct StreamletteConfig {
    pub path: String,      // "/message_posted/stream"
    pub entity: String,    // the ChangeEvent.entity this stream carries
    pub source: StreamSource,
}

pub enum StreamSource {
    /// merkql is itself the log: consume the topic. Supports resume, and may
    /// carry payloads (see "Payload" for why that pairing is not accidental).
    /// The topic MUST be single-partition — see "Resume".
    MerkqlTopic {
        broker: BrokerRef,
        topic: String,
        poll_interval: Duration,
        include_payload: bool,
    },
    /// Poll-diff an existing store (Mongo, Postgres, …) via the existing
    /// SearcherTail. No resume, no payload.
    Tail {
        searcher: Arc<dyn Searcher>,
        repository: Arc<dyn Repository>,
        poll_interval: Duration,
    },
}
```

Both variants name the field `poll_interval`: both genuinely poll (see "Two sources"), and two differently-named interval fields would imply a distinction that doesn't exist.

Two deliberate shapes here, both from review findings:

- **`include_payload` lives on `MerkqlTopic`, not on `StreamletteConfig`** — this makes the invalid combination unrepresentable rather than merely discouraged. See "Payload."
- **No `searcher` field on `StreamletteConfig`.** An earlier draft had one "for backfill on resume"; backfill comes from the log instead (see "Resume"), so it would be dead weight.

`StreamletteConfig` also carries `hub_capacity`, resolving the open question under "Payload" about where broadcast capacity lives: it is a config field, not a `StreamSource` field, because both variants have subscribers and both can lag.

There is **no breaking-change churn**. Because `ServerConfig` is untouched, none of its ~20 in-repo construction sites change, and neither do the out-of-repo `cms` and `teamchat` products (which pin `v0.1.0`). A deployment with no streams calls the existing builders and never mentions streamlettes at all.

## Two sources, one contract

Both variants implement the existing `ChangeSource` trait and feed a **per-meshlette `ChangeHub`**; everything downstream of the hub is identical, which is what keeps the *stream's* behavior — filtering, `ready`, lag, entity scoping — identical across backends. It does **not** make the two sources interchangeable at the cert level: see "Testing" for why the shared `ChangeSource` cert suite stays a `Tail`-only suite.

- **`MerkqlTopic`** — a `merkql::consumer::Consumer` subscribed to the entity's topic. **This polls.** `Consumer::poll(&mut self, _timeout)` is synchronous, takes `&mut self`, and *ignores its timeout argument entirely*, returning immediately (`merkql/src/consumer.rs:116`) — there is no blocking wait and no subscribe-with-callback. Do **not** write a zero-interval busy loop.

  What makes it the low-latency path is not the absence of polling but the cost per poll: an in-memory offset comparison, versus `SearcherTail`'s full `find_all` plus payload-hash diff. It can therefore be polled far more aggressively.

  **The topic must be pre-created at startup.** `Consumer::subscribe` *silently skips* topics the broker can't find (`merkql/src/consumer.rs:80-81`), and `positions` is only ever populated inside `subscribe` — so a consumer that subscribes before the entity's first write never delivers anything, **permanently, even after the topic later appears**. merkql creates topics lazily on produce, and a streamlette's pump starts at boot, which is exactly when a fresh deployment has no topic yet. That is a silent dead stream on day one of every new entity. Call `create_topic` (idempotent) at streamlette construction; do not rely on lazy creation. Cover it with a test that boots a streamlette *before* any write and asserts the first subsequent write is delivered.

- **`Tail`** — wraps the existing, already-certified `SearcherTail`. Higher latency, works on every backend.

**One pump task per streamlette, not the shared `run_tails` round-robin.** `meshql_changes::run_tails(hub, sources, interval)` (`hub.rs:41`) is a single task owning *one* interval across all its sources. That model cannot deliver what this design needs: a slow `SearcherTail::find_all` would block the aggressive merkql poll behind it, so per-source intervals would be inert fields. Each streamlette therefore spawns its own pump over its own source at its own `poll_interval`. `run_tails` stays exactly as it is for the deployment-level `/changes` feed — this adds a pump model, it doesn't replace one.

Per invariant 6 ("pick your scale"), having both sources is the design working, not a compromise — same contract, different deployment weight.

## `ChangeEvent` gains two optional fields

This is a real, if small, breaking change to a public struct, and it has existing tests asserting its shape (`meshql-changes/src/event.rs`, plus the no-leak tests). It is not free, and the plan should treat it as a deliberate step.

```rust
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
    pub authorized_tokens: Vec<String>,   // filtering input; never serialized
    pub cursor: Option<String>,           // NEW: "{partition}:{offset}", MerkqlTopic only
    pub payload: Option<serde_json::Value>, // NEW: MerkqlTopic + include_payload only
}
```

`WireEvent` gains `cursor` and `payload` and **must continue to omit `authorized_tokens`** — the existing split exists precisely so tokens can't leak, and a test already asserts that (`wire_json_never_leaks_tokens`), and `meshql-changes/tests/sse_integration.rs` asserts the wire shape. Keep them passing.

`change_stream` currently sets SSE's `id:` to `ev.created_at.to_string()` (`sse.rs:49`); it becomes `ev.cursor`, omitted when `None`.

## Resume

**Cursor = merkql's `{partition}:{offset}`.** `merkql::record::Record` carries both (`merkql/src/record.rs`), so it is exactly expressible, and it is *exact* — which is why dedupe uses it rather than the canonical `(created_at, id)` order key.

**A streamlette-backed topic MUST be single-partition, asserted loudly at construction.** A single `Last-Event-ID` cannot address a multi-partition topic: merkql topics can have N partitions (`create_topic(name, num_partitions)`, producer routes by key hash, `merkql/src/topic.rs:147-158`), and `Consumer::poll` interleaves partition batches in `HashMap` iteration order. With N>1, "skip records at-or-before the cursor" is meaningful only *within the cursor's own partition* — history in other partitions is silently dropped or duplicated, and the claim below that "the log has everything, in exact order" stops being true. merkql's `default_partitions: 1` means the happy path works and this bug hides indefinitely. So: fail construction with a clear error if `num_partitions != 1`, rather than degrade. Multi-partition support means a per-partition cursor vector and is deliberately out of scope (see "Summary of what this design leaves open").

**An earlier draft was wrong about this, and the correction matters.** That draft claimed resume depends on the canonical-ordering work (`5d5cbdb`) and would dedupe by its order key. It can't: `created_at` is millisecond-precision and `5d5cbdb` explicitly acknowledges ties are real, so two versions of one record committed in the same millisecond yield byte-identical `ChangeEvent`s — indistinguishable for dedupe. The log offset has no such ambiguity. `5d5cbdb` is therefore *not* a dependency of this design; it was listed as one in error.

**Backfill reads the log, not a searcher.** A searcher returns the latest version per id and excludes tombstones, so it cannot replay intermediate versions or deletes — wrong for a change feed. The log has everything, in exact order.

**v1 seek mechanism.** merkql has no seek API — only `OffsetReset::{Earliest, Latest}` at subscribe time. So v1 subscribes from `Earliest` and skips records at-or-before the cursor. Two constraints:

- **A fresh `group_id` per connection is mandatory.** `Consumer::subscribe` prefers a *committed* offset over `offset_reset` unconditionally (`merkql/src/consumer.rs:88-104`), so `OffsetReset::Earliest` applies only when no committed offset exists. Reusing a `group_id` yields a stream that looks healthy but starts from the wrong position — silent, and exactly the kind of bug that survives a happy-path test. `meshql-merkql` already uses fresh-UUID group ids; follow it.
- Re-reading the log prefix on every resume is the accepted cost at these volumes. **A real seek in `merkql` is the obvious efficiency follow-up** and drops in behind the same cursor format unchanged.

**An unusable cursor degrades to `resume: false` — never to a silent skip.** `Last-Event-ID` is untrusted client input, and every failure mode here otherwise produces "resume looked fine, events missing," which is the exact failure class layer 1 exists to eliminate. Malformed cursor; a partition that isn't 0; an offset beyond the log tail (which would skip everything and yield a silently empty stream); an offset below the retention floor (`Partition::advance_retention`/`read_range` honour retention, `merkql/src/partition.rs:630-680` — opt-in via `RetentionConfig.max_records`, default `None`, so latent today rather than active). In all cases: don't resume, report `resume: false` in the `ready` frame, and let the client fall back to fetch-on-connect. Test each case.

**`Tail` sources emit no cursor and do not support resume.** A client sending `Last-Event-ID` to one is a client bug (the manifest told it resume was unavailable), but SSE's auto-reconnect makes a `400` actively harmful — the browser would reconnect in a loop. So: **ignore the header, and always emit a first `event: ready` frame declaring the mode actually honoured.**

```
event: ready
data: {"resume": true, "cursor": "0:1841"}
```

`resume` is whether `Last-Event-ID` was honoured. `cursor` is **the position resume actually started from** — the client's own cursor when honoured, and `null` whenever `resume` is `false` (live-only source, no header sent, or an unusable cursor per above). It is deliberately *not* the current log tail: a client comparing it against what it sent can detect that its cursor was rejected. The `ready` frame carries no SSE `id:`, so it cannot clobber the browser's `Last-Event-ID` tracking.

**`ready` lives in the streamlette handler, not in `change_stream`.** Both the streamlette and the deployment-level `/changes` route call `change_stream`; emitting `ready` inside it would silently change `/changes`'s wire contract for existing consumers, including meshobj's TS client. Keep `change_stream` a pure event stream and prepend `ready` in the streamlette handler.

## Gap handling

Three layers, because SSE gaps have three distinct causes.

**1. Fix the silent-lag bug (prerequisite, not optional).** `change_stream` currently does `.take_while(|item| !matches!(item, Err(BroadcastStreamRecvError::Lagged(_))))` (`meshql-changes/src/sse.rs:36`) — on broadcast lag it *silently terminates the stream*. The browser auto-reconnects, so it looks like it works, but the client is never told it missed events. For chat that is silently dropped messages. Emit an explicit `event: lagged` frame carrying the skipped count `tokio` provides, then close. An existing test (`lagged_subscriber_stream_closes`) locks in today's behavior and must be updated, not deleted — the stream still closes, it just says why first.

**2. Live-only baseline + fetch-on-connect.** The universally-correct mode, available on every backend and on both sources. A client fetches current state via the graphlette on connect *and on every reconnect*, then streams. Gaps heal by refetch, so the server needs no history. This is the honesty pattern extended to streams, and `meshql-iron` must teach it as the default.

**3. `Last-Event-ID` resume** on `MerkqlTopic`, per above.

**The backfill→live handover.** Subscribe to the hub **before** reading history, buffer live events during backfill, then emit history followed by the buffer, deduping by `cursor`. **Lag during backfill** is possible — the buffer is a bounded broadcast receiver and a slow backfill can overrun it — and resolves the same way as any other lag: emit `lagged`, close, and the client falls back to layer 2. Test this deliberately, with a write injected mid-backfill.

**Capability is advertised, never assumed.** See "Manifest" below.

## Payload

`include_payload` on `MerkqlTopic` only, keyed to the event-vs-projection distinction already central to this architecture:

- **Event meshes → `true`.** An immutable fact *is* its content. Being told `message_posted/abc happened` and then refetching it is waste, and on a busy channel it turns one message into N refetches from N subscribers.
- **Projections → `false`.** A projection may already have been superseded by the time a client renders it; notification-only forces a read through the graphlette, which returns current state.

**Why `Tail` can never carry payloads.** `SearcherTail`'s own documentation (`meshql-changes/src/tail.rs:10-18`) records that a token-only ACL change with an identical payload is *undetectable* to it, and that **stale tokens are retained** until the next payload change or delete. So on a `Tail` stream the claim "a subscriber allowed the event is by construction allowed its payload" is not airtight — the event's tokens may be stale. Rather than document a footgun, the type makes it unrepresentable. The staleness window still deserves a note in the streaming reference doc, since it affects notification-only `Tail` streams too, just far less dangerously.

**No new authorization surface.** `change_stream` already filters each event by `tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens)`, and envelope-level visibility is precisely what gates a read — so on a `MerkqlTopic` stream, a subscriber allowed the event is allowed its payload.

**Payloads change what at-least-once delivery costs, and the client rule must change with it.** `ChangeSource`'s contract (`meshql-changes/src/source.rs`) justifies at-least-once on the grounds that "consumers tolerate duplicates because the client response is an idempotent refetch." With `include_payload: true` that stops holding: the client's response is to *append* the payload, so a duplicate renders a duplicate message. Server-side dedupe by cursor covers the backfill→live handover, but not reconnect — after `lagged` → close → refetch → reconnect, the browser resends its last-seen `Last-Event-ID`, the server replays events the refetch already returned, and a payload-consuming client shows each twice. For teamchat this is *the* visible failure mode.

Two rules, both belonging in the streaming reference doc:

1. **After a full refetch, the client must abandon its cursor.** Browser `EventSource` auto-reconnects *and* auto-resends `Last-Event-ID`, and there is no API to clear it — so the client must explicitly `close()` the `EventSource` and construct a **new** one, which starts with no `Last-Event-ID`. Letting it auto-reconnect after a refetch is precisely the bug.
2. **Payload-consuming clients dedupe locally by `cursor`** as belt-and-braces, since at-least-once remains the delivery contract.

**Broadcast pressure.** Payloads flow through the bounded `tokio::sync::broadcast` inside `ChangeHub`, so a payload-carrying stream lags materially sooner than a notification-only one at the same capacity. `ChangeHub::new(capacity)` is already parameterized; a payload-carrying streamlette should be configured with a larger capacity, and the plan should decide whether that's a `StreamSource` field or left to the deployment.

**Connect-time tokens.** Subscriber tokens are captured **once at connect** (`sse.rs`'s header comment states this). A long-lived stream won't observe a mid-connection privilege change — someone banned from a channel keeps receiving it until they reconnect. Acceptable for v1, but *document it*: a product with hard revocation requirements (teamchat's ban semantics are exactly this) must force a disconnect on revocation.

## Manifest

The resume capability is advertised so clients discover rather than guess — which resolves the tension with "behavior is certified identical across adapters." The *contract* is identical where a capability exists; the manifest is how a consumer learns which exist, and that is already the manifest's job.

- **Property name: `"resume": true|false`** on the entity's `sse` surface, alongside `kind` and `path`. Naming it here is the point — the server and `meshql-iron`'s new doc must agree on the key, and nothing else defines it.
- **Who produces it:** there is no framework-level manifest generator. The only one is per-deployment (`examples/egg-economy/src/manifest.rs`, which today emits a deployment-level `{"kind":"sse","path":"/changes"}`). So each deployment's generator emits its own streamlette surfaces, derived from its `StreamletteConfig`s.
- **Conformance:** add an assertion in the spirit of `examples/egg-economy/tests/manifest_conformance.rs` that **a surface advertising `resume: true` is not `Tail`-sourced.** Without it, nothing stops a manifest promising a capability the deployment can't honour — precisely the manifest-honesty failure that test already guards against for REST surfaces.

## `meshql-iron`: `references/streaming.md`

The consumption gap is why `teamchat` polls, so this ships *with* the capability, not after it.

A new reference doc covering: discover the `sse` surface and its `resume` flag from the manifest; `EventSource` subscribe; read the `ready` frame to learn the mode actually honoured; **fetch-on-connect, and again on every reconnect**; handle `lagged` by resyncing; use `Last-Event-ID` only where advertised; the connect-time-token and `Tail`-staleness caveats; and when *not* to stream (a page that reads once needs no subscription).

**Three corrections to `SKILL.md`, and it exists in five places.**

1. The Decision guide (lines 18-23) says nothing about SSE — it needs an *addition*, not an edit.
2. Non-goals line 27 says SSE is out of scope — must change.
3. Non-goals line 29, "No reactive store, no subscribe/notify machinery," contradicts the new doc just as much and was missed in the first draft — must change too.

`meshql-iron/SKILL.md` is **byte-identical (same md5, not symlinks) in five repos**: `meshql-rs`, `meshql`, `meshobj`, `cms`, `teamchat`. Updating only `meshql-rs` leaves `teamchat`'s copy — the named first consumer — still telling agents SSE is out of scope. All five need syncing, including the two product repos.

## Testing

- **Unit:** the `lagged` frame is emitted (not swallowed); the `ready` frame declares the honoured mode; token filtering excludes invisible events; `include_payload` toggles payload presence; `WireEvent` never serializes `authorized_tokens`.
- **Cursor/resume:** backfill returns exactly the events after a cursor; the handover emits no duplicates and drops nothing when a write lands *during* backfill; lag during backfill degrades to `lagged` + close; a fresh `group_id` is used per connection (assert two sequential connections both see history, which fails if a committed offset is reused); a `Tail` stream emits no cursor and ignores `Last-Event-ID` while reporting `resume: false` in `ready`.
- **Certification across sources: DROPPED, deliberately.** An earlier draft required `meshql-changes/src/testing.rs`'s `ChangeSource` cert suite to run against the merkql source as well as `Tail`. It cannot: those certs encode `SearcherTail` semantics that a raw append-only log does not have, and forcing them would mean asserting the log is something it isn't.
  - `test_ignores_identical_rewrite` is **false by construction** for a log — an append is recorded whether or not the payload changed, so there is nothing to dedupe against. `SearcherTail` can skip it only because it diffs against kept state by payload hash.
  - `test_detects_delete` and `test_update_then_delete_between_polls` assume a tombstone-aware `find_all`, i.e. deletes observed as *disappearances* from a latest-version-per-id view. A log observes deletes as records, not absences.

  The replacement is source-specific tests in Tasks 7–10, covering the log's own semantics (cursor exactness, backfill, resume degradation) rather than a shared suite that would have to be weakened until it certified nothing. The `Tail` source keeps running the existing cert suite, unchanged.
- **Manifest conformance:** `resume: true` implies a seekable source.
- **Break-it verification:** per the practice this project has settled into, deliberately break the lag frame, the dedupe, and the `group_id` freshness, and confirm the certs go red before shipping. Both prior fixes this week (`e823ce0`, `5d5cbdb`) shipped only after this check, and in both cases the pre-existing suite stayed green — which is exactly why the check exists.
- **End-to-end:** a real server with a streamlette, a real SSE client, a write, and an assertion the event arrives without polling.

## Implementation sequencing

This design is at the upper edge of a single plan — a config break, a public-struct break, two sources, resume, three-layer gap handling, manifest plus conformance, and a five-repo doc sync. Sequence it so every step ships green and independently valuable, rather than landing as one change:

1. **Lag frame on the existing `/changes`.** Fix `change_stream`'s silent `take_while`; update `lagged_subscriber_stream_closes`. Valuable on its own — it fixes a live bug in a shipped route.
2. **`ChangeEvent`/`WireEvent` gain `cursor` + `payload`** (both `None` everywhere so far). Isolates the breaking change from any behavior change.
3. **`build_app_with_streams` + `Tail` source + `ready` frame.** A working per-entity stream on every backend, no resume, no payload. (Not `ServerConfig.streamlettes` — see "Configuration" for the cycle.)
4. **`MerkqlTopic` + resume + payload**, with the single-partition assertion, topic pre-creation, fresh `group_id`, and unusable-cursor policy.
5. **`meshql-iron` `streaming.md` + the three `SKILL.md` edits, synced across all five repos.**

Steps 1–3 de-risk the rest and are worth having even if 4 slips.

## Revision history

First draft was reviewed and had substantive problems, all fixed above and recorded because several were the kind that would have produced a wrong build:

- **Resume was specified two incompatible ways** — backfill from a `Searcher` (canonical `(created_at, id)` order) *and* from the merkql log (`{partition}:{offset}`). Resolved to the log; the `searcher` config field is gone.
- **The dependency on `5d5cbdb` was claimed in error** — its order key has real millisecond ties and cannot dedupe same-millisecond versions. The offset can. Removed from "Depends on."
- **The cursor and payload had nowhere to live** — `ChangeEvent`/`WireEvent` needed new fields, presented as free. Now an explicit step with its breaking-change cost named.
- **"No polling" for `MerkqlTopic` was factually wrong** — merkql's `poll` is sync, `&mut self`, and ignores its timeout. Now specified as its own poll task, with the latency claim reframed to cost-per-poll.
- **Skip-from-`Earliest` is silently wrong with a reused `group_id`** — committed offsets win over `offset_reset`. Now mandatory and tested.
- **`Tail`'s documented ACL staleness undercut the payload argument** — `include_payload` moved onto `MerkqlTopic` so the unsound pairing is unrepresentable.
- **The manifest capability had no name and no producer** — now `"resume"`, emitted per-deployment, with a conformance assertion.
- **`meshql-iron` needed three edits, not one, across five repos** — the decision guide needs an addition, and non-goals line 29 was missed.
- **"Rejects/ignores `Last-Event-ID`" was two behaviors** — now: ignore, and declare the honoured mode in a `ready` frame.
- **Lag during backfill was unaddressed** — now degrades to `lagged` + close.

A second review confirmed all of the above landed and found five more, also fixed:

- **A single `Last-Event-ID` cannot address a multi-partition topic** — with N>1 partitions, history in partitions other than the cursor's is silently dropped or duplicated, and merkql's `default_partitions: 1` hides it indefinitely. Now a hard single-partition assertion at construction.
- **`Consumer::subscribe` silently skips topics that don't exist yet**, and `positions` is only populated in `subscribe` — so a streamlette booting before the entity's first write is a *permanently* dead stream, even after the topic appears. Now requires idempotent `create_topic` at construction, with a test that boots before any write.
- **`run_tails` is one round-robin task with one interval**, so the per-source `poll_interval` fields would have been inert, and a slow `SearcherTail` would block the aggressive merkql poll. Now one pump task per streamlette; `run_tails` is untouched for `/changes`.
- **Payloads invalidate `ChangeSource`'s "duplicates are harmless" rationale** (it assumes the client response is an idempotent refetch; a payload consumer appends). Now two explicit client rules: abandon the cursor after a refetch by closing and reconstructing the `EventSource`, and dedupe locally by cursor.
- **No policy for an unusable cursor** (malformed, wrong partition, past the tail, below retention) — each produced a silently empty stream. Now all degrade to `resume: false`.

Also settled from that review: `ready` is emitted by the streamlette handler rather than `change_stream`, so `/changes`'s wire contract is unchanged for existing consumers including meshobj's TS client; `ready.cursor` is defined as the position resume actually started from (never the log tail), so a client can detect its cursor was rejected; and both source variants name the field `poll_interval`.

Implementation found two more, corrected in place above rather than left as live requirements:

- **`ServerConfig.streamlettes` was not buildable** — `StreamletteConfig` lives in `meshql-changes`, which depends on `meshql-core`, so the field would have closed a dependency cycle. Streamlettes are a `build_app_with_streams` parameter on `meshql-server` instead; `meshql-core/src/config.rs` is untouched, which also erases the breaking-change churn the draft budgeted for.
- **The shared `ChangeSource` cert suite cannot certify a log-backed source** — `test_ignores_identical_rewrite` is false by construction for an append-only log, and the delete certs assume a tombstone-aware `find_all`. Requirement dropped, with source-specific tests in its place.

## Summary of what this design intentionally leaves open

- **A real seek API in `merkql`** — v1 skips from `Earliest`; the cursor format is chosen so a seek drops in behind it unchanged.
- **Multi-partition topics for streamlettes** — v1 asserts single-partition. Support means a per-partition cursor vector, and should wait until a deployment actually needs the throughput.
- **Mid-connection privilege changes** — tokens captured at connect; documented, not solved.
- **`SearcherTail`'s token-staleness window** — pre-existing, documented, not fixed here; it is why `Tail` cannot carry payloads.
- ~~**Whether broadcast capacity becomes a `StreamSource` field**~~ — settled: it is `StreamletteConfig.hub_capacity`, since both variants have subscribers and both can lag.
- **Java/TS parity** — separate project, genuinely different because of merkql's Rust-only constraint.
- **Converting `teamchat` off polling** — follow-on, and the real test of whether the guidance suffices.

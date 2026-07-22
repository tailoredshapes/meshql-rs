# merkql CDC bridge + worker pipeline (shared across farm's three language deployments)

**Date:** 2026-07-22
**Status:** Brainstormed, NOT yet spec-reviewed or planned — captured under time pressure ahead of a context compaction. Treat as a faithful record of a long design conversation, not a final, vetted spec. Run it through the normal spec-review loop before writing an implementation plan.
**Depends on:** the farm event-sourcing retrofit (`2026-07-22-farm-event-sourcing-retrofit-design.md`) — needs `lay_report` to exist as a real event and `hen_productivity` to exist as a real projection restlette, in all three languages, before this pipeline has anything meaningful to move.
**Background:** see the `merkql-architecture` skill (`.claude/skills/merkql-architecture/SKILL.md`) for the underlying constraint this design is built around — merkql is an embedded Rust library, not a network service, so anything that touches it directly must be Rust, but nothing past the REST boundary is language-constrained.

## The invariant this pipeline exists to prove out

Confirmed 2026-07-22, in the user's own words: *"The worker should work perfectly in all three meshql implements AND all data stores."* One Rust worker binary, unmodified, must be pointable at a farm deployment written in Rust, Java, or TS — and separately, must work regardless of which database the *connector* tails (Mongo, Postgres, SQLite, MySQL). Changing the datastore changes only the deployed connector; changing the language of the farm meshlettes changes nothing about the worker at all, only its REST/GraphQL target config.

This works because of a strict separation, confirmed across several corrections this session:

```
restlette (any language) -> database (any supported backend)
   -> merkql-connector (Rust, per-database, CDC producer)
      -> merkql topic (thin events)
         -> worker (Rust, consumer, shared/unmodified across deployments)
            -> [optional] GraphQL query back to source graphlette, for full event detail
            -> REST write to target restlette (any language)
```

The connector's only dependency is the source database's change-notification mechanism (Mongo change streams, Postgres logical replication, polling for SQLite, etc.) — it never talks to a meshlette at all. The worker's only dependency is the merkql event schema — it never touches any database directly, and never assumes what language produced the event or what language will receive its output. Both directions cross language boundaries exclusively over REST/GraphQL, meshql's existing network-facing surfaces.

## Component 1: the merkql connector (per-database CDC producer)

Extends the existing `meshql-changes` crate rather than building new plumbing — that crate already has the right shape (`ChangeSource` trait, `SearcherTail` poll-based implementation, `ChangeHub` broadcast for SSE). This pipeline adds a **second sink** alongside the existing SSE `ChangeHub`: a merkql-writing sink that takes the same `ChangeEvent` stream and appends it to a merkql topic instead of (or in addition to) broadcasting to SSE subscribers.

- **Input**: **corrected during spec review** — all three languages' `examples/farm` currently use MongoDB (verified: Java's `Main.java` wires `MongoConfig`/`MONGO_URI` exactly like Rust's and TS's; the original draft of this spec incorrectly guessed "Postgres or similar" for Java). So for this specific pipeline, the connector only needs to tail MongoDB, via the existing `meshql-changes` `ChangeSource`/`SearcherTail` + `meshql-mongo` Rust code, pointed at whichever physical Mongo instance/collection the target farm deployment (Rust, Java, or TS) is actually running against — the connector doesn't care which language wrote the data, only that it's in Mongo. **This must still be verified during planning, not assumed**: `meshql-mongo`'s document read shape (nested `{id, createdAt, payload: {...}}`, per existing project memory) must actually match what Java's and TS's own Mongo repository code writes, or `SearcherTail`/`MongoSearcher` will silently read wrong/missing fields. If a mismatch is found, the fix is a translation layer inside the connector (still Rust, still one implementation, just with per-language field mapping) — not a rewrite of the overall design. (If a future extension of this pipeline needs a non-Mongo source, e.g. Postgres, note that `meshql-postgres`'s auto-created schema does not match hand-rolled Postgres schemas some other language/example might use — that connector would need to be schema-aware and built fresh, not assumed reusable from `SearcherTail` as-is. Out of scope for farm today; flagged so it isn't a surprise later.)
- **Output**: appends `ChangeEvent { entity, id, created_at, deleted }` to a merkql topic (one topic per entity — confirmed safe against merkql's actual API during spec review: `Broker::open`/`create_topic(name, num_partitions)`/string topic names impose no constraint against this, and it matches `PRODUCT.md`'s own "one consumer group per projection" guidance for the worker side).
- **Implementation shape, resolved during spec review**: don't fork SSE vs. merkql into a mutually-exclusive mode. `ChangeHub` already exposes `subscribe()` (a `broadcast::Receiver`) — the merkql sink is simply an *additional* subscriber task that writes each event to merkql. `run_tails`/`ChangeHub` need no changes; this is strictly additive.
- **Deployment**: one small Rust binary (or a mode of the existing changes-serving process) per source database being tailed. Genuinely CDC — it reacts to writes, it doesn't get called by anything.
- **Constraint carried over from the architecture skill**: this must be Rust because it's an in-process embedded-library call (`Broker::open(...)`) — no way around that, by merkql's own design.

## Component 2: the worker (Rust, consumer, shared/unmodified across all deployments)

- **Input**: polls/consumes `ChangeEvent`s from the merkql topic(s) written by Component 1.
- **Detail lookup (optional, per event)**: the thin `ChangeEvent` is deliberately minimal — matching the existing SSE shape exactly, no new schema. When the worker needs the full record (e.g., `lay_report`'s `henId`/`eggs`/`timeOfDay`), it issues an ordinary GraphQL query against the source entity's graphlette (`getLayReport(id, at: created_at)` — reusing the existing temporal-query discipline, "reads never bypass GraphQL," already an established invariant in `meshql-patterns`). This is exactly the same "thin notification → query for detail → react" shape the SSE-consuming FE client already uses — confirmed explicitly: *"SSE is just emitting to web subscribers (who are in effect readonly workers)"* — the backend worker and an SSE-subscribed FE are the same consumer pattern with different reactions.
- **Fold logic**: application-specific — for farm, folding `lay_report` events into `hen_productivity`'s aggregate fields (exact shape deferred to the companion spec, not settled here either).
- **Output**: writes the resulting projection update via ordinary REST (`POST`/`PUT /hen_productivity/api`) to the target restlette — enforces the "single writer" invariant, workers never get direct database access, full stop.
- **Configuration, not code, varies per deployment**: which GraphQL endpoint to query for detail and which REST endpoint to write results to are ordinary per-environment config values (same pattern as existing `MONGO_URI`/`PLATFORM_URL` env vars already used across the farm examples) — pointing the same compiled worker binary at Rust-farm vs. Java-farm vs. TS-farm is purely a config change, never a rebuild.
- **Auth**: the worker authenticates as the `worker` role (see the retrofit spec's Casbin policy) when writing to `hen_productivity` — it's just another authorized REST caller, no special-cased bypass.

## Why this specific design and not alternatives

- **Not a shared library/binary embedded in each language's process** — rejected implicitly by the existing `meshql-patterns` skill's anti-pattern list (no runtime plugin/dylib loading; Rust's unstable ABI plus async trait objects make that a hazard). The worker is a wholly separate, headless Rust *process*, reachable only over the network like any other service.
- **Not per-language reimplementation of the worker** — explicitly rejected by the user (*"use merkql and share the workers across the implementations"*) — the entire point of routing detail lookups and writes through REST/GraphQL is that the worker's logic needs to be written exactly once.
- **merkql over Kafka/Debezium for this particular example**: the user's call — *"This should be well trodden at this point. And for our purposes using MerkQL should suffice."* — not a claim that merkql is required generally (the architecture skill is explicit that Kafka/Debezium remains a fully valid alternative elsewhere; the Java `legacy` example already uses it). This is a choice for this specific reference pipeline, not a new framework default.

## What's explicitly not decided here

- Exact worker process packaging (new crate under `meshql-rs/`? standalone repo? a `workers/` directory alongside `examples/farm`?) — no location decided.
- Exact config format for the worker's target endpoints (env vars, a config file, CLI flags) — pattern-matched to existing examples above, not fully specified.
- Whether the connector is a new standalone crate or a new binary target within the existing `meshql-changes` crate.
- Whether one worker process handles all farm's event→projection pipelines, or each projection gets its own worker. **Leaning "one worker per projection"**, per `merkql/PRODUCT.md`'s own "multiple consumer groups (one per projection)" guidance surfaced during spec review, and consistent with `meshql-patterns`' domain-design language — not fully confirmed with the user, but planning should default to this unless a reason emerges not to.
- **Backpressure/retry, partially resolved during spec review, still needs planning-time detail**: the natural default, given what's actually in the repo, is "don't advance the consumer offset until the REST write to the projection succeeds" — mirroring `SearcherTail::poll`'s existing transactional pattern (state commits only after all fallible ops succeed) plus merkql's manual `commit_sync()` API, giving an at-least-once-delivery + idempotent-fold = effectively-exactly-once-effect result (a pattern already named in the domain-design reference). This is a direction, not a full spec — exact retry/backoff behavior on a failed REST write is still open.

## Out of scope

- The farm event-sourcing retrofit itself (actors/events/projection entities, manifest, auth) — see the companion spec.
- The `domain` field and FE client library — both sequenced after this and the retrofit spec land.
- Any change to `examples/egg-economy`'s existing (Kafka/Debezium-based, Java-only) pipeline — untouched by this work.

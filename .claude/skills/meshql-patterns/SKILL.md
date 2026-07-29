---
name: meshql-patterns
description: Core architecture patterns for building services with meshql-rs. Use when adding entities, endpoints, resolvers, or storage backends; designing GraphQL/JSON schemas; wiring federation; or modeling domains (actors/events/projections) on meshql. Covers Repository/Searcher traits, Envelope semantics, temporal queries, authorization, and CQRS conventions.
---

# meshql Architecture Patterns

meshql turns every entity into a pair of HTTP surfaces backed by the same store:

- **REST restlette** at `/<entity>/api` — CRUD writes + simple reads, JSON Schema validation
- **GraphQL graphlette** at `/<entity>/graph` — rich queries, federation to other entities, point-in-time reads

Everything is stored as an **Envelope**: `{id, payload, created_at, deleted, authorized_tokens}`. Updates append new versions (same `id`, newer `created_at`); deletes set `deleted = true`. Nothing is mutated or hard-deleted. This is what makes temporal queries (`at:` parameter) and audit trails free — never break it.

**The pattern meshql is built for** (start here for any non-trivial system): model the domain as **events** (immutable facts) and **projections** (domain models derived from events by **workers**). Front ends write events, never domain models; new domain models can be materialized by replaying history. The invariants below exist to make this sound.

**Before wiring a single entity for a new system, read `references/domain-design.md` in full — not just this summary.** It is not one optional deep-dive among several; it is the file whose own first line says "if you internalize one pattern from this skill, make it this one." Skipping it and defaulting to plain CRUD for every entity is the single most common way to misuse this framework, and it's an easy trap: nothing stops a plain-CRUD build from compiling, passing its own tests, or looking finished. If you're choosing **merkql** as the storage backend for an entity, that's a strong signal you're already in event-sourced territory — merkql exists specifically to be an event log (see `merkql-architecture`); using it as a generic swap-in Repository/Searcher for an ordinary mutable entity works mechanically but forgoes the entire point of picking it (replay, rebuildable projections, an honest event history) and should prompt you to re-open `domain-design.md` before proceeding, not treat "I picked a database" as a finished design decision.

**A note on `meshql-iron`'s `event-vs-domain-mesh.md`, if you also have that skill loaded:** it says the event/domain split is "a recommended pattern, not a technology requirement" and tells you not to force it onto plain-CRUD entities. That's correct advice *for a frontend consuming an already-built deployment* — you can't and shouldn't invent an event/domain split for a backend someone else already designed as plain CRUD. It is not license to skip the methodology below when *you* are the one designing the backend from scratch. If you're doing both jobs (building the backend and its frontend in the same task), `domain-design.md`'s guidance governs the design decision; `event-vs-domain-mesh.md` governs how you *read* whatever the backend ends up being once it exists.

## The six invariants

Follow these in every change; they are what the architecture excels at:

1. **CQRS by convention.** Writes go through REST (`POST/PUT/DELETE /<entity>/api`). Reads — especially anything relational or historical — go through GraphQL (`/<entity>/graph`). Don't add write mutations to graphlettes; don't build join logic into restlettes.
2. **Envelopes are immutable versions.** A PUT creates a new Envelope version. A DELETE writes a tombstone. Reads return the latest non-deleted version at-or-before the requested time. Never write code that updates a row/document in place or filters without excluding `deleted`.
3. **Temporal everywhere.** Every GraphQL `Query` field takes `at: Float` (epoch millis — `Float`, not `Int`: GraphQL `Int` is 32-bit and overflows on millisecond timestamps) and every `Repository.read` / `Searcher.find` honors it. The root query's `at` propagates through federated resolver hops, so point-in-time reads are consistent across the whole graph. When adding a query, include `at` in the schema and let the adapter's version-windowing handle it.
4. **Authorization rides the Envelope.** `authorized_tokens` on each Envelope is matched against caller credentials extracted by the `Auth` trait. Visibility: empty tokens = public, `"*"` = everyone, otherwise intersection. Repositories and Searchers must filter by tokens on *every* read path — adding a query that skips token filtering is a security bug.
5. **Storage is pluggable, behavior is certified — and the certification *is* the contract.** Datastores only implement `Repository` + `Searcher` (`meshql-core/src/lib.rs`). All business behavior lives above the traits. A new adapter must pass the shared certification tests (`meshql-core/src/testing.rs`, `meshql-cert`). Choosing a backend is a question of what you already run, what you know, and what you like — **not** of capability: there is no practical difference in behaviour between the implementations, and that is exactly what the certification suite exists to guarantee. The property this buys you is that you can swap the database engine and replay the queue into the new store without any end user noticing a difference. The consequence that matters: **never depend on a storage-engine property the certification does not guarantee.** See `references/storage-adapters.md`, "Don't reach underneath the abstraction."
6. **Pick your scale — one size does not fit all.** The same abstraction deliberately has an in-process form and a distributed form, and choosing a different implementation for dev/test than for production is *idiomatic*, not a compromise. In-memory SQLite or merkql for a test; Postgres, Mongo, or Kafka for prod. A poll-based CDC tail in-process; a native change-stream connector in prod. The behavior contract (certification, the trait, the fold) is identical; only the deployment weight changes. When you see two implementations of one seam, that is the design working — do not collapse them.

## Honesty: as-of freshness

The FE should be able to tell how fresh a payload is, so it can show a "pending" state and let the user refresh instead of silently rendering stale data. Envelope metadata stays internal (invariant 2 links `created_at` to versioning, not exposure) except for two deliberate, minimal leaks:

- **REST**: `create`/`read`/`update` responses carry `X-Meshql-Created-At` (RFC3339) and `X-Meshql-Deleted` (`true`/`false`) response headers — automatic, no config, the JSON body stays payload-only. Not present on `list` or `delete` (see `references/storage-adapters.md` and `meshql-restlette/src/routes.rs`).
- **GraphQL**: every Searcher merges `createdAt` (RFC3339) into the result Stash next to `id`. A type opts in by declaring `createdAt: String` — the scalar field resolver is a generic map lookup, so no resolver code is needed. `deleted` is never exposed (search results already exclude deleted/superseded versions).

Don't widen this: no other Envelope field (`authorized_tokens`, etc.) should ever leak into a REST body or an unopted GraphQL field.

## Naming and layout conventions

| Thing | Convention | Example |
|---|---|---|
| Graphlette path | `/<entity>/graph` (snake_case entity) | `/lay_report/graph` |
| Restlette path | `/<entity>/api` | `/lay_report/api` |
| Collection/table | plural snake_case | `lay_reports` |
| Foreign key in payload | camelCase `<parent>Id` | `farmId` |
| GraphQL schema file | `config/graph/<entity>.graphql` | `config/graph/hen.graphql` |
| JSON Schema file | `config/json/<entity>.schema.json` | `config/json/hen.schema.json` |

**Query naming — pick one dialect per service and be consistent:**

- **Entity-named** (used by `examples/farm`, `examples/egg-economy`): `getFarm(id, at)`, `getFarms(..., at)`, `getCoopsByFarm(id, at)`. Readable in multi-entity schemas.
- **Generic** (required for `meshql-mcp` auto-derivation, which matches these names exactly — see `meshql-mcp/src/capability.rs`): `getById(id, at)`, `getAll(at)`, `getByName(name, at)`, `getBy<X>Id(<x>_id, at)`. Use this dialect if the deployment will be exposed to LLM agents via `meshql-mcp`; entity-named queries are invisible to its auto-derivation (you'd have to hand-write capabilities).

## Minimal entity wiring (the canonical shape)

From `examples/farm/src/main.rs` — one entity is always: repo + searcher + RootConfig + GraphletteConfig + RestletteConfig.

```rust
let farm_repo = Arc::new(MongoRepository::new(URI, DB, "farms", Arc::clone(&auth)).await?);
let farm_searcher: Arc<dyn Searcher> =
    Arc::new(MongoSearcher::new(URI, DB, "farms", Arc::clone(&auth)).await?);

let farm_config = RootConfig::builder()
    .singleton("getFarm", r#"{"id": "{{id}}"}"#)          // envelope id — top-level
    .vector("getFarms", r#"{"payload.name": "{{name}}"}"#) // payload field — needs payload.
    .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
    .build();

// ServerConfig { port, graphlettes: vec![GraphletteConfig { path: "/farm/graph", schema_text, root_config, searcher }],
//                restlettes: vec![RestletteConfig { path: "/farm/api", schema_json, repository }] }
// then meshql_server::run(config)  — or run_with_auth / run_ext for custom Auth / extra routes
```

### Query templates: `payload.` is not optional

Query templates are **Handlebars producing a JSON query**. The single most important rule:

> **Payload fields need the `payload.` prefix. The Envelope's own `id` is top-level.**
> `{"payload.farmId": "{{id}}"}` — correct. `{"farmId": "{{id}}"}` — silently wrong.

Every Rust adapter's query builder matches on exactly these two shapes:

| Adapter | `{"id": …}` becomes | `{"payload.x": …}` becomes | A bare `{"x": …}` does |
|---|---|---|---|
| `meshql-merkql` | dot-path `id` on the Envelope | dot-path `payload.x` | **no match → empty result** |
| `meshql-postgres` | `id = $n` | `(payload::jsonb)->>'x' = $n` | **silently skipped** (`query.rs`) |
| `meshql-sqlite` | `id = ?` | `json_extract(payload, '$.x') = ?` | **silently skipped** |
| `meshql-ksql` | `id = '…'` | `EXTRACTJSONFIELD(payload, '$.x') = '…'` | **silently skipped** |
| `meshql-mysql` | `` `id` = ? `` | `JSON_UNQUOTE(JSON_EXTRACT(payload, '$.x')) = ?` | emitted as a bare column → **SQL error** |
| `meshql-mongo` | `id` (top-level) | `payload.x` (nested doc path) | matches a non-existent top-level field → **empty result** |

**Every one of these failure modes is silent.** merkql's matcher (`meshql-merkql/src/matcher.rs:16-26`) does a literal dot-path lookup against the serialized Envelope — `{id, payload, created_at, deleted, authorized_tokens}` — and returns `false` when the path is absent:

```rust
for (key, expected) in query_obj {
    let path: Vec<&str> = key.split('.').collect();
    match get_path(record_json, &path) {
        Some(actual) => { if actual != expected { return false; } }
        None => return false,
    }
}
```

The SQL adapters are arguably worse: an unrecognised key hits `// Unknown key — skip` (`meshql-postgres/src/query.rs`, `meshql-sqlite/src/query.rs`, `meshql-ksql/src/query.rs`), so if it was the *only* condition the WHERE clause comes out empty and the query returns **every record** rather than none.

**You will not get an error either way.** A by-id test passes regardless, because `id` is top-level and correct in both the right and the wrong version of a template. So: **always test a list/vector query against data you actually wrote.** A test suite that only exercises `getById` will go green over a completely broken set of templates. This exact bug was fixed in `examples/farm` — see `examples/farm/src/lib.rs:35-36`, where `getFarm` uses `{"id": …}` and `getFarms` uses `{"payload.name": …}`.

### merkql matcher limits: equality and AND, nothing else

`matches()` above is the whole matcher. It supports **exact equality only, with all conditions ANDed.** There is **no range comparison, no OR, no ORDER BY, no LIKE, no IN.** A `>=` or a sort order cannot be expressed in a merkql query template at all.

Every range question must therefore become a **materialised bucket field** written by the worker at fold time — `close_period`, `occurred_on`, `size_band` — and then queried by equality:

```rust
// Not expressible: "reports in the last 30 days"
.vector("getLayReportsOn", r#"{"payload.occurred_on": "{{occurred_on}}"}"#)
```

Treat this as good practice rather than merely a workaround: bucket fields are also the **portable intersection across all the adapters**, so a projection built this way keeps invariant 5's swap-and-replay property intact. A projection that leans on Postgres range predicates silently pins itself to Postgres.

## Deployment model: compose your own binary

A meshql system is a small Rust binary owned by the service developer — there is no prebuilt meshql server configured at runtime. This is a deliberate design decision:

- **Cargo is the composition mechanism.** Depend on `meshql-core`, `meshql-server`, and *only* the adapter crates the service uses (`meshql-postgres`, `meshql-mongo`, …). Unused backends are never imported, so they cost nothing — the crate split plays the role a classpath/plugin directory plays elsewhere.
- **`main.rs` is the configuration.** The ~130-line wiring file (see `examples/farm/src/main.rs`) is code-as-config on purpose: entity wiring, resolver names, and query templates are type-checked at compile time instead of failing at runtime.
- **Adapters are interchangeable at the source level.** The certification suite guarantees behavior parity, so swapping Postgres for Mongo is a two-constructor-line change, not a framework migration.
- **Do not propose runtime plugin loading** (dylibs/`dlopen`, runtime driver registries). Rust's unstable ABI plus async trait objects make shared-library plugins a hazard, and the idea was considered and rejected. If a prebuilt-binary distribution need ever arises, the agreed direction is out-of-process adapter sidecars over JSON-RPC (the `Repository`/`Searcher` surfaces are already wire-shaped) — never shared libraries.

## Decision guide

- **Adding/changing an entity or query** → read `references/adding-an-entity.md`
- **Relating entities (1:1, 1:N), internal vs HTTP federation** → read `references/federation.md`
- **New datastore adapter, or touching Repository/Searcher internals** → read `references/storage-adapters.md`
- **Modeling a domain** (the pattern meshql is built for): identify **events** first (immutable facts), derive **projections** (domain models) from them, and write **one worker per projection** that folds events into it. Front ends write events, never domain models — enforced by infra + convention — and new domains can be built by replaying history. Full methodology in `references/domain-design.md`. Entity taxonomy split into **actors** (long-lived: farm, hen), **events** (lay_report, storage_deposit), and **projections** (hen_productivity) is shown across `examples/egg-economy/` (13 entities); `examples/farm/` is the minimal non-event-sourced case.
- **Custom commands/side effects**: don't bolt logic into adapters. Use `build_restlette_router_ext` (validators, defaults, `post_create` side effects) or `run_ext` extra Axum routes for computed endpoints. See `meshql-restlette/src/routes.rs`.
- **Auth beyond NoAuth**: `StashKeyAuth` extracts identity from a request stash key; wrap with `CasbinAuth` (`meshql-casbin`) for role-based action checks (`authorize_action(creds, "write")`). Pass via `run_with_auth`/`build_app_with_auth`.
- **Exposing a deployment to LLM agents at runtime** → use the existing `meshql-mcp` crate (`meshql-mcp/README.md`), not this skill.
- **FE needs to know how fresh data is / show a "pending" state** → see "Honesty: as-of freshness" above; add `createdAt: String` to the GraphQL type, nothing else to do.

## Anti-patterns to flag

- A GraphQL query without `at: Float` — breaks temporal uniformity. (`at: Int` is also wrong: millisecond timestamps overflow GraphQL's 32-bit `Int`.)
- Hard deletes, in-place updates, or reads that don't filter `deleted`.
- A Searcher query path that skips `authorized_tokens` filtering.
- **A query template addressing a payload field without the `payload.` prefix** — `{"farmId": "{{id}}"}`. Fails silently: empty results on merkql/Mongo, *all* results on the SQL adapters, an error only on MySQL. Test a list query against written data, not just `getById`.
- **A range, `OR`, or sort expressed in a query template.** The merkql matcher is equality-and-AND only; materialise a bucket field in the projection instead.
- Business/aggregation logic inside an adapter crate (`meshql-mongo`, `meshql-postgres`, …) — it belongs in restlette validators/side-effects, extra routes, or projection entities.
- Cross-entity joins implemented in a restlette — that's what graphlette resolvers are for.
- A new adapter merged without passing the certification suite.
- **Depending on an engine property the certification doesn't guarantee** — a physical row id, a writer-serialisation guarantee, a native sequence — especially as a CDC cursor or an ordering key. It silently couples the product to one backend and breaks swap-and-replay (invariant 5; `references/storage-adapters.md`).
- Building machinery to reconstruct a total event order before publishing — the queue's topic append already defines the order (`references/domain-design.md`, "The queue is the ordering authority").
- A stateful domain refusal reported as a 4xx from an admission gate, discarding the fact that the user tried it. Admission refuses only what's refusable without domain state; the rest is a worker-emitted rejection object (`references/domain-design.md`).
- Runtime plugin/driver-loading schemes for adapters — composition happens in `Cargo.toml` (see "Deployment model").

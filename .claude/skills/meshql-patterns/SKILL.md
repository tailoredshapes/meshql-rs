---
name: meshql-patterns
description: Core architecture patterns for building services with meshql-rs. Use when adding entities, endpoints, resolvers, or storage backends; designing GraphQL/JSON schemas; wiring federation; or modeling domains (actors/events/projections) on meshql. Covers Repository/Searcher traits, Envelope semantics, temporal queries, authorization, and CQRS conventions.
---

# meshql Architecture Patterns

meshql turns every entity into a pair of HTTP surfaces backed by the same store:

- **REST restlette** at `/<entity>/api` — CRUD writes + simple reads, JSON Schema validation
- **GraphQL graphlette** at `/<entity>/graph` — rich queries, federation to other entities, point-in-time reads

Everything is stored as an **Envelope**: `{id, payload, created_at, deleted, authorized_tokens}`. Updates append new versions (same `id`, newer `created_at`); deletes set `deleted = true`. Nothing is mutated or hard-deleted. This is what makes temporal queries (`at:` parameter) and audit trails free — never break it.

**The pattern meshql is built for** (start here for any non-trivial system): model the domain as **events** (immutable facts) and **projections** (domain models derived from events by **workers**). Front ends write events, never domain models; new domain models can be materialized by replaying history. The invariants below exist to make this sound. See `references/domain-design.md`.

## The five invariants

Follow these in every change; they are what the architecture excels at:

1. **CQRS by convention.** Writes go through REST (`POST/PUT/DELETE /<entity>/api`). Reads — especially anything relational or historical — go through GraphQL (`/<entity>/graph`). Don't add write mutations to graphlettes; don't build join logic into restlettes.
2. **Envelopes are immutable versions.** A PUT creates a new Envelope version. A DELETE writes a tombstone. Reads return the latest non-deleted version at-or-before the requested time. Never write code that updates a row/document in place or filters without excluding `deleted`.
3. **Temporal everywhere.** Every GraphQL `Query` field takes `at: Float` (epoch millis — `Float`, not `Int`: GraphQL `Int` is 32-bit and overflows on millisecond timestamps) and every `Repository.read` / `Searcher.find` honors it. The root query's `at` propagates through federated resolver hops, so point-in-time reads are consistent across the whole graph. When adding a query, include `at` in the schema and let the adapter's version-windowing handle it.
4. **Authorization rides the Envelope.** `authorized_tokens` on each Envelope is matched against caller credentials extracted by the `Auth` trait. Visibility: empty tokens = public, `"*"` = everyone, otherwise intersection. Repositories and Searchers must filter by tokens on *every* read path — adding a query that skips token filtering is a security bug.
5. **Storage is pluggable, behavior is certified.** Datastores only implement `Repository` + `Searcher` (`meshql-core/src/lib.rs`). All business behavior lives above the traits. A new adapter must pass the shared certification tests (`meshql-core/src/testing.rs`, `meshql-cert`).

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
    .singleton("getFarm", r#"{"id": "{{id}}"}"#)
    .vector("getFarms", r#"{"name": "{{name}}"}"#)
    .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
    .build();

// ServerConfig { port, graphlettes: vec![GraphletteConfig { path: "/farm/graph", schema_text, root_config, searcher }],
//                restlettes: vec![RestletteConfig { path: "/farm/api", schema_json, repository }] }
// then meshql_server::run(config)  — or run_with_auth / run_ext for custom Auth / extra routes
```

Query templates are **Handlebars producing a JSON query**: `{"farmId": "{{id}}"}`. Top-level keys address payload fields (adapters map them to JSONB/doc paths); `id` addresses the Envelope id.

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

## Anti-patterns to flag

- A GraphQL query without `at: Float` — breaks temporal uniformity. (`at: Int` is also wrong: millisecond timestamps overflow GraphQL's 32-bit `Int`.)
- Hard deletes, in-place updates, or reads that don't filter `deleted`.
- A Searcher query path that skips `authorized_tokens` filtering.
- Business/aggregation logic inside an adapter crate (`meshql-mongo`, `meshql-postgres`, …) — it belongs in restlette validators/side-effects, extra routes, or projection entities.
- Cross-entity joins implemented in a restlette — that's what graphlette resolvers are for.
- A new adapter merged without passing the certification suite.
- Runtime plugin/driver-loading schemes for adapters — composition happens in `Cargo.toml` (see "Deployment model").

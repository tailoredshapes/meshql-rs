# Federation: Resolvers and Query Templates

Graphlettes federate by calling **other graphlettes' queries**, never by joining at the storage layer. Each entity owns its own store; relations are resolved at query time.

## Query templates

Defined on `RootConfig` (`meshql-core/src/config.rs`). A template is Handlebars text that renders to a JSON query:

```rust
.singleton("getCoop", r#"{"id": "{{id}}"}"#)              // one result
.vector("getCoops", r#"{"name": "{{name}}"}"#)            // many results
.vector("getCoopsByFarm", r#"{"farmId": "{{id}}"}"#)      // relation lookup
```

- `singleton` vs `vector` controls whether the GraphQL field returns an object or a list — it must match the schema's return type (`Coop` vs `[Coop]`).
- Top-level template keys address **payload fields** (`farmId`, `name`); the special key `id` addresses the Envelope id. Adapters translate to their native form (Mongo query doc, SQL over JSONB, etc.).
- Args come from the GraphQL call's arguments — `getCoopsByFarm(id: "farm-1")` renders `{"farmId": "farm-1"}`.

## The two resolver directions

For Farm 1—N Coop (coop payload carries `farmId`):

**Child → parent (singleton):** on the coop's RootConfig:

```rust
.singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
//                  ^field   ^foreign_key    ^query     ^target graphlette
```

Reads `farmId` from the parent Coop object, then invokes `getFarm` on `/farm/graph` with `id = <farmId>`.

**Parent → children (vector):** on the farm's RootConfig:

```rust
.vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
```

`foreign_key: None` means "pass this object's own `id`". The target graphlette must define `getCoopsByFarm` with template `{"farmId": "{{id}}"}` — **the relation query lives on the child entity's RootConfig**, the resolver on the parent's. Forgetting one half is the most common federation bug.

## Internal vs HTTP resolution

- `singleton_resolver` / `vector_resolver` take a target that can be a local path (`"/coop/graph"`) or a full URL to a remote meshql service. `meshql-server::build_app*` registers every local graphlette's searcher in a `ResolverRegistry` keyed by path, so local-path resolution happens **in-process** — no HTTP hop.
- `internal_singleton_resolver` / `internal_vector_resolver` force registry resolution explicitly.
- Use full URLs only when federating across separately deployed services (see `examples/egg-economy-sap` / `-salesforce` for anti-corruption-layer federation to external systems).

**Temporal caveat:** resolver hops currently evaluate at `Utc::now()` (`meshql-graphlette/src/schema_builder.rs`) — the root query's `at` does **not** propagate into federated fields. A point-in-time read applies to the root entity only; related entities resolve as of now. Don't promise (or test for) historical consistency across a resolver hop.

## Schema side

Each graphlette's `.graphql` file declares a **local view** of foreign types — only the fields reachable through this entity:

```graphql
type Farm {
    id: ID
    name: String
    coops: [Coop]      # resolved by vector_resolver
}

type Coop {            # local projection of the Coop entity
    id: ID
    name: String
}
```

Keep these views one level deep. If a consumer needs farm → coop → hen in one query, the chain works because each hop is its own resolver (see `examples/farm`, which chains farm → coop → hen → lay_report).

## Checklist for a new relation

1. Child payload carries `<parent>Id` (camelCase) — add it to the child's JSON Schema.
2. Child RootConfig: `vector` query `get<Child>sBy<Parent>` with template `{"<parent>Id": "{{id}}"}`.
3. Child schema: `parent` field + local parent type; child RootConfig: `singleton_resolver` with `Some("<parent>Id")`.
4. Parent schema: `children: [Child]` field + local child type; parent RootConfig: `vector_resolver` with `None`.
5. Both queries take `at: Int` in the schema.

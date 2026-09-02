# Adding an Entity

The complete checklist for adding entity `widget` (with a `parentId` relation to `parent`). Reference implementation: `examples/farm/` (minimal, 4 entities) and `examples/egg-economy/` (13 entities, actors/events/projections).

## 1. GraphQL schema — `config/graph/widget.graphql`

```graphql
type Widget {
    id: ID
    name: String
    parentId: String
    parent: Parent
}

# Local view of the federated type: declare only the fields this
# graphlette exposes. Each graphlette owns its own schema file.
type Parent {
    id: ID
    name: String
}

type Query {
    getWidget(id: ID, at: Float): Widget
    getWidgets(name: String, at: Float): [Widget]
    getWidgetsByParent(id: ID, at: Float): [Widget]
}
```

Rules:
- **Every** query takes `at: Float` (epoch millis, point-in-time read — not `Int`, which is 32-bit in GraphQL and overflows on millis).
- **Optional:** add `createdAt: String` to a type to opt into the "honesty" as-of field — every Searcher already merges it into the result Stash (RFC3339, next to `id`), so declaring the field is all that's needed; no resolver code. Omit it and it's simply not selectable. (REST gets the equivalent via `X-Meshql-Created-At`/`X-Meshql-Deleted` response headers automatically — see SKILL.md.)
- Query naming: this example uses the entity-named dialect (`getWidget`/`getWidgets`/`getWidgetsBy<Parent>`) matching the repo's examples. If the service will be exposed to LLM agents via `meshql-mcp` auto-derivation, use the generic dialect instead (`getById`/`getAll`/`getByName`/`getBy<X>Id`) — the MCP parser matches those names exactly. See SKILL.md "Query naming".
- Federated fields (`parent: Parent`) get a local type definition with just the fields you want reachable from this entity. One level of nesting is the norm.

## 2. JSON Schema — `config/json/widget.schema.json`

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "name": { "type": "string" },
    "parentId": { "type": "string" }
  },
  "required": ["name"]
}
```

**`schema_json` is published, not enforced — wire validation yourself.** `build_restlette_router*` carries this schema into the manifest (so clients can discover the write contract) but does **not** validate incoming payloads against it. A `POST` with missing required fields or wrong types is accepted as-is. Two independent product builds have now discovered this the same way — by writing a test asserting a bad payload gets rejected, and watching it come back `201`.

If writes need to actually be validated (they almost always do), use `build_restlette_router_ext`'s `validator` extension point with a compiled JSON Schema validator (the `jsonschema` crate), pointed at the same schema file the manifest publishes — so the published contract and the enforced contract can't drift. Don't assume declaring `schema_json` is sufficient, and don't write a test that only asserts the *happy* path, which passes either way.

This validates the REST write payload. It describes the **payload** only — never include `id`, `created_at`, `deleted`, or `auth`; those are Envelope metadata managed by the framework.

## 3. Wire it in `main.rs`

```rust
const WIDGET_GRAPHQL: &str = include_str!("../config/graph/widget.graphql");
const WIDGET_JSON: &str = include_str!("../config/json/widget.schema.json");

// Repository (REST writes/reads) and Searcher (GraphQL reads) over the same collection
let widget_repo =
    Arc::new(MongoRepository::new(URI, DB, "widgets", Arc::clone(&auth)).await?);
let widget_searcher: Arc<dyn Searcher> =
    Arc::new(MongoSearcher::new(URI, DB, "widgets", Arc::clone(&auth)).await?);

let widget_config = RootConfig::builder()
    // queries: name must match the GraphQL Query field; template is
    // Handlebars rendering a JSON query against payload fields (and "id")
    .singleton("getWidget", r#"{"id": "{{id}}"}"#)
    .vector("getWidgets", r#"{"payload.name": "{{name}}"}"#)
    .vector("getWidgetsByParent", r#"{"payload.parentId": "{{id}}"}"#)
    // relations: see references/federation.md
    .singleton_resolver("parent", Some("parentId"), "getParent", "/parent/graph")
    .build();
```

Add to `ServerConfig`:

```rust
graphlettes: vec![ /* ... */
    GraphletteConfig {
        path: "/widget/graph".to_string(),
        schema_text: WIDGET_GRAPHQL.to_string(),
        root_config: widget_config,
        searcher: widget_searcher,
    },
],
restlettes: vec![ /* ... */
    RestletteConfig {
        path: "/widget/api".to_string(),
        schema_json: serde_json::from_str(WIDGET_JSON)?,
        repository: widget_repo,
    },
],
```

If the new entity is the *target* of relations, also add the reverse query (`getWidgetsByParent`-style) and a resolver field to the parent's schema + RootConfig.

## 4. What you get for free

- `POST/GET/PUT/DELETE /widget/api[/:id]` with schema validation, defaults, soft delete, versioning
- `POST /widget/graph` GraphQL with temporal reads and federation
- Authorization filtering on every read, via the `Auth` passed to repo/searcher/server
- `X-Meshql-Created-At`/`X-Meshql-Deleted` response headers on REST `create`/`read`/`update` — the "honesty" as-of fields, automatic, no config needed

## 5. Validators, defaults, and side effects (commands)

When a write needs business rules or downstream effects, do it at the restlette layer — never in the storage adapter. `build_restlette_router_ext` (re-exported by `meshql-server`) accepts:

- `defaults: Option<Stash>` — fields merged into the payload when absent
- `validator: ValidatorFn` — reject writes with a 400 (gets a `ValidatorContext` with an HTTP client + service URLs for cross-entity checks)
- `post_create: PostCreateFn` — fire-and-forget side effect after creation (e.g., emit a follow-up event entity)

Mount the resulting router via `run_ext(config, extra_router)` / `build_app_ext`. For computed read endpoints that don't fit the graph (e.g., aggregations), add plain Axum routes to `extra` the same way.

## 6. Verify

```bash
cargo build -p <example-or-server-crate>
cargo test -p <crate>
```

Smoke test: `POST /widget/api` a payload, then query `getWidget(id: "...")` and `getWidget(id: "...", at: <past-millis>)` to confirm temporal behavior.

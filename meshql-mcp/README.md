# meshql-mcp

Model Context Protocol server building blocks for meshql-rs deployments.

Given a running meshql-rs HTTP server (one or more meshql entities, each with REST + GraphQL surfaces), this crate exposes a stdio JSON-RPC MCP server that an LLM can drive. Catalog operations — list, get-by-id, find-by-name — are auto-derived from the deployment's GraphQL schemas. Custom analytics or write tools layer on top.

## Quick start

```rust
use meshql_mcp::{CapabilitiesBuilder, McpServerConfig, MeshqlClient, MeshqlMcpServer};
use std::sync::Arc;

const DEPLOYABLE_GRAPHQL: &str = include_str!("../config/graph/deployable.graphql");
const SERVICE_GRAPHQL:    &str = include_str!("../config/graph/service.graphql");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Arc::new(MeshqlClient::from_env("CATALOG_URL", "http://localhost:3000"));

    let capabilities = CapabilitiesBuilder::new()
        .auto_from_schemas(&[
            ("deployable", "/deployable/graph", DEPLOYABLE_GRAPHQL),
            ("service",    "/service/graph",    SERVICE_GRAPHQL),
        ])
        .describe("list_deployables",
            "List every deployable with federated team metadata. Use for inventory.")
        .build();

    MeshqlMcpServer::new(McpServerConfig {
        server_name: "catalog-mcp".into(),
        server_version: env!("CARGO_PKG_VERSION").into(),
        client,
        capabilities,
    }).serve_stdio().await
}
```

That's a complete MCP server. About 30 LOC including the imports and the schema constants.

## The two layers

- **Low-level** (`tool` + `transport`): `Tool` is the raw MCP tool struct (name + description + input_schema + closure handler). `MeshqlMcpServer` speaks JSON-RPC 2.0 over stdio and dispatches `tools/call` to the registered handlers. Drop down to this layer when you need full control over a tool's behavior.

- **High-level** (`capability` + `schema`): `Capability` describes a tool declaratively — its handler is a `CapabilityHandler` enum that says "POST this templated GraphQL query" or "POST this REST path". The `CapabilitiesBuilder` lets you assemble a vec of capabilities from schema files plus optional per-name description overrides plus custom capabilities. This is what most callers want.

## Auto-derivation conventions

`CapabilitiesBuilder::auto_from_schemas` parses each schema's `type Query { ... }` block and produces one capability per operation, following these conventions:

| Operation          | Capability name              | Description sketch                          |
|--------------------|------------------------------|---------------------------------------------|
| `getAll`           | `list_<entities>`            | "List every <entity>…"                      |
| `getById(id)`      | `get_<entity>_by_id`         | "Fetch one <entity> by UUID…"               |
| `getByName(name)`  | `find_<entities>_by_name`    | "Find <entities> whose name matches…"       |
| `getByXId(x_id)`   | `<entities>_for_<x>`         | "List <entities> related to a given <x>…"   |

Pluralization is naive (`<entity>s`). When the default reads wrong, override with `.describe("name", "better description")`.

Each generated capability's `query_template` selects every field on the entity type, including federated projections expanded one level deep — so the LLM gets rich, federation-aware results in a single round trip.

## CapabilityHandler variants

- `GraphQuery { path, query_template }` — POST a templated GraphQL query to a graphlette. The handler substitutes `{arg}` placeholders from the call's input JSON (string values are GraphQL-escaped). Used by auto-derived catalog capabilities.
- `RestGet { path_template }` — GET a templated REST path. For computed/aggregated endpoints not yet in the graph schema (e.g. `/org_node/:id/effective_bylaws`).
- `RestPost { path_template, body_template }` — POST a templated REST path. For writes and commands (e.g. `/change_request/:id/plan`).
- `Custom(handler)` — escape hatch. Drop in any async closure with the right signature when the templated handlers don't fit.

## CQRS

Reads go via `/graph` (GraphQL); writes go via `/api` (REST). Auto-derived catalog capabilities use `GraphQuery` handlers. Custom tools that wrap computed-but-unmodeled GET endpoints use `RestGet`. Custom tools that POST commands use `RestPost`.

The `MeshqlClient` exposes both `gql(path, query)` and `get_path(path)` / `post_path(path, body)` / `list(entity)` / `get(entity, id)` — see the client module docs for which to use when.

## License

MIT.

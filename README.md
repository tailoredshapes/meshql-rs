# MeshQL-RS

Define schemas. Wire resolvers. Get a full data API with REST, GraphQL, and federation — no boilerplate.

MeshQL-RS is the Rust implementation of [MeshQL](https://tailoredshapes.github.io/meshql/), a framework for building data services where every entity gets its own REST endpoint, its own GraphQL endpoint, and federation resolvers that connect them. You write configuration, not plumbing.

## What You Get

- **GraphQL** endpoints with queries, mutations, and federated resolvers
- **REST** endpoints with `POST`, `GET`, `PUT`, `DELETE` and bulk operations
- **JSON Schema validation** on REST writes
- **Temporal queries** — every query supports point-in-time reads
- **Health checks** at `/health` and `/ready`

## Core Concepts

| Concept | What It Does |
|:--------|:-------------|
| **Graphlette** | GraphQL endpoint for an entity — queries, federation resolvers |
| **Restlette** | REST endpoint for an entity — CRUD, bulk ops, JSON Schema validation |
| **Resolver** | Connects entities across graphlettes (singleton for 1:1, vector for 1:N) |
| **Envelope** | Internal wrapper: `{id, payload, created_at, deleted}` — stays internal except for opt-in "honesty" fields (see below) |

## Features

- **Dual APIs**: REST and GraphQL from the same entity definition
- **Federation**: Resolvers connect entities across graphlettes via HTTP or in-process calls
- **Multiple datastores**: MongoDB, PostgreSQL, MySQL, SQLite, MerkQL, DynamoDB, merk-cloud — mix and match
- **Temporal queries**: Point-in-time reads on any query
- **"Honesty" as-of freshness**: REST responses carry `X-Meshql-Created-At`/`X-Meshql-Deleted` headers; GraphQL types can opt into a `createdAt` field — so the FE always knows how fresh a payload is
- **Change feed**: `meshql-changes` streams SSE change notifications so clients don't have to poll for read-your-writes
- **Async throughout**: Built on Tokio and Axum for efficient async I/O
- **Type-safe**: Rust's type system catches configuration errors at compile time

## Workspace Crates

```
meshql-rs/
├── meshql-core/        # Traits: Repository, Searcher, Config, Envelope
├── meshql-graphlette/  # GraphQL endpoint implementation (async-graphql)
├── meshql-restlette/   # REST endpoint implementation (axum)
├── meshql-server/      # Server assembly with CORS and routing
├── meshql-mongo/       # MongoDB adapter
├── meshql-postgres/    # PostgreSQL adapter (sqlx)
├── meshql-mysql/       # MySQL adapter (sqlx)
├── meshql-sqlite/      # SQLite adapter (sqlx)
├── meshql-merkql/      # MerkQL adapter (embedded event log)
├── meshql-merksql/     # ksqlDB-style streaming SQL engine over merkql
├── meshql-ksql/        # Confluent Cloud ksqlDB/Kafka adapter (HTTP-only)
├── meshql-dynamo/      # DynamoDB adapter (Repository + Searcher)
├── meshql-merk/        # merk-cloud adapter — create-only Repository, no Searcher
├── meshql-casbin/      # RBAC authorization (wraps another Auth)
├── meshql-changes/     # SSE change feed + deployment manifest generator
├── meshql-mcp/         # Auto-derived MCP server for LLM agents
├── meshql-lambda/      # AWS Lambda adapter (meshql-server on lambda_http)
├── meshql-cert/        # Cucumber BDD test suite
└── examples/
    ├── farm/                    # Hierarchical federation (4 entities)
    ├── egg-economy/             # Event sourcing + projections (13 entities)
    ├── egg-economy-sap/         # Anti-corruption layer over SAP
    └── egg-economy-salesforce/  # Anti-corruption layer over Salesforce
```

## Quick Start

### Prerequisites

- Rust 1.75+ (2021 edition)
- Docker (for database-backed tests)

### Build

```bash
cargo build
```

### Test

```bash
cargo test
```

### Run the Farm Example

```bash
cargo run -p farm
```

## Also See

- [MeshQL Documentation](https://tailoredshapes.github.io/meshql/) — product docs
- [MeshQL-Java](https://github.com/tailoredshapes/meshql-java) — the Java 21 implementation
- [MeshQL-TS](https://github.com/tailoredshapes/meshql-ts) — the TypeScript implementation (the OG)

## License

[Business Source License 1.1](LICENSE)

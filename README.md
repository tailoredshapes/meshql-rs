# meshql-rs

The Rust implementation of **[meshql](https://git.tildarc.com/tailoredshapes/meshql)**.

What meshql *is* — the model, the envelope, temporal reads, honesty, how
authorization works, and the Gherkin contract every storage plugin is certified
against — is defined once in the [contract repository][contract], which this
repository runs as a git submodule at `meshql-cert/tests/features/contract`.
Read that first. This README is the *how*: what is specific to running meshql on
Tokio and Axum.

[contract]: https://git.tildarc.com/tailoredshapes/meshql

## What is specific to Rust

- **Async throughout**, on Tokio and Axum.
- **The widest set of storage plugins**: MongoDB, PostgreSQL, MySQL, SQLite,
  MerkQL, MerkSQL, ksqlDB, DynamoDB, and merk-cloud.
- **`meshql-changes`**, an SSE change feed so a client gets read-your-writes
  without polling. Neither other implementation has one.
- **`meshql-mcp`**, an MCP server derived from the configuration, so an agent
  can query a deployment without bespoke tooling.
- **`meshql-lambda`**, which runs a server on `lambda_http`.
- **`/<id>/versions`**, a REST endpoint listing a document's version history.
  Java and TypeScript do not have this yet; nothing in the contract certifies
  it, which is why the gap went unnoticed.

### Certifying the ksqlDB adapter

It needs a real Kafka and ksqlDB, not a mock. `scripts/ksql-local.sh` starts
both and prints the environment:

```bash
eval "$(scripts/ksql-local.sh)"
cargo test -p meshql-ksql
```

The suite **fails** rather than skips when no backend is reachable. It used to
return early, which exits 0, so the adapter reported success on every machine
that had never configured Confluent Cloud — hiding a temporal read that returned
the present and a searcher that ignored `at` entirely.

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

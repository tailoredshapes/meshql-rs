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

It also **refuses to run against anything but loopback**. The certification
creates and drops topics, and a developer with `CONFLUENT_*` already exported
for a real cluster would otherwise have `cargo test` do that to it without ever
saying so. No test in this repository talks to Confluent Cloud.

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

### Events, projections and workers

Entities are of three kinds (contract: `specs/egg-economy.md`), and a restlette is
told which when the server is built. The default is an ordinary entity (an actor).

| kind | declared | what the HTTP surface does |
|---|---|---|
| actor | `WriteMode::Open` (the default) | create, replace, delete, as the auth plugin allows |
| event | `WriteMode::Event` | `POST` creates it; `PUT` and `DELETE` answer 405 |
| projection | `WriteMode::ProducedBy(vec!["hen-worker".into()])` | `POST`, `PUT`, `DELETE` answer 403 unless the session is one of the named principals |

```rust
let mut modes = WriteModes::new();               // restlette path -> mode
modes.insert("/lay_report/api".into(), WriteMode::Event);
modes.insert("/hen_productivity/api".into(), WriteMode::ProducedBy(vec!["hen-worker".into()]));
let app = meshql_server::build_app_with_modes(config, auth, Router::new(), modes).await?;
```

`build_app_with_auth` is `build_app_with_modes` with no modes, so existing
servers are unchanged. "Is the caller the principal `hen-worker`" is answered by
the auth plugin, as every authorization question is: `Session::is_principal`
(the token plugin answers yes when one of the caller's tokens is that name; a
plugin that does not implement it leaves a produced entity closed to every HTTP
caller, never open). The worker writes through the restlette with its own
credentials, so its writes pass the same checks and leave the same versions as
any other. How an event gets to a worker (Debezium and Kafka, `merkql-connect`, a
trigger and a queue table) is the deployment's: the library neither starts nor
knows any of it. The names follow meshql-fib's `(:event)` and `(:produced-by ..)`.

### Certification

Storage is certified per plugin by the storage contract (repository, searcher,
versions, authorization: `cargo test -p meshql-<store> --test repo_cert` and
friends). The end to end example is not repeated per store: meshql is storage
agnostic and that is what those certs prove, so the pyramid has one e2e, on the
cheapest store, SQLite. It is the **egg economy**
(`meshql-cert/src/egg_economy.rs`: fourteen entities, five actors, five events,
four projections), which replaced the farm:

```bash
cargo test -p meshql-sqlite --test egg_economy_cert
```

It runs the contract's `egg_economy.feature` (events are create-only, a
projection is closed to the front end and open to its worker, the graph federates
and answers `at:`) and `meshql-cert/tests/features/egg_graph.feature` (this
repository's scenarios over all fourteen graphlettes). The CDC and the reference
hen worker (`egg_economy/worker.rs`) belong to the harness and are not under
test: they let the surfaces have data to serve.

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

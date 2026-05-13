# MeshQL-RS Project Context

MeshQL-RS is a Rust implementation of the [MeshQL](https://tailoredshapes.github.io/meshql/) framework. It enables building data services where entities are automatically exposed via both REST and GraphQL endpoints, with built-in support for data federation and point-in-time (temporal) queries.

## Project Structure

The project is organized as a Rust workspace:

- **`meshql-core`**: Defines the foundational traits and data structures.
  - `Repository`: Trait for CRUD operations and point-in-time reads.
  - `Searcher`: Trait for query-based data retrieval.
  - `Envelope`: The internal data wrapper containing `id`, `payload` (JSON), `created_at`, and metadata.
  - `Config`: Configuration structures for defining entities (Graphlettes and Restlettes).
- **Adapters (Persistence)**:
  - `meshql-postgres`, `meshql-mysql`, `meshql-sqlite`: SQL-based implementations using `sqlx`.
  - `meshql-mongo`: MongoDB implementation.
  - `meshql-merkql`: In-memory/filesystem implementation.
- **Service Layers**:
  - `meshql-restlette`: Implementation of REST endpoints using `axum`.
  - `meshql-graphlette`: Implementation of GraphQL endpoints and federated resolvers using `async-graphql`.
  - `meshql-server`: Orchestrator that assembles Restlettes and Graphlettes into a complete web server.
- **Testing & Quality**:
  - `meshql-cert`: A BDD (Cucumber) test suite used to "certify" that different adapter implementations behave consistently according to the MeshQL spec.
- **`examples/`**: Demonstrates the framework in various scenarios (e.g., `farm` for basic federation, `egg-economy` for complex event-sourcing patterns).

## Key Commands

### Building
```bash
cargo build
```

### Testing
```bash
# Run all unit and integration tests
cargo test

# Run a specific example (e.g., the farm example)
cargo run -p farm
```

### Certification Tests
Many crates have a `tests/` directory with `*_cert.rs` files. these use the `meshql-cert` crate to run Cucumber features against the local implementation.
```bash
# Example: Run repository certification for SQLite
cargo test -p meshql-sqlite --test repo_cert
```

## Development Conventions

- **Traits First**: Persistence layers must implement `Repository` and/or `Searcher` traits from `meshql-core`.
- **Envelopes**: All data stored and retrieved is wrapped in an `Envelope`.
- **Async/Await**: The entire codebase is async, primarily using `tokio` and `axum`.
- **Certification**: When adding a new storage adapter or making core changes, ensure that the `meshql-cert` test suite passes for the affected components.
- **Error Handling**: Uses `thiserror` for defining custom error types within crates and `anyhow` for top-level application logic.
- **Formatting**: Adheres to standard `rustfmt` conventions.

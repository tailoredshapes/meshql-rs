# Storage Adapters: Repository, Searcher, and Certification

A datastore plugs into meshql by implementing two traits from `meshql-core/src/lib.rs`. Existing adapters: `meshql-mongo`, `meshql-postgres`, `meshql-mysql`, `meshql-sqlite` (smallest — best starting reference), `meshql-merkql`, `meshql-merksql`, `meshql-ksql`.

## The traits

```rust
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope>;
    async fn read(&self, id: &str, tokens: &[String], at: Option<DateTime<Utc>>)
        -> Result<Option<Envelope>>;
    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool>;
    async fn create_many(&self, envelopes: Vec<Envelope>, tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove_many(&self, ids: &[String], tokens: &[String]) -> Result<HashMap<String, bool>>;
}

#[async_trait::async_trait]
pub trait Searcher: Send + Sync {
    async fn find(&self, template: &str, args: &Stash, creds: &[String], at: i64)
        -> Result<Option<Stash>>;
    async fn find_all(&self, template: &str, args: &Stash, creds: &[String], at: i64)
        -> Result<Vec<Stash>>;
}
```

## Semantics every adapter MUST honor

These are the contract, enforced by the certification suite:

1. **Append-only versioning.** `create` with an existing `id` appends a new version (newer `created_at`); it never overwrites. Reads return the latest version per id.
2. **Temporal reads.** `read(.., at: Some(t))` / `find(.., at)` return the latest version with `created_at <= t`. `at: None` (or current-time millis) means "now". The canonical query shape is a window per id: `ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at DESC)` filtered to row 1 (SQL adapters), or the equivalent `$sort`+`$group` aggregation (Mongo) — applied *after* the `created_at <= at` filter.
3. **Soft delete.** `remove` appends a tombstone version (`deleted: true`). All read paths exclude records whose *latest applicable version* is deleted. A temporal read *before* the deletion still returns the record.
4. **Token filtering on every read.** A record is visible iff its `authorized_tokens` is empty, contains `"*"`, the caller holds `"*"`, or the sets intersect (see `envelope_visible_to` in `meshql-core/src/auth.rs`). `create` stamps the caller's tokens onto the envelope.
5. **Template rendering.** Searchers render the Handlebars template with `args` into a JSON object, then translate keys: `id` → envelope id column/field; anything else → payload path (e.g. Postgres: `(payload::jsonb)->>'breed' = $n`, see `meshql-postgres/src/query.rs`). Always parameterize — never splice rendered values into SQL.
6. **Searcher returns payloads, not envelopes.** `find`/`find_all` return the payload Stash with `id` merged in, ready for GraphQL resolution. Also merge in `createdAt` (RFC3339 string, from the Envelope's `created_at`) right next to `id` — this is what lets a `.graphql` type opt into the "honesty" as-of field (`createdAt: String`) with zero resolver code, since `meshql-graphlette`'s scalar field resolver is a generic `stash.get(&field_name)` lookup. Never merge in `deleted` — search results already exclude deleted/superseded versions, so there's nothing to expose.

## Adding a new adapter

1. New workspace crate `meshql-<store>` (add to root `Cargo.toml` members), depending on `meshql-core`.
2. Constructor convention: `<Store>Repository::new(uri, db, collection, auth: Arc<dyn Auth>)` and matching `<Store>Searcher::new(...)` — mirrors Mongo/Postgres so examples stay copy-paste portable.
3. Implement both traits per the semantics above. `meshql-sqlite/src/` is the smallest complete reference (~250 LOC for both).
4. **Certify.** Wire the shared behavior tests from `meshql-core/src/testing.rs` against your adapter. Existing adapters carry `tests/repo_cert.rs`, `tests/searcher_cert.rs`, and `tests/farm_cert.rs` (copy the pattern from `meshql-sqlite/tests/`): `cargo test -p meshql-<store> --test repo_cert --test searcher_cert`. The Cucumber suite in `meshql-cert` provides BDD-level coverage. An adapter is not done until certification passes — it covers create/read/list/remove, bulk ops, temporal reads, soft delete, and token visibility.

## What does NOT belong in an adapter

- Business validation, defaults, side effects → restlette layer (`build_restlette_router_ext`).
- Cross-entity joins → graphlette resolvers (federation).
- Aggregations/read models → projection entities or `run_ext` extra routes.

Adapters translate Envelope semantics to a store. Nothing else.

# Storage Adapters: Repository, Searcher, and Certification

A datastore plugs into meshql by implementing two traits from `meshql-core/src/lib.rs`. Existing adapters: `meshql-mongo`, `meshql-postgres`, `meshql-mysql`, `meshql-sqlite` (smallest — best starting reference), `meshql-merkql`, `meshql-merksql`, `meshql-ksql`, `meshql-dynamo`, `meshql-merk`.

Two of them are deliberately not general-purpose, and both are worth knowing about before you pick one:

- **`meshql-dynamo`** implements both traits and passes every suite, but `read`/`read_many` are single-`Query` point reads while **every non-`id` search predicate is a full table `Scan`** — arbitrary-attribute equality plus latest-version-per-id plus a temporal cutoff has no expression in a key-value store's key model, and the template language has no way to declare an index. Fine for point reads and small projections; not a substitute for an indexed store behind a wide search surface.
- **`meshql-merk`** implements `Repository::create`/`create_many` and **nothing else** — every read path returns an error and there is no `Searcher` at all. It backs create-only event meshlettes on object storage, where a search would scan the log from offset zero and download it whole. It does not pass repository certification and is not meant to: it is a write-side adapter, paired with an indexed store on the read side.

Certification is still the contract for the parts they implement. An adapter narrower than the traits should say so in its own docs and be unable to pretend otherwise — see `meshql-merk`'s `tests/structural_guards.rs`.

## The traits

```rust
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope>;
    async fn read(&self, id: &str, session: &dyn Session, at: Option<DateTime<Utc>>)
        -> Result<Option<Envelope>>;
    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>>;
    async fn remove(&self, id: &str, session: &dyn Session) -> Result<bool>;
    async fn create_many(&self, envelopes: Vec<Envelope>, session: &dyn Session) -> Result<Vec<Envelope>>;
    async fn read_many(&self, ids: &[String], session: &dyn Session) -> Result<Vec<Envelope>>;
    async fn remove_many(&self, ids: &[String], session: &dyn Session) -> Result<HashMap<String, bool>>;
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>>;
    async fn read_version(&self, id: &str, token: &str, session: &dyn Session)
        -> Result<Option<Envelope>>;
}

#[async_trait::async_trait]
pub trait Searcher: Send + Sync {
    async fn find(&self, template: &str, args: &Stash, session: &dyn Session, at: i64)
        -> Result<Option<Stash>>;
    async fn find_all(&self, template: &str, args: &Stash, session: &dyn Session, at: i64)
        -> Result<Vec<Stash>>;
}
```

**Storage holds no credentials.** `tokens` used to be a parameter on every one
of these methods, and handing every adapter author the credentials made
answering the authorization question their job — eleven adapters answered it
eleven ways and nothing detected the difference, because a wrong answer looks
exactly like a correct one from outside. What an adapter gets now is a
`Session` it can only ask, never interpret. See
`meshql-cert/tests/features/contract/specs/auth-plugin-owns-authorization.md`.

## Semantics every adapter MUST honor

These are the contract, enforced by the certification suite:

1. **Append-only versioning.** `create` with an existing `id` appends a new version (newer `created_at`); it never overwrites. Reads return the latest version per id.
2. **Temporal reads.** `read(.., at: Some(t))` / `find(.., at)` return the latest version with `created_at <= t`. `at: None` (or current-time millis) means "now". The canonical query shape is a window per id: `ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at DESC)` filtered to row 1 (SQL adapters), or the equivalent `$sort`+`$group` aggregation (Mongo) — applied *after* the `created_at <= at` filter.
3. **Soft delete.** `remove` appends a tombstone version (`deleted: true`). All read paths exclude records whose *latest applicable version* is deleted. A temporal read *before* the deletion still returns the record.
4. **Ask the plugin on every read; never answer for it.** A record is visible iff `session.is_authorized(Operation::Read, &envelope)` says so. The adapter does not know the rule and must not try to reconstruct one: the envelope's `auth` is an opaque `AuthMark` that storage persists verbatim, inside the same write as the payload, and never reads. `create` hands the envelope to `session.stamp(..)` and stores whatever comes back. `remove` resolves the record under `SystemSession` and then asks `Operation::Remove`, so remove is a real question rather than a synonym for read; the tombstone keeps the mark of the record it buries, so the change feed can still say who was entitled to hear about the deletion. There is **no unset session** — a caller outside a request (a worker, the change feed, a migration, a test inspecting storage) names `meshql_core::SystemSession` explicitly.
5. **Template rendering.** Searchers render the Handlebars template with `args` into a JSON object, then translate keys. There are exactly **two** recognised key shapes, and a new adapter must honour both: `"id"` → the envelope id column/field, and `"payload.<field>"` → the payload path (Postgres: `(payload::jsonb)->>'breed' = $n`, see `meshql-postgres/src/query.rs`; SQLite: `json_extract(payload, '$.breed') = ?`; merkql: a literal dot-path against the serialized Envelope, `meshql-merkql/src/matcher.rs`). **A payload field written bare, without the `payload.` prefix, is not a supported template** — see SKILL.md, "Query templates: `payload.` is not optional". Existing adapters differ in how they fail on one (skip vs. no-match vs. SQL error), all of them silently; prefer to fail loudly if you are writing a new one. Always parameterize — never splice rendered values into SQL.
6. **Searcher returns payloads, not envelopes.** `find`/`find_all` return the payload Stash with `id` merged in, ready for GraphQL resolution. Also merge in `createdAt` (RFC3339 string, from the Envelope's `created_at`) right next to `id` — this is what lets a `.graphql` type opt into the "honesty" as-of field (`createdAt: String`) with zero resolver code, since `meshql-graphlette`'s scalar field resolver is a generic `stash.get(&field_name)` lookup. Never merge in `deleted` — search results already exclude deleted/superseded versions, so there's nothing to expose.
7. **Authorization is fetch-then-ask, and `limit` comes last.** An opaque mark cannot build a `WHERE` clause, so nothing is pushed into the query — not the visibility filter, and therefore not the `limit` either. Fetch, ask the session about each resolved envelope, *then* truncate, so a limit returns N **authorized** rows rather than being consumed by rows the caller never gets to see. A plugin may offer a translatable pushdown hint as an optimization; none does today, and an adapter that cannot translate a hint must ignore it (ignoring is always correct, misreading is not).
8. **Canonical result ordering.** A result set comes back in insertion order: sort by the *resolved* version's `created_at` (millisecond), tiebroken by envelope `id`, byte-ordered — `meshql_core::envelope_order`, certified by the ordering cases in `meshql-core/src/testing.rs`. Because the key is the resolved version's timestamp rather than the id's first appearance, a `limit` truncates a meaningful prefix instead of an arbitrary subset. Adapters that *do* have a monotonic physical sequence (merkql log offset, SQLite `rowid`) use it only to decide *which version* of an id resolves inside a millisecond — never as the primary sort key, because Postgres/MySQL/Mongo/ksql have no equivalent and cross-adapter equivalence is worth more than sub-millisecond fidelity.

## Don't reach underneath the abstraction

The certification suite is not a smoke test; it is the **contract**. Choosing a storage backend is about what you already operate, what your team knows, and what you like — not about capability. There is no practical difference in behaviour between the implementations, and that is precisely what certification exists to guarantee. The property worth protecting: **you should be able to swap the database engine and replay the queue into the new store without any end user noticing a difference.**

So: **never depend on a storage-engine property the certification does not guarantee.**

A recent design picked SQLite specifically so a worker could tail `_id INTEGER PRIMARY KEY` as a CDC cursor — the reasoning being that SQLite serialises writers, so `_id` order is commit order. It works, and it is reaching *underneath* the abstraction. Nothing in `Repository`/`Searcher` exposes a physical row id; nothing in certification promises one exists, or that it is dense, or that it is monotonic with commit order; Postgres, Mongo and ksql have no equivalent to offer. The moment that cursor exists, swap-and-replay is gone and the product is welded to one engine — for a property that was never part of the deal.

The replacements, in order of what you were actually trying to do:

- **Need a stable order over results?** Use the certified canonical order (semantics 7 above: `created_at`, then `id`). Every adapter guarantees it.
- **Need a durable consumption position?** That's the queue's offset, not a row id — and it is checkpointed with the fold state as one artifact (see `references/domain-design.md`).
- **Need events on a log at all?** That's the CDC bridge or merkql-as-primary-store, not a hand-rolled tail (same file).

## Adding a new adapter

1. New workspace crate `meshql-<store>` (add to root `Cargo.toml` members), depending on `meshql-core`.
2. Constructor convention: `<Store>Repository::new(uri, db, collection, auth: Arc<dyn Auth>)` and matching `<Store>Searcher::new(...)` — mirrors Mongo/Postgres so examples stay copy-paste portable.
3. Implement both traits per the semantics above. `meshql-sqlite/src/` is the smallest complete reference (~250 LOC for both).
4. **Certify.** Wire the shared behavior tests from `meshql-core/src/testing.rs` against your adapter. Existing adapters carry `tests/repo_cert.rs`, `tests/searcher_cert.rs`, and `tests/farm_cert.rs` (copy the pattern from `meshql-sqlite/tests/`): `cargo test -p meshql-<store> --test repo_cert --test searcher_cert`. The Cucumber suite in `meshql-cert` provides BDD-level coverage. An adapter is not done until certification passes — it covers create/read/list/remove, bulk ops, temporal reads, soft delete, and token visibility.

## Three gaps certification does not close

Found by deliberately breaking each guard while writing `meshql-dynamo` and watching what went red. **A green certification does not currently rule any of these out**, so an adapter author has to get them right without help.

1. **Unrecognised template key: empty vs wide.** No cert case exists. `meshql-merkql` fails *empty* (the dot path resolves to `None`, the record is rejected); the SQL adapters *skip* the condition, so a single mistyped key — `{"kind": "x"}` instead of `{"payload.kind": "x"}` — returns **every** record. Both certify clean. Prefer empty: failing wide on a typo is an authorization-shaped bug, because a searcher's result set is what a graphlette hands a caller. Pin it with your own test (`meshql-dynamo/src/matcher.rs`, `meshql-dynamo/src/searcher.rs`).

2. **The `at` cutoff is only ever exercised on millisecond boundaries.** Every seeded `created_at` in `testing.rs` comes from `DateTime::from_timestamp_millis` or is separated by whole seconds, so an adapter whose cutoff is off by one millisecond passes all of it. The case that would catch it: two versions of one id at `T` and `T + 400µs`, then `read(.., Some(T_ms))` asserting the *second* comes back, since the contract is `created_at_ms <= at_ms`. Worth writing against your own adapter until the shared suite has it.

3. **Tombstone marks are now specified: the tombstone keeps the buried record's mark.** This used to be unspecified and the adapters disagreed — `meshql-sqlite` and `meshql-postgres` built `deleted_env` with the original tokens and then handed it to `create`, which overwrote them with the caller's, so the code's visible intent was the opposite of its behaviour. Every adapter now writes the tombstone under `SystemSession`, whose `stamp` leaves the envelope alone. Keep it that way: re-stamping a tombstone answers for the plugin, and the change feed reads that mark to decide who hears about the delete.

## What does NOT belong in an adapter

- Business validation, defaults, side effects → restlette layer (`build_restlette_router_ext`).
- Cross-entity joins → graphlette resolvers (federation).
- Aggregations/read models → projection entities or `run_ext` extra routes.

Adapters translate Envelope semantics to a store. Nothing else.

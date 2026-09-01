# Listing a document's versions

**Date:** 2026-09-01
**Status:** Approved, implementing
**Scope:** meshql-rs, meshql (Java), meshobj (TypeScript)

## The problem

meshql keeps every version of every document. An envelope is immutable, a write
appends a new one under the same id, and reads resolve the newest version at or
before a cutoff. That history is the platform's answer to any question of the
form "what did this say in March", and it is why a caller never needs to build
their own audit trail.

Nothing can read it. There is no way to enumerate the versions of a document:
no method on `Repository`, no route on the restlette. A caller who knows a
version exists cannot find out when, and a caller who does not know cannot find
out that it does.

## The second problem, which is worse

Even with a listing, a version has no address.

`created_at` is millisecond precision, so two versions of one document can
genuinely tie. `envelope_order` breaks ties on the envelope id, which does real
work across a result set — where at most one version per id resolves — and none
at all within one document's history, where the id is a constant by definition.

The adapters paper over this inconsistently:

| Adapter | Tiebreak in `read` |
|---|---|
| SQLite | `ORDER BY created_at_ms DESC, rowid DESC` |
| Postgres | `ORDER BY created_at_ms DESC` — none |
| Mongo | `$sort: { createdAt: -1 }` — none |

So `read(id, at:)` is **already nondeterministic** on Postgres and Mongo when
two versions share a millisecond. This is a live correctness bug, not a
consequence of the feature below. Any workload that writes a document twice in
quick succession hits it; a projection worker folding a log does so routinely.

## The design

### A version is addressed by URL, and the client never builds one

```
GET /<entity>/api/<id>/versions
```

returns the versions of that document, oldest first, each with a URL that
resolves to exactly that version.

The client treats the URL as opaque. It follows one; it never constructs one.
That is what keeps the identity problem on the server, where each
implementation can solve it with what its store actually offers.

```json
{
  "id": "fa4fdd02-2205-4f99-a1c1-33ba6fc9f31a",
  "versions": [
    { "url": "/deployable/api/fa4fdd02.../versions/9f2b...", 
      "created_at": "2026-08-31T09:14:22.418Z", "deleted": false },
    { "url": "/deployable/api/fa4fdd02.../versions/1c77...",
      "created_at": "2026-08-31T09:14:22.418Z", "deleted": false },
    { "created_at": "2026-08-31T11:02:07.900Z", "deleted": false,
      "unauthorized": true }
  ]
}
```

Note the first two entries: same millisecond, distinct URLs. That is the case
the whole design exists to serve.

### The token is derived from content, not from the store

A natural per-row key does not exist everywhere. SQLite has `rowid` and Mongo
has `_id`, but the Postgres and MySQL tables carry only the five envelope
columns, and `ctid` is not stable across a `VACUUM`. Adding a sequence column to
every adapter is a migration on every deployment of three implementations.

So the token is a hash over the fields that identify a version:

```
token = sha256(id ‖ created_at_ms ‖ deleted ‖ authorized_tokens ‖ payload)
```

Two versions collide only when every one of those is identical, which means
they are the same version recorded twice and indistinguishable by any means.

This buys a property worth having on its own: a version URL survives a
migration between adapters. Move a deployment from SQLite to Postgres and every
version URL still resolves, because the token never depended on the store.

### Ordering is oldest first

The list matches the log. An index into it stays stable as versions append,
where newest-first renumbers the whole history on every write.

Within a millisecond the order is the token, byte-ordered. It is arbitrary but
identical across adapters and stable across replays, which is what the cert
suite pins. Arbitrary-but-agreed beats the current
arbitrary-and-adapter-specific.

### Deleted versions appear

A delete is a version carrying `deleted: true`. A history that hides them
misrepresents what happened, and "when did this go away" is one of the questions
the feature exists to answer.

### A version the caller cannot read appears as a tombstone

Reads filter on the envelope's tokens, so a caller can be entitled to some
versions of a document and not others. Silently omitting the rest makes the
history look continuous when it is not.

Such an entry carries `created_at`, `deleted`, and `"unauthorized": true`, and
no URL. The caller learns that something happened and is told they cannot see
it, which is the honest answer.

### No pagination

No REST surface in meshql paginates. `Repository::list` returns everything and
`list_handler` takes no parameters; the graph side takes a `limit` template
argument with its own certified truncation order. Paginating exactly one REST
endpoint would be the odd surface out.

Revisit when a document is observed accumulating versions faster than a caller
can consume them. Until then this is a deliberate omission, not an oversight.

## The surface

### Repository

```rust
async fn list_versions(&self, id: &str, tokens: &[String]) -> Result<Vec<VersionRef>>;
async fn read_version(&self, id: &str, token: &str, tokens: &[String]) -> Result<Option<Envelope>>;
```

```rust
pub struct VersionRef {
    pub token: String,
    pub created_at: DateTime<Utc>,
    pub deleted: bool,
    /// False when the caller's tokens do not authorize this version. The entry
    /// still appears, without a token to dereference.
    pub authorized: bool,
}
```

`list_versions` returns every version, oldest first. `read_version` resolves one
and applies the same authorization as `read`.

### Restlette

```
GET /<entity>/api/<id>/versions          -> the list above
GET /<entity>/api/<id>/versions/<token>  -> the same shape as GET /<entity>/api/<id>
```

A version URL returns the payload flat with `id` merged, matching the existing
item route. A caller following a version URL gets a document, not an envelope.

Unknown token: 404. Unauthorized version: 403, not 404 — the caller has already
been told it exists.

## The tie-break fix, which ships alongside

`read(id, at:)` must resolve the same version on every adapter. With a
content-derived token available, the secondary sort is the token:

```
ORDER BY created_at_ms DESC, token DESC
```

Every adapter applies both keys. This is a behaviour change on Postgres and
Mongo, where the second key previously did not exist, and no change on SQLite
except that `rowid` stops being consulted — which also removes the one place a
storage engine's physical row id leaked into a resolution rule.

## Conformance

The shared cert suite pins behaviour, never token format. An implementation is
conformant when, for the same sequence of writes:

1. The version count matches the number of writes.
2. The order is identical across implementations, oldest first.
3. Two versions written in one millisecond both appear, with distinct tokens.
4. Every URL dereferences to exactly one distinct document.
5. A deleted version appears with `deleted: true`.
6. A version the caller cannot read appears as a tombstone with no token.
7. `read(id, at: t)` resolves the same version on every adapter, including when
   two versions share the millisecond `t`.

Point 7 is the existing bug. It belongs in the same suite because the same
ordering rule fixes it.

## Rollout

meshql-rs first: `Repository`, the reference adapter, the restlette route, and
the cert tests. The shape is proven once, then ported to meshql and meshobj
against the same suite.

## Open

- Whether `list_versions` belongs on `Searcher` as well, so a graphlette can
  expose history. Out of scope here; the restlette answers the question asked.
- Whether the ksql and Dynamo adapters can honour the ordering rule without a
  secondary index. To be established while implementing, not assumed.

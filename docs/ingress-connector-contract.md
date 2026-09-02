# The ingress connector contract

Every claim here is grounded in source read in `/tank/repos/tailoredshapes/meshql-rs`
at commit `2fdd5d3` (main). File:line references are to that tree. Where the
existing `CommitSource` trait does not express something an enterprise SaaS
source needs, it is called out under "Where the trait does not fit" with a
proposed change — those sections are the point of this document.

---

## 1. What an ingress connector is

The three sources that exist today — `merkql-connect/src/sqlite.rs`,
`src/mongo.rs`, `src/postgres.rs` — all read a **meshql envelope table**. The
envelope already exists; the source's job is to notice it was committed and
carry it, unmodified, onto a topic. `sqlite.rs:219-225` and
`mongo.rs:97` (`meshql_mongo::converters::document_to_envelope`) both
*reconstruct* an envelope that the mesh already wrote.

An **ingress** connector reads a foreign system that has never heard of meshql.
There is no envelope to reconstruct, so the connector **synthesises** one. That
single difference is what changes the risk profile: every field of the envelope
is now a decision the connector author makes, and every one of those decisions
has a silent failure mode.

Both kinds implement the same trait (`source.rs:109-148`) and are driven by the
same loop (`sink.rs:271-332`). Nothing below asks you to fork that.

---

## 2. What the framework already guarantees — do not re-implement it

Read these before writing a line; a connector that re-implements any of them is
wrong.

| Guarantee | Where it lives | What it means for you |
|---|---|---|
| Append **then** commit the position | `sink.rs:309-323`, `offsets.rs:1-17` | You never write to the offset store. You put a position on `SourceInfo::position` and the loop does the rest. |
| An interrupted snapshot restarts cold | `offsets.rs:103-108` | A position staged with `snapshot_in_progress == true` is **discarded** on restart. See gap G2 — this is the single rule that hurts SaaS backfill most. |
| Unusable position → policy, never a silent restart | `source.rs:73-88`, `sink.rs:285-304`, `sink.rs:343-362` | You return `CdcError::UnusablePosition`. You do **not** decide what happens next. |
| One writer per merkql topic | `sink.rs:96-165` (`flock` + single-partition check) | Enforced structurally — but only on the merkql sink. See gap G4. |
| Offset file is fenced by connector+entity | `offsets.rs:66-75` | An offset file written by a different connector/entity pair is a startup error, not a silent adoption. |
| A record with no `after` is an error | `sink.rs:236-244` | Never emit a `ChangeRecord` with `after: None` to a repository sink. |
| Cross-backend behaviour is certified | `src/cert.rs:74-88` | There is a **certification suite for sources**. Use it. See §5. |

### The one difference between the two sinks, restated accurately

`sink.rs:26-39` says it and `sink.rs:236-256` implements it: `RepositorySink`
appends `record.after` — **the envelope alone**. `Repository::create` takes an
`Envelope` (`meshql-core/src/lib.rs:73`), so `op`, `ts_ms`, and the whole
`source` block (`record.rs:90-103`) are dropped on the floor for every non-merkql
queue.

The prompt's shared rule 2 is therefore **correct**, and stronger than it sounds:

- `op: r` vs `op: c` — backfill vs live — **does not reach the consumer** on a
  Kafka/ksql/Postgres queue. If a worker must suppress notifications during a
  backfill, the connector must materialise that into the payload.
- `source.position` — the native cursor — does not reach the consumer either. If
  a consumer wants to dedupe against the source system's own numbering rather
  than against the envelope id, that value must be in the payload.
- `source.ts_ms` — the *source system's* modification time — does not reach the
  consumer. `Envelope::created_at` is set by `Envelope::new`
  (`meshql-core/src/lib.rs:30-38`) to `Utc::now()` — **connector wall-clock, not
  domain time**. For an ingress connector doing a backfill of five years of
  history, every envelope will claim to have been created today. If domain time
  matters (it always does for events), put the source system's timestamp in the
  payload as an explicit field, and do not rely on `created_at`.

That last point is the one most likely to be missed, because on the merkql sink
`source.ts_ms` is present and it looks like the problem is solved.

---

## 3. The seven shared rules, checked against source

Each rule the three builders were given, verified, corrected, or extended.

### Rule 1 — Wrap at ingress. **Correct, and under-specified.**

`Envelope { id, payload, created_at, deleted, auth }`
(`meshql-core/src/lib.rs:20-27`). The connector supplies all five. Three of them
are traps:

- **`id`** — see §4, the id derivation rule. This is the rule with the highest
  blast radius and it is not stated anywhere in the crate.
- **`created_at`** — `Utc::now()` unless you construct the struct literally
  rather than via `Envelope::new`. `sqlite.rs:219-225` constructs it literally in
  order to preserve the store's timestamp.

  **For any source with unordered delivery this is a correctness requirement,
  not a nicety, and it is the sharpest silent failure in this document.**
  `envelope_order` (`meshql-core/src/lib.rs:64-69`) sorts by `created_at` with
  `id` as tiebreak, and a read "resolves each `id` to the latest version
  at-or-before the `at` cutoff" (`meshql-core/src/lib.rs:41-49`). So
  **`created_at` decides which version of a record wins.** HubSpot v3 webhooks
  have no ordering guarantee: version 2 of a contact can arrive before version 3
  and after version 4. Stamp `Utc::now()` and the *stale* version gets the later
  `created_at` and wins every read, permanently, with nothing anywhere reporting
  an error. Replaying the topic does not fix it — the wrong timestamps are in
  the envelopes.

  Rule: **`created_at` is the source system's modification timestamp.** If the
  source does not give you one, you cannot correctly ingest an unordered feed,
  and that is a finding to escalate rather than paper over.
- **`deleted`** — meshql expresses a delete as a **new envelope version with
  `deleted: true`** (`record.rs:18-22`, `meshql-patterns` invariant 2). If the
  source system's change feed surfaces deletes, this is where they go. If it does
  not surface deletes, say so in the connector's module docs — a connector that
  silently never emits tombstones produces a projection that grows forever and
  is wrong in a way nobody can detect from the topic.

### Rule 2 — The Debezium `source` block does not survive `RepositorySink`. **Correct.** See §2 above; the `created_at`/`ts_ms` consequence is the part to add.

### Rule 3 — The cursor is an opaque `String`. **Correct, and incomplete.**

`Resume::At(String)` (`source.rs:29`), stored verbatim
(`offsets.rs:24-34`), carried on `SourceInfo::position: Option<String>`
(`record.rs:100`). `mongo.rs:77-88` is the model: encode the server's own token
as JSON and never interpret it.

Two constraints the word "opaque" hides, both enforced by the cert suite:

1. **Positions must be distinct per record** (`cert.rs:160-186`: `certify_positions_are_present_and_distinct` fails if any two of three consecutive records share a position). A bare `updated_at`/`SystemModstamp`/`hs_lastmodifieddate` watermark **does not satisfy this** — SaaS systems routinely stamp bulk edits with the same millisecond. A tie is not a cosmetic failure: `WHERE modified > cursor` skips the tied records permanently (a gap), and `>= cursor` re-delivers them forever (a livelock at the tie). The cursor must be a **server-issued token**, or a **composite `(timestamp, id)` with a total order the query can express**, never a bare timestamp.
2. **A record may carry `position: None`, and sometimes it must.** `sink.rs:313` stages nothing when the position is absent. `mongo.rs:224-234` uses this deliberately during a snapshot. The generalisation is a rule nobody has written down, and it is a silent gap if broken: **when one source event fans out into several `ChangeRecord`s, only the last may carry a position; the earlier ones must carry `None`.** The loop stages a position after each individual append (`sink.rs:311-323`), and `Resume::At(P)` means "everything through P is done". So if a fan-out emits A and B both carrying position P, and the process dies after A is appended and P committed, the restart resumes after P and **B is never delivered** — not a duplicate, a permanent loss. This is not hypothetical for ingress: a HubSpot merge fans out into one tombstone per merged-away id plus the surviving record (§4), and a batched poll response yielding N records from one HTTP call has the same shape if you give each of them the page cursor.

3. **A composite cursor is legitimate — encode it, do not concatenate it.** The string is opaque, so a source needing to carry more than one value (Salesforce must track the Avro `schema_id` alongside the replay id; a windowed poller must track both the window and the offset within it) may serialise a struct into it. `mongo.rs:77-88` is the precedent: JSON, round-tripped, never interpreted by anything but the source. **Version the encoding.** A connector upgrade that changes the cursor's shape turns every stored position into a parse failure, which surfaces as `UnusablePosition` — so `when_needed` silently re-snapshots the whole org on deploy and `initial`/`never` refuse to start. A version tag plus a tolerant reader for the previous shape costs nothing and stops an upgrade looking like a data-loss incident.

### Rule 4 — Unusable position → `CdcError::UnusablePosition`, never a silent restart; `when_needed` recovers, `initial`/`never` hard-fail. **Correct.**

Verified: `source.rs:60-71`, `sink.rs:290-296` (mid-stream), `sink.rs:346-356`
(at startup), tested at `sink.rs:586-635`.

What this means for a SaaS source concretely: an **expired replay id / delta
token / cursor** is `UnusablePosition`. A **401** is not — that is an auth
failure, not a position failure (see G5). A **429** is not either (see G1).
Misclassifying a 429 as `UnusablePosition` under `when_needed` triggers a full
re-snapshot of the org on a transient throttle, which is how you turn a rate
limit into an outage.

### Rule 5 — Secrets from the environment, never the TOML. **Correct as a rule, but the repo's precedent is mixed — do not cite the existing sources as justification.**

The rule is stated at `config.rs:90-92` and implemented at `main.rs:83-88`
(`KsqlConfig::from_env`). But `SourceConfig::Postgres { conn: String }`
(`config.rs:141-154`) is a libpq connection string in the TOML file, and a libpq
connection string carries a password. The rule is right; the codebase is not yet
consistent with it. New source variants should hold **non-secret coordinates
only** — instance URL, object type, portal/client id — and read every credential
from the environment.

### Rule 6 — At-least-once; duplicates are cheap, gaps are permanent. **Correct.** `lib.rs:55-58`, `offsets.rs:9-13`.

### Rule 7 — No network in tests; `wiremock`. **Correct, and note that `wiremock` is not yet a dependency.**

`merkql-connect/Cargo.toml:46-62` has no `wiremock`. Every builder will add it to
`[dev-dependencies]` and every builder will conflict there. See §6.

---

## 4. The id derivation rule

Not present anywhere in the crate today. It should be, because it is the rule
whose violation is silent, permanent, and unfixable by replay.

**The envelope id is not a label. It is the identity meshql versions by and the
key merkql partitions by.**

- meshql treats two envelopes sharing an `id` as **two versions of the same
  entity**; a read resolves the latest version at-or-before the cutoff
  (`meshql-core/src/lib.rs:41-49`).
- the merkql producer key **is** the envelope id, hardcoded
  (`record.rs:140-154`, `sink.rs:179`).

So the id has four requirements, and every SaaS system violates at least one of
them if you use its raw record id:

1. **Stable.** Derived only from fields that cannot change for the life of the
   source record. An id containing a modification timestamp turns every update
   into a *new entity* instead of a new version — the projection ends up with N
   copies of one contact and nothing anywhere reports an error.
2. **Globally unique across everything that shares the topic.** Two different
   source records that derive to the same id become versions of each other, and
   the older one silently disappears from every read.
3. **Canonical.** One source record must produce exactly one id **string**. If
   the source system can hand you the same record under two spellings, you must
   normalise before you hash or concatenate.
4. **Derivable from a delete notification.** Deletes usually arrive with less
   data than creates — often only the record id and the object type. If your id
   needs a field that a delete event does not carry, you cannot write the
   tombstone.
5. **Actually stable in the source system — verify, do not assume.** This is the
   clause I originally folded into (1), and it deserves its own line because the
   hard case is not "did I pick a mutable field", it is "**the vendor renames
   records**". Since January 2025 a HubSpot merge creates a *brand-new record
   with a new id*; the old ids survive only as aliases that resolve to it. No
   field you chose is mutable, and the identity still moved.

   So the question to answer before writing any mapping code is not "is my key
   stable?" but:

   > **Is the source's natural key stable over the record's lifetime, and if it
   > is not, which event renames it?**

   Salesforce's 15-vs-18-char encoding and SAP's composite keys are the easy
   cases — they are *encoding* problems, solved once by a canonicalisation
   function. Identity *mutation* is the hard case and it needs a runtime answer,
   because meshql cannot retroactively rename an envelope id: the id is the
   version chain (`meshql-core/src/lib.rs:41-49`) and the merkql producer key
   (`record.rs:140-154`).

   The only thing expressible in meshql's model is to **emit the rename as
   records**: one tombstone (`deleted: true`) per merged-away id, plus the
   surviving record under its new id. Which makes the fan-out position rule in
   §3 rule 3 load-bearing — only the last of those records may carry a position,
   or a crash mid-fan-out loses the tombstones and the merged-away records stay
   alive in every projection forever.

   A connector against a source that renames records and does **not** consume the
   rename event produces duplicates that no replay can repair. If the rename
   event is unavailable, that is a documented permanent hole, not an
   implementation detail.

### The rule

> **`id = "{system}:{object_type}:{canonical_natural_key}"`**, where
> `canonical_natural_key` is produced by a single pure function, documented with
> the reason it is canonical, and unit-tested for the collision it prevents.

The three in-flight builds each hit a different clause of this rule, which is
why the rule needs all four:

- **Salesforce — clause 3 (canonical).** Salesforce record ids exist in a
  15-character case-**sensitive** form and an 18-character case-**insensitive**
  form with a checksum suffix; different APIs return different lengths.
  Concatenating whichever one you got yields two ids for one record. Normalise
  to 18 characters at the boundary, always, with a tested conversion — and
  assert in the test that the 15-char and 18-char forms of the same record
  produce the same envelope id.
- **HubSpot — clauses 2 and 5.** Object ids are numeric and scoped to the object
  type; a contact and a company can both be `12345`, so the full key is
  `(portalId, objectTypeId, objectId)` and `objectId` must be carried as a
  **string** (it is not safely an integer, and a JSON number round-trip is a
  precision hazard). The portal is part of the key because one connector build
  will eventually face two tenants. And per clause 5, HubSpot **mutates
  identity on merge** — a connector that does not consume `*.merge`
  (`primaryObjectId`, `mergedObjectIds`, `newObjectId`) accumulates duplicates
  that replay cannot repair.
- **SAP — clauses 2 and 3 (composite keys).** SAP business objects are keyed by
  tuples, typically including the client. Joining components with a separator is
  only safe if the separator cannot occur in a component *or* each component is
  escaped/fixed-width — otherwise `("A", "BC")` and `("AB", "C")` collide.
  Choose a separator that is illegal in every component and assert that in the
  test, or escape.

### Anti-rule

Do **not** hash the whole record to make an id. It satisfies uniqueness and
destroys stability: every edit produces a new id, so nothing is ever a new
version of anything, and the delete notification cannot reproduce the hash.

---

## 5. Where the `CommitSource` trait does not fit an enterprise SaaS source

Ranked by consequence. Each is confirmed or refuted against the real trait.

*(Ranking note: the sections are numbered G1–G9 in the order they were first
written. The **ranked** list is §8, and it does not match this order — G2 and G9
outrank G1. Read §8 for priority.)*

### G1 — `run_connector` has no retry, so a 429 or a 503 kills the process. **Confirmed.**

`sink.rs:305`: `Err(e) => return Err(e.into())`. Any `CdcError::Backend`
terminates the loop and `main.rs:52-58` returns it from `main`. There is no
backoff anywhere in the crate: the only `sleep` is the Postgres heartbeat tick
(`postgres.rs:850`), and it is a WAL-retention mechanism, not a retry.

For a database source that is defensible — a supervisor restarts the process and
it resumes from the committed offset. For a SaaS source it is not, and the reason
is G2: a 429 arriving during a backfill does not cost you a restart, it costs you
the **entire backfill**, and the restart then hits the same rate limit at the
same point. That is a livelock, not a retry loop.

**Proposed change** (smallest thing that works, in order of preference):

1. **Do not put retry in the trait.** Put it in the connector's HTTP client:
   respect `Retry-After`, exponential backoff with jitter, a bounded attempt
   count, and surface *only* the exhausted case as `CdcError::Backend`. This
   requires no framework change and is the right layer — the trait's job is to
   describe a feed, not a transport.
2. **Do add one thing to the framework**: `run_connector` should not be the only
   thing standing between a transient error and process death. A
   `RetryPolicy`-shaped parameter, or at minimum a documented statement that a
   source is responsible for its own transient-error handling and that any
   `CdcError::Backend` reaching the loop is fatal by design. Today that
   statement exists nowhere and a builder will reasonably assume the loop
   retries.

### G2 — A snapshot cannot be resumed, and for SaaS the snapshot is the expensive part. **Confirmed. Structural, not an oversight to work around.**

`offsets.rs:103-108`:

```rust
pub fn resume(&self) -> crate::Resume {
    match &self.stored {
        Some(s) if !s.snapshot_in_progress => crate::Resume::At(s.position.clone()),
        _ => crate::Resume::Cold,
    }
}
```

A position staged mid-snapshot is **read back and thrown away**. The reasoning
(`offsets.rs:96-102`, `record.rs:46-56`) is sound for a table scan: a
mid-snapshot position names a row *inside* the snapshot and resuming the live
stream there would start it at a point the stream never reached. For a SQLite
table the cost of re-snapshotting is milliseconds.

For a SaaS backfill the cost is hours and a large fraction of a daily API quota,
and the probability of completing without a single transient failure falls
towards zero as the org grows. Combined with G1: every 429 during a backfill
discards the backfill.

**A source cannot honestly work around this today.** It could encode a composite
position (`{"phase":"snapshot","page":…,"stream_at":…}`) into the opaque string —
but it would have to stage it with `snapshot_in_progress == false` for `resume()`
to hand it back, and that flag is precisely what tells the framework "this is not
a stream position". Lying about it re-introduces the silent gap the flag exists
to prevent.

**Proposed change:** make the distinction a type rather than a boolean.

```rust
pub enum Resume {
    Cold,
    /// A stream position: everything at or before it is on the topic.
    At(String),
    /// A *snapshot* position — meaningful only to the source that emitted it,
    /// and never interpretable as a stream position. A source that does not
    /// support resumable snapshots treats this exactly like `Cold`.
    Snapshotting(String),
}
```

`OffsetStore::resume` returns `Snapshotting(position)` where it currently returns
`Cold`-on-`snapshot_in_progress`. The safety property is *strengthened*: today a
bool discipline stops the position being misread as a stream position; with a
separate variant the type system does. Sources that cannot resume a snapshot
match `Cold | Snapshotting(_) => cold start` in one arm and are unchanged.

This is the single most valuable change on this list. Without it, "backfill a
large Salesforce org" is not reliably achievable.

### G3 — One connector process captures exactly one entity, and that is a config constraint, not a trait constraint. **Confirmed — and the trait is less of an obstacle than it looks.**

Read carefully:

- `CommitSource::entity(&self) -> &str` (`source.rs:115`) **has no callers in the
  crate.** The offset store's entity comes from `ConnectorConfig::entity()`
  (`main.rs:38`, `config.rs:205-211`), not from the source.
- `CommitSource::connector(&self) -> &'static str` (`source.rs:112`) has exactly
  one caller, and it is a test (`sink.rs:685`). The offset store's connector name
  comes from `ConnectorConfig::connector_name()` (`main.rs:37`,
  `config.rs:213-219`).
- The value that actually reaches the wire, `SourceInfo::connector`, is a
  **`String`** (`record.rs:93`), built by each source from a local `const`. A
  parameterised name like `"salesforce:Contact"` is already expressible there.

So the candidate gap "`connector()` returns `&'static str`, does that hold when a
connector is parameterised by object type?" is **mostly refuted**: the method is
vestigial. The `&'static str` that *does* bite is on the error type —
`CdcError::UnusablePosition { connector: &'static str, .. }` and
`NoFeed { connector: &'static str, .. }` (`source.rs:85`, `source.rs:96`) — which
must be supplied at every error site. A per-object-type connector name there
requires `Box::leak` or a static lookup table. **Proposed change: make those two
fields `String` (or `Cow<'static, str>`).** Small, mechanical, and it removes the
only real pressure to leak memory for a log string.

The **real** one-entity-per-process constraint is elsewhere and is much harder:

- `ConnectorConfig` has one `topic`, one `source`, one `state_dir`
  (`config.rs:36-59`).
- `offset_path()` is `{state_dir}/{topic}.offsets.json` (`config.rs:221-223`) —
  keyed by **topic only**, not by entity.
- `OffsetStore::open` refuses a file whose stored connector/entity differ from
  the caller's (`offsets.rs:66-75`).
- `TopicWriter::claim` takes an exclusive `flock` on
  `{state_dir}/{topic}.writer.lock` (`sink.rs:116-132`).

Consequence, which every SaaS builder must be told explicitly: **one process per
object type, one topic per object type, and a distinct `state_dir` or `topic` for
each.** Two connectors for Contacts and Companies pointed at the same topic and
state dir will fail at startup — the `flock` catches it on merkql, and the offset
file's entity fence catches it everywhere. That is the framework working
correctly; it is not a bug to route around.

Whether one topic per object type is *right* is a domain question, and the honest
answer from `lib.rs:80-86` is that it does not matter much: the producer key is
the envelope id, partitioning is per-record, and `num_partitions = 1` gives total
order per topic. Separate topics per object type are cheap and give independent
offsets, independent backfills, and independent blast radius. **Recommend
one connector process per object type.** Do not build a multiplexing source that
interleaves object types behind one `CommitSource` — its cursor would have to be
a composite of N independent cursors, and the offset store holds one string.

### G4 — The single-writer guard applies to merkql only. **Confirmed. Worth knowing before you pick a queue.**

`TopicWriter::claim` (with its `flock` and its single-partition check) is
constructed only in the `QueueConfig::Merkql` arm of `open_sink`
(`main.rs:69-77`). The `Ksql` arm (`main.rs:80-93`) builds a `RepositorySink`
with no lock of any kind. For Kafka that is correct — Kafka tolerates concurrent
producers — but it means the "a second connector fails at startup" guarantee that
`sink.rs:74-95` describes so forcefully is **not in force on a repository sink**.
Two ingress connectors misconfigured onto one ksql topic will both run happily
and interleave. No change proposed; this is a documentation gap, and the skill
should say it.

### G5 — OAuth token refresh: **certain, not hypothetical — but still not a trait change.** The right verdict is that it *raises the rank of G2*.

**Verified:** HubSpot OAuth access tokens expire in **30 minutes**
(`expires_in: 1800`, reduced from 6 hours). Expiry surfaces as a bare HTTP 401,
and the documented error category is `INVALID_AUTHENTICATION` — there is no
confirmed distinct "expired" category, so **matching on the category string to
detect expiry is unsound**. Treat any 401 as "refresh once and retry".

That makes the arithmetic unarguable: any backfill longer than half an hour
crosses at least one expiry, and a 6-hour backfill crosses at least twelve. This
is not an edge case to handle eventually; it is on the main path of the first
run.

`CdcError` has three variants (`source.rs:73-102`). A 401 reaching the stream
becomes `CdcError::Backend(anyhow)`, hits `sink.rs:305`, kills the process, and
via G2 discards the backfill — which then restarts and expires again. Livelock.

**I still say this is not a trait change, and I want to be explicit about why**,
because the framing "the error model has no way to express *re-authenticate and
resume from where I was*" makes it sound like one:

- "Resume from where I was" is **already expressed** — that is `Resume::At`, and
  it is what the offset store exists for. Nothing needs adding.
- Re-authentication is not a stream-level event. It is a property of the
  transport, invisible above it, and the connector loop has strictly *less*
  context to act on a 401 than the client that issued the request does. Adding
  `CdcError::AuthExpired` would move the retry somewhere worse and give every
  future source a variant it must remember to construct.
- The correct layering is: **the client owns the token and the 401 never reaches
  the stream.** That is achievable with no framework change, and it is the
  design every one of these connectors should have.

So the consequence of this fact is not a new gap. It is that **G2 moves from
"probable failure on large orgs" to "certain failure on the first backfill of
any org"**, because the trigger no longer depends on org size, throttling, or
luck — it depends only on the clock. G2's ranking above G1 is confirmed.

**One genuine framework gap does fall out of this, and it is new.** Vendors that
**rotate the refresh token on each use** require the connector to durably persist
a new credential mid-run. There is nowhere sanctioned to put it: the offset file
is a typed, fenced, single-purpose record (`offsets.rs:24-34` — `connector`,
`entity`, `position`, `snapshot_in_progress`), and putting a secret in it would
both break the fence and write a credential into a file whose whole design
assumes it is safe to lose. The answer is that `state_dir` (`config.rs:51`)
already exists and a connector may own its own file beside the offset file — but
that is nowhere stated, and the wrong instinct (widen `Stored`) is the obvious
one. **Proposed: document that `state_dir` is connector-writable for
source-private state, and that the offset file is off-limits.**

**Proposed rule instead** (belongs in the skill, not the trait):

- The HTTP client owns the token. It refreshes **proactively** on a clock (before
  expiry, not after a 401) *and* reactively on a single 401, retrying the request
  once. A `ChangeStream` must never yield an error because an access token
  expired.
- A failure to *refresh* — revoked refresh token, changed client secret, revoked
  connected app — is genuinely unrecoverable without an operator, and
  `CdcError::Backend` with a message naming the credential is exactly right. It
  is fatal, and it should be.
- Refresh must be safe to call from inside the stream. Guard it so N concurrent
  401s cause one refresh, not N.

The gap worth recording in the framework is smaller and different: **nothing in
`CommitSource` gets a chance to run before the stream is polled again**, so a
connector that needs periodic non-record work (token refresh on a timer, a
keepalive) must own its own timer inside the stream, as `postgres.rs:844-851`
does for the heartbeat. That is a workable pattern; the skill should point at it
rather than at a new trait method.

### G6 — `durable_through()` is **not** dead weight for ingress, but its meaning changes. **Candidate gap refuted.**

`durable_through` (`source.rs:145-147`, default no-op; `postgres.rs:628-647` the
real implementation) is called by `sink.rs:320-322` only after an offset commit
has hit the disk. Its contract is "you may now let go of what you were holding".

For a **polling** SaaS source there is nothing held server-side, so the default
no-op is correct and the method is genuinely unused — same as SQLite and Mongo,
which also take the default.

For a **push** SaaS source it is exactly the right hook and should be used:
a connector that receives webhooks must spool them to disk before acknowledging
(otherwise it has accepted a delivery it can lose), and `durable_through` is when
the spool entry may be truncated. Same shape as the Postgres slot, same reason.

So: not dead weight, but you will only use it if you build a push connector. If
you take the default, say in the module docs *why* — "this source holds nothing
back" — the way the crate does for its other two.

### G7 — Rate limiting and backoff appear nowhere in the trait, and mostly should not. **Partly refuted.**

Per-request retry belongs in the client (G1). But two rate-limit-shaped facts are
genuinely the *connector's* business and have nowhere to live:

- **Polling interval.** The crate's design doc opens by arguing against a pull
  shape (`source.rs:3-15`) and `sqlite.rs:151-160` refuses to start rather than
  "silently degrade to a timer". A builder reading that will either think polling
  is forbidden or will poll silently. Both are wrong. The correct position: **an
  honest poller is fine when the API has no change feed; a silent poller is
  not.**

  **Correction to the framing given to the builders** ("a poller is legitimate,
  it just must not pretend to be a feed", implying these APIs mostly lack feeds):
  that is wrong for Salesforce, and probably wrong more often than a first look
  suggests. Salesforce has a genuine server-maintained resumable change feed that
  **includes deletes** — `changeType` covers `DELETE` and `UNDELETE` — reachable
  over CometD as plain HTTP/1.1 long-polling JSON, with no gRPC and no Avro
  required and no deprecation notice. A Salesforce connector that polls SOQL on a
  timer is not an honest poller; it is a connector that **did not look hard
  enough**, and it will silently miss deletes that the real feed would have
  delivered. The decision procedure must press on "is there a real feed?" until
  the answer is evidenced, not assumed.

  Salesforce's feed also emits **`GAP_*` events** when it cannot supply the
  detail of a change. A gap event means "something happened here and you are not
  getting the record". Dropping it is the exact silent gap this crate exists to
  prevent: the connector must either reconcile by fetching the record by id, or
  emit something that marks the hole. Ignoring an event type you do not recognise
  is never safe on a change feed.

  **Proposed change:** a defaulted trait method that makes the honesty
  structural, logged at startup next to the sink backend (`main.rs:44-50`):

  ```rust
  pub enum FeedKind {
      /// The source tells us when there is something to say.
      Push,
      /// We ask on a timer, because the API has no change feed.
      Poll { interval: Duration },
  }

  fn feed_kind(&self) -> FeedKind { FeedKind::Push }
  ```

  Cost: one defaulted method, three unchanged sources. Benefit: `merkql-connect`
  logs "salesforce → merkql topic 'crm_contact' (poll every 30s)" and nobody
  discovers the latency characteristic in production.

- **Quota is shared across connectors.** N connector processes against one
  Salesforce org share one daily API quota; the framework's one-process-per-
  entity shape (G3) actively multiplies the number of quota consumers. Nothing
  in the config expresses this. No framework change proposed — but the skill must
  require the connector's config to carry an explicit request-budget knob, and
  the module docs to state the per-org arithmetic.

### G8 — Schema drift: **I was wrong to call this largely benign. Confirmed as a silent data-loss path.**

`Envelope::payload` is `Stash = serde_json::Map<String, Value>`
(`meshql-core/src/lib.rs:18`). Nothing in `merkql-connect` validates a payload:
`RepositorySink::append` (`sink.rs:236-256`) passes the envelope straight to
`Repository::create`. So a custom field appearing mid-stream costs nothing in the
connector, and the answer to "what breaks?" is **nothing here**.

**But asking "what breaks in the envelope?" was the wrong question.** A
schemaless payload does not save you when **the source truncates the record
before you ever see it**, and that is the normal case, not an exception:

**Verified — HubSpot.** The CRM API does **not** return all properties by
default. You name them in a `properties` query parameter; **unknown names are
silently ignored with no error**; a property you did not request simply does not
appear. So a custom field added by an admin mid-stream is invisible *forever*
until the connector re-discovers the schema
(`GET /crm/v3/properties/{objectType}`) and widens its request. Worse,
`propertyChange` webhook subscriptions are **per-property**, so the new property
also fires no notification until a subscription exists for it — there is no edge
to tell you anything changed. Both halves of the feedback loop are silent.

**Verified — Salesforce, the opposite shape.** CDC event messages always reflect
the latest field definitions, so nothing is truncated; but the Avro **schema id
changes** when the schema changes, and a Pub/Sub consumer must notice the new
`schema_id` and call `GetSchema`. The schema id therefore belongs in the cursor
alongside the replay id — which is fine, because the cursor is opaque (§3 rule 3)
— and it must be a *versioned* encoding, or the connector upgrade that
introduces it invalidates every stored position.

**The generalisation, which belongs in the skill as a gating question:**

> **Does the source return the whole record, or only what you asked for?**
> Answer it before writing a line of mapping code.

Truncating sources need periodic schema re-discovery with the delta logged.
Non-truncating sources need schema-version tracking in the cursor. Assuming
either behaviour without checking is how a connector quietly stops carrying a
field that a projection depends on.

Two further consequences:

1. **The id must not depend on a droppable field** (§4, clause 1). An admin
   deleting a field that participates in your natural key changes every id going
   forward, silently forking every entity.
2. **Downstream projection schemas.** The projection restlette validates against
   a JSON Schema. A new field flows through the connector untouched and is
   rejected there — loudly, which is the right place for it to fail. Not the
   connector's problem; do not add validation to the connector to "protect"
   downstream.

---

### G9 — `cert::certify` cannot certify an ingress connector. **Confirmed by test. New, and it invalidates a claim I made earlier in this document.**

The suite hardcodes the literal envelope ids it expects: `["pre-1", "pre-2"]`
(`cert.rs:123`), `"live-1"` (`cert.rs:135`), `"during-downtime"`
(`cert.rs:215`), `"after-start"` (`cert.rs:243`). That works for the three
database sources because `CertStore::write(envelope)` stores exactly that
envelope and the source reads it back unchanged.

An ingress connector **derives** its id (§4), so `record.key()` returns
`salesforce:Contact:0035…` and **three of the four sub-tests fail on the id
comparison before reaching the property they exist to check.** My §6 claim that
`write()` is "where your id derivation gets tested against real assertions" is
wrong as the code stands.

The two available workarounds are both bad: injecting an identity derivation into
the test certifies nothing and disables the assertion that matters most, and
editing the shared `cert.rs` on a connector branch is exactly what should not
happen to a shared contract.

**Proposed change:** a defaulted method on `CertStore`, applied at each of the
four assertion sites.

```rust
/// The envelope id a source will derive for the logical record named
/// `logical`. Sources that store envelopes verbatim take the default.
fn envelope_id(&self, logical: &str) -> String { logical.to_string() }
```

Three lines, no behaviour change for the existing backends, and it makes the
suite's most valuable property — that the ids you derive are the ids that come
back — actually testable. Until it lands, run
`certify_positions_are_present_and_distinct` (which asserts nothing about ids)
as-is and transliterate the other three, saying so in the test file.

## 6. Certification — the part most likely to be skipped

`merkql-connect/src/cert.rs` is a **certification suite for `CommitSource`
implementations**, not an example. `certify()` (`cert.rs:74-88`) runs four
contract tests, and every existing backend's integration test drives it rather
than writing its own approximation (`cert.rs:1-9`).

**Read G9 first — the suite does not currently pass for a derived-id source.**
The rest of this section describes what certification should look like once the
`CertStore::envelope_id` hook exists, and what to transliterate until then.

An ingress connector **can and should** be certified, against a `wiremock`-backed
fake rather than a real API. `CertStore` needs two methods (`cert.rs:21-31`):

- `write(envelope)` → "make the fake serve a source record that derives to this
  envelope". This is where your id derivation *would* be exercised against real
  assertions — see G9 for why it currently is not.
- `source()` → a fresh source pointed at the fake.

Three adaptations an ingress `CertStore` needs, each of which teaches something:

1. `certify_snapshot_then_stream` asserts `after.auth` is `["cert"]`
   (`cert.rs:148-153`). Ingress tokens come from config, so configure the fake
   source with the mark `["cert"]`.
2. `certify_positions_are_present_and_distinct` (`cert.rs:160-186`) is the test
   that catches a bare-timestamp cursor. If your fake serves three records with
   the same modification timestamp — and it should, because that is the case
   that breaks production — a watermark cursor fails here. **This is the single
   most valuable test in the suite for a SaaS source.**
3. `PATIENCE` is 20 seconds and `certify_never_mode_skips_history` expects quiet
   for 750 ms then a live record (`cert.rs:71`, `cert.rs:226-246`). A poller with
   a 60-second interval cannot pass. The poll interval must be configurable, and
   the test must configure it low. That is a feature, not a workaround: a
   hardcoded interval is a connector nobody can tune.

If some part of `certify()` genuinely cannot apply to a given source, run the
sub-functions individually (they are all `pub`) and document in the test file
which one is excluded and why. Do not silently not-certify.

---

## 7. Shared-file edit points

Every ingress connector touches the same five places. All three in-flight builds
will conflict on all five; whoever merges second resolves.

| File | Edit | Failure if omitted |
|---|---|---|
| `merkql-connect/src/config.rs:112-155` | new `SourceConfig` variant | config does not parse |
| `merkql-connect/src/config.rs:205-219` | arms in `entity()` and `connector_name()` | non-exhaustive match, compile error (this is the good kind) |
| `merkql-connect/src/lib.rs:101-110` | `#[cfg(feature = "…")] pub mod …;` | module not compiled |
| `merkql-connect/src/main.rs:104-150` | arm in `open_source` | falls through to `main.rs:144-148`, "no support for source", at runtime |
| `merkql-connect/Cargo.toml:39-44` and `:46-62` | feature + deps; `reqwest` is currently a **dev**-dependency only (`Cargo.toml:57`) and `wiremock` is absent entirely | will not build |

Note `default = ["sqlite", "mongo", "postgres"]` (`Cargo.toml:40`). Decide
deliberately whether a SaaS connector joins the default feature set; a default-on
feature that pulls a large HTTP/gRPC stack into every build of the binary is a
cost, and the crate's own comment (`Cargo.toml:27-28`) explains that defaults are
on because a connector binary is built once and pointed at whichever store a
deployment runs. That argument is weaker for SaaS sources than for databases.

---

## 8. Summary of proposed framework changes

Ranked by consequence.

1. **`Resume::Snapshotting(String)`** (G2) — makes a resumable backfill
   expressible at all, and strengthens the existing safety property rather than
   weakening it (a type instead of a bool discipline).

   **Ranked first because the trigger is now the clock, not luck.** HubSpot
   access tokens expire in 30 minutes (G5), so any backfill over half an hour
   crosses an expiry and a 6-hour one crosses twelve. Combined with `sink.rs:305`
   being fatal and `offsets.rs:103-108` discarding mid-snapshot positions, a
   large backfill does not *risk* livelock — it livelocks unless the client's
   refresh and retry are perfect on the first try.

   **Scope correction:** this is only needed where the backfill mechanism differs
   from the incremental one. A uniform keyset poller can use
   `snapshot_mode = "never"` with a configured `start_from` and get an exactly
   resumable backfill today, honestly and with no framework change. That covers
   some sources; it does **not** cover Salesforce (Bulk API 2.0 vs CDC replay
   ids) or HubSpot (list endpoint vs search/journal), where the two cursor
   namespaces force a genuine snapshot phase.

2. **`CertStore::envelope_id(&self, logical: &str) -> String`** (G9) — three
   defaulted lines that make the certification suite usable by any source that
   derives its ids, i.e. every ingress connector. Without it the crate's own
   contract tests cannot be run against the connectors being built right now.

3. **A documented transient-error contract for `run_connector`** (G1) — at
   minimum, state that `CdcError::Backend` is fatal by design and that retry is
   the source's responsibility. Today a builder will reasonably assume the loop
   retries; nothing says otherwise.

4. **`CdcError::{UnusablePosition,NoFeed}.connector` as `String`/`Cow`** (G3) —
   removes the only real pressure toward `Box::leak` for a parameterised
   connector name.

5. **`CommitSource::feed_kind()`** (G7) — makes honest polling structural instead
   of a comment.

6. **Document that `state_dir` is connector-writable for source-private state,
   and that the offset file is off-limits** (G5) — the only place a rotated
   refresh token can go, and the wrong instinct (widen `Stored`) is the obvious
   one.

7. **Document that the single-writer guard is merkql-only** (G4).

Explicitly **not** proposed: an `AuthExpired` error variant (G5), rate-limit
policy in the trait (G7), payload validation in the connector (G8), a
multiplexing multi-object-type source (G3). Note the carve-out: one entity with a
*record* feed and a *delete* feed is not multiplexing, and its composite cursor
is forced rather than chosen.

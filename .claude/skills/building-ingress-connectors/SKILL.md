---
name: building-ingress-connectors
description: Use when adding a `merkql-connect` source that reads a foreign system rather than a meshql envelope store — Salesforce, HubSpot, SAP, NetSuite, Workday, Zendesk, Dynamics, Shopify, Stripe, ServiceNow, or any REST/gRPC/webhook API. Also use when reviewing such a connector, when a connector's backfill keeps restarting, when a projection built from an ingested feed is missing records, or when deciding whether an API's change mechanism is usable for change data capture at all.
---

# Building ingress connectors

An **ingress connector** is a `CommitSource` (`merkql-connect/src/source.rs:109-148`)
whose upstream has never heard of meshql. The three sources that exist today —
`sqlite.rs`, `mongo.rs`, `postgres.rs` — read an envelope table and *reconstruct*
an envelope the mesh already wrote. An ingress connector **synthesises** one.

That is the whole difference, and it is enough to change the risk profile
completely: every field of the envelope becomes a decision you make, and almost
every wrong decision fails silently. Nothing in the crate validates a synthesised
envelope — `RepositorySink::append` hands it straight to `Repository::create`
(`sink.rs:236-256`).

**The theme of every rule below is silent gaps.** A connector that crashes is
fine; a supervisor restarts it and the offset store replays. A connector that
delivers 94% of a source system and reports healthy is the failure this crate
exists to prevent, and it is what a naive implementation produces.

Full derivation, with the gaps in the trait and the proposed framework changes:
`docs/ingress-connector-contract.md`. Read it before proposing any change to
`CommitSource`, `Resume`, or `CdcError`.

## Before anything else: read the framework, do not re-implement it

| You do NOT write | It already exists |
|---|---|
| Offset persistence, commit intervals, atomic writes | `offsets.rs` |
| The append-then-commit ordering | `sink.rs:309-323` |
| Snapshot-mode policy, unusable-position handling | `sink.rs:285-304`, `sink.rs:343-362` |
| Retry of the *whole* stream | Nothing — the loop is fatal on error. See "Rate limits" below. |
| Your own contract tests | `src/cert.rs` — a real certification suite for sources |

You write exactly one thing: an `impl CommitSource` that yields `ChangeRecord`s
with correct ids and correct positions, plus a `SourceConfig` variant and a
`main.rs` arm.

## Step 1 — Interrogate the API before you write any code

Answer all seven in the module doc comment, with links to the vendor docs.
**If you cannot answer one, that is the finding**, and it goes in the docs as an
unknown rather than as an assumption.

1. **Is there a real change feed?** Server-maintained, resumable, delivering
   changes you were not connected for. Salesforce has one (Change Data Capture
   over the Pub/Sub API). Most SaaS APIs do not.
2. **What is the cursor and when does it expire?** Salesforce replay ids live in
   an event bus with a **72-hour retention window**
   ([docs](https://developer.salesforce.com/docs/atlas.en-us.change_data_capture.meta/change_data_capture/cdc_subscribe_delivery.htm)) —
   a connector down over a long weekend comes back to an unusable position by
   design. SAP OData delta-token expiry is **implementation-defined**: there is
   no standard, it depends on the backend service
   ([SAP delta token docs](https://help.sap.com/doc/d9c75eebcfa840c8a4aa4b0e6a8136de/3.0.14/en-US/7c043ee0700610149372d5f766be596a.html)),
   so treat it as unknown-and-short until an operator confirms it for their
   system. **Whatever the number is, it must be in the module docs and the
   README, because it is the operational MTTR budget for the connector.**
3. **Do deletes surface?** In the change feed, or only through a separate
   endpoint with its own (usually shorter) retention, or not at all? A connector
   that cannot see deletes produces a projection that only ever grows.

   **Where the retention windows do not overlap, you have a permanent hole, and
   documenting it is mandatory.** Salesforce is the worked example: replay ids
   last 72 hours, and past that you fall back to a Bulk API 2.0 backfill plus
   `getDeleted` — which itself only reaches back **15 days**. Deletes older than
   15 days are recoverable by *no* path. That is not a bug to fix; it is a
   property of the source that every projection built from the feed inherits, and
   the only failure is not writing it down.

### Cursor retention — the numbers, and why they are the MTTR budget

| Source | Cursor | Retention | Expiry detectable as |
|---|---|---|---|
| Salesforce CDC (Pub/Sub or CometD) | replay id | **72 h** (24 h for PushTopic / standard-volume) | CometD 400 naming the invalid replayId; Pub/Sub `…replayid.corrupted` |
| Salesforce deletes (`getDeleted`) | — | **15 days** | — |
| HubSpot Webhooks Journal API v4 (**BETA**) | journal offset | **3 days** | — |
| HubSpot v3 webhooks | none | n/a — no cursor, no replay, **no ordering guarantee, no dedup guarantee**, 10 retries over 24 h then gone | — |
| SAP OData delta token | delta token | **backend-defined; there is no standard** | vendor-specific |

The retention window is how long the connector may be down before a restart
cannot resume — the operational MTTR budget. It belongs in the module docs and
the README. Where it is unknown (SAP), write "unknown, confirm per system"
rather than inventing a number.
4. **What is the result-window cap on a backfill?** HubSpot's CRM Search API is
   capped at **10,000 total results per query**, max 200 per page
   ([docs](https://developers.hubspot.com/docs/api-reference/latest/crm/search-the-crm)).
   That is not a paging limit you page past — it is a hard ceiling, and the only
   way through it is to window the query (by modification date) and walk the
   windows. A backfill written as "page until exhausted" silently stops at 10,000
   records.
5. **What are the rate limits and what does exceeding them return?** HubSpot's
   search endpoints are **5 requests/second per account**; other endpoints have
   separate per-app burst limits. Get the status code and the retry header, not
   just the number.
6. **What is the token lifetime, and can it expire mid-backfill?** For any
   backfill measured in hours, assume yes.
7. **Does the source return the whole record, or only what you asked for?**
   Answer this before writing a line of mapping code — a schemaless `Stash`
   payload does not save you when the *source* truncates the record before you
   see it. HubSpot's CRM v3 API does **not** return all properties by default:
   you name them in a `properties` parameter, **unknown names are silently
   ignored with no error**, and a property you did not request simply is not
   there ([docs](https://developers.hubspot.com/docs/guides/api/crm/properties)).
   A custom field added by an admin is invisible *forever* until the connector
   re-discovers the schema (`GET /crm/v3/properties/{objectType}`) and widens its
   request — and `propertyChange` webhook subscriptions are **per-property**, so
   there is no edge telling you anything changed either. Both halves of the
   feedback loop are silent.

   Salesforce is the opposite shape: CDC events always reflect current field
   definitions, but the Avro **schema id changes** when the schema changes and
   the consumer must notice it and re-fetch. That id belongs in the cursor
   (which is opaque — see Step 4), versioned.

   **Truncating source → periodic schema re-discovery, delta logged.
   Non-truncating source → schema-version tracking in the cursor.** Assuming
   either without checking is how a connector quietly stops carrying a field a
   projection depends on.

8. **Is the endpoint you backfill from the same one you poll incrementally?**
   Usually not, and the cap that bites is on the search/query path. HubSpot's
   search endpoint hard-caps at **10,000 total results** (400 beyond that) at 5
   req/sec, so backfill must use the **list** endpoint (`limit` max 100, `after`
   = record id), which has no documented total cap. Salesforce backfills via Bulk
   API 2.0 with a `Sforce-Locator` header and streams via CDC replay ids. Two
   endpoints means two cursor namespaces — which is what makes Step 5's snapshot
   problem real for you.

*(Vendor specifics above were verified against the linked docs. Anything you need
that is not linked here, verify yourself — do not carry over a number from
another connector's comments.)*

## Step 2 — Choose the ingestion mechanism

```
Does the API have a server-maintained, resumable change feed?
├─ YES → use it. The cursor is theirs; store it verbatim (mongo.rs:77-88 is the model).
│         Is its retention shorter than your worst-case downtime?
│         └─ YES → snapshot_mode = "when_needed" is the only safe default. Say so in the README.
└─ NO  → Are there webhooks?
          ├─ VENDOR-HOSTED webhooks, and you can run an endpoint they can reach
          │   → push connector. You MUST spool to disk before acking the delivery,
          │     and truncate the spool in `durable_through` (source.rs:145-147).
          │     Webhooks alone are NOT sufficient: no cursor, so anything
          │     delivered while you were down is gone. Check explicitly whether
          │     the vendor guarantees ordering and dedup — often neither. Pair
          │     with a reconciliation poll.
          ├─ IN-SYSTEM SCRIPT you author and deploy inside the source system
          │   (SuiteScript, an Apex trigger, an ABAP exit) → this is NOT a
          │     webhook and is weaker than one: typically at-most-once, no
          │     server-side retry, no backlog, no cursor. It silently drops
          │     everything while your endpoint is down. Usable only as a latency
          │     optimisation on top of a poll that is the actual source of truth.
          └─ NO → honest poller. Read "Polling is allowed; silent polling is not."
```

### Polling is allowed; silent polling is not

`source.rs:3-15` argues hard against a pull shape, and `sqlite.rs:151-160`
refuses to start rather than "silently degrade to a timer". Both are about
**degradation**: a source that *could* be notified must not quietly become a
poller, because its latency and load then stop matching what was deployed.

**But press hard on "is there a real feed?" before accepting that you have no
choice, because the answer is yes more often than a first look suggests.**
Salesforce has a genuine server-maintained resumable change feed that **includes
deletes** (`changeType` covers `DELETE` and `UNDELETE`), reachable over CometD as
plain HTTP/1.1 long-polling JSON — **no gRPC, no Avro, no deprecation notice**. A
Salesforce connector that polls SOQL on a timer is not an honest poller; it is a
connector that did not look hard enough, and it will silently miss the deletes
the real feed would have delivered. Evidence the answer; do not assume it.

**Never drop an event type you do not recognise.** Salesforce emits `GAP_*`
events when it cannot supply a change's detail — "something happened here and
you are not getting the record". Ignoring it is precisely the silent gap this
crate exists to prevent. Either reconcile (fetch the record by id) or emit
something that marks the hole.

An API with genuinely no change feed leaves you no choice, and polling it is
correct. The obligations that come with it:

- The poll interval is **configuration**, never a constant. `cert.rs:71` gives a
  20-second patience budget; a hardcoded 60-second interval cannot be certified,
  and an operator cannot trade latency against quota.
- Log the interval at startup, next to the sink backend (`main.rs:44-50`), so
  "why is this 30 seconds behind" is answerable from the logs.
- If the API *does* have a change feed and you could not establish it, that is
  `CdcError::NoFeed` (`source.rs:94-98`) — refuse to start. Do not fall back to
  polling.

## Step 3 — The envelope mapping rules

`Envelope { id, payload, created_at, deleted, auth }`
(`meshql-core/src/lib.rs:20-27`). All five are yours.

### id — the rule that matters most

The envelope id is not a label. meshql treats two envelopes sharing an `id` as
**two versions of one entity** (`meshql-core/src/lib.rs:41-49`), and the merkql
producer key **is** the envelope id, hardcoded (`record.rs:140-154`,
`sink.rs:179`).

> **`id = "{system}:{object_type}:{canonical_natural_key}"`** — where
> `canonical_natural_key` comes from one pure function, documented with the
> reason it is canonical, and unit-tested for the collision it prevents.

Four requirements. Every SaaS system violates at least one if you use its raw id:

1. **Stable** — derived only from fields that cannot change for the record's
   life. An id containing a modification timestamp turns every update into a
   *new entity* rather than a new version. The projection ends up with N copies
   of one record and nothing reports an error.
2. **Globally unique across everything sharing the topic** — two source records
   deriving to one id become versions of each other, and the older one silently
   vanishes from every read.
3. **Canonical** — one source record must produce exactly one id *string*. If the
   system can hand you the same record under two spellings, normalise first.
4. **Derivable from a delete notification** — deletes arrive with less data than
   creates, often only an id and a type. If your id needs a field the delete
   event does not carry, you cannot write the tombstone.
5. **Actually stable *in the source system*** — verify, do not assume. The hard
   case is not "did I pick a mutable field", it is **the vendor renaming
   records**. Since January 2025 a HubSpot merge creates a brand-new record with
   a **new id**; the old ids survive only as aliases. No field you chose was
   mutable and the identity still moved. So the question is not "is my key
   stable?" but:

   > **Is the source's natural key stable over the record's lifetime, and if not,
   > which event renames it?**

   Encoding problems (Salesforce 15-vs-18, SAP composite keys) are the easy case
   — one canonicalisation function solves them forever. Identity *mutation* needs
   a runtime answer, because meshql cannot retroactively rename an id: the id
   *is* the version chain and the partition key. The only thing expressible is to
   **emit the rename as records** — one tombstone (`deleted: true`) per
   merged-away id, plus the survivor under its new id. A connector that ignores
   the rename event accumulates duplicates no replay can repair.

Each of the three hand-built connectors hit a different clause, which is why all
four are load-bearing:

| System | Clause hit | The rule it forces |
|---|---|---|
| Salesforce | 3, canonical | Record ids come in a 15-char case-sensitive and an 18-char case-insensitive form; different APIs return different lengths. Normalise to 18 at the boundary, always. **Test that both forms of one record produce the same envelope id.** |
| HubSpot | 2, globally unique | Object ids are numeric and scoped to the object type — a contact and a company can share `12345`. The object type is *part of the identity*, not metadata. |
| SAP | 2 and 3 | Business objects are keyed by tuples, typically including the client. Joining with a separator is only safe if the separator cannot occur in a component, **or** every component is escaped/fixed-width — otherwise `("A","BC")` and `("AB","C")` collide. Assert that in the test. |

**Never hash the whole record to make an id.** It buys uniqueness and destroys
stability: every edit forks a new entity, and a delete notification cannot
reproduce the hash.

### payload

The record, plus **everything the domain needs that the Debezium block will not
carry**. `Repository::create` takes an `Envelope` (`meshql-core/src/lib.rs:73`),
so on any non-merkql queue `RepositorySink` appends `record.after` and drops
`op`, `ts_ms` and the whole `source` block (`sink.rs:26-39`, `sink.rs:236-256`).

The consequence most often missed: **`created_at` is connector wall-clock.**
`Envelope::new` stamps `Utc::now()` (`meshql-core/src/lib.rs:30-38`). Backfill
five years of history and every envelope claims to have been created today. If
domain time matters — and for events it always does — put the source system's
modification timestamp in the payload as an explicit field. Do not rely on
`created_at`, and do not rely on `source.ts_ms` reaching the consumer.

Same reasoning for backfill-vs-live: `op: r` vs `op: c` does not survive a
repository sink. If a worker must behave differently during a backfill,
materialise that into the payload.

### created_at — **the source system's MODIFICATION timestamp**

Construct `Envelope` literally rather than via `Envelope::new`, exactly as
`sqlite.rs:219-225` does. Which timestamp you pick is a correctness decision, not
a cosmetic one:

`envelope_order` sorts by `created_at` with `id` as tiebreak
(`meshql-core/src/lib.rs:64-69`), and a read resolves each id to **the latest
version at-or-before the cutoff** (`meshql-core/src/lib.rs:41-49`). So
`created_at` decides **which version of a record wins**. Therefore:

- **Never the record's creation date.** Every version of one record would tie on
  the sort key with an identical tiebreaker, and version resolution becomes
  arbitrary. The field is named `created_at` and the source will have a
  `date_created` column sitting right there; this is a trap.
- **Never `Utc::now()` for a source with unordered delivery.** HubSpot v3
  webhooks explicitly guarantee neither ordering nor dedup, so version 2 can
  arrive after version 4. Stamp wall-clock and the *stale* version gets the later
  `created_at` and wins every read, permanently. Replaying the topic does not
  repair it — the wrong timestamps are inside the envelopes.
- If the source gives you no modification timestamp, you cannot correctly ingest
  an unordered feed. Escalate that; do not paper over it.

### deleted

meshql expresses a delete as a **new envelope version with `deleted: true`**
(`record.rs:18-22`). That is where a delete notification goes. Never emit a
`ChangeRecord` with `after: None` — `RepositorySink` errors on it by design
(`sink.rs:236-244`).

**A tombstone's payload is whatever the delete notification actually carried** —
usually an id, a type and a timestamp, and no record body. Say so explicitly in
the payload (a `_deleted_at` and a marker naming the delete source), and state in
the module docs that a fold written against the record shape will see a
near-empty envelope for tombstones. `created_at` on a tombstone is the **deletion**
timestamp: it must sort after the last live version, or the delete loses to it.

### The authorization mark (`auth`)

From config, never from the source record. A connector writes outside any
request, so the sink appends under `meshql_core::SystemSession`, whose `stamp`
leaves the envelope exactly as the source built it — the mark you set is the
mark that is stored. Getting it wrong therefore makes the connector's own
writes invisible to every reader, with nothing to correct it downstream.

## Step 4 — The cursor

`Resume::At(String)` is opaque (`source.rs:29`), stored verbatim
(`offsets.rs:24-34`), and carried on `SourceInfo::position: Option<String>`
(`record.rs:100`). You never write the offset file; the loop stages your position
after appending (`sink.rs:311-323`).

Two constraints "opaque" hides:

**Positions must be distinct per record.** `cert.rs:160-186` fails if any two of
three consecutive records share one. A bare `updated_at` / `SystemModstamp` /
`hs_lastmodifieddate` watermark **does not satisfy this** — bulk edits stamp many
records with one timestamp. This is not cosmetic: `WHERE modified > cursor` skips
the tied records **permanently**, and `>= cursor` re-delivers them forever. Use a
server-issued token, or a composite `(timestamp, id)` keyset with a total order
your query can actually express. If the API cannot express that order, you have
found a real limitation — write it down rather than shipping a watermark.

**A composite keyset is not sufficient on its own. It is only safe over *closed*
time buckets.** With second- or minute-granularity timestamps, a record modified
during bucket `T` but *committed* after you have already read past `(T, id_high)`
is skipped **permanently** by `(ts = T AND id > last_id)` — the same silent loss
a bare watermark causes, reintroduced by the fix for it. Modification touches
arbitrary ids, so within an open bucket the ordering is not append-monotonic and
no tie-break can save you.

So every query also carries a ceiling:

```sql
WHERE (ts > :cursor_ts OR (ts = :cursor_ts AND id > :cursor_id))
  AND ts <= :ceiling        -- ceiling = now - watermark_lag
ORDER BY ts, id
```

`watermark_lag` is **configuration**, it is the connector's latency floor, and it
is logged at startup with the poll interval. Test it directly: a record whose
timestamp falls in an open bucket must not be delivered until the ceiling passes
it, and a low-id record committed late into an already-passed bucket must still
arrive.

**Normalise every timestamp to UTC at the boundary, and assert it.** This is the
same class of bug as id canonicalisation and it is easy to miss: SaaS APIs
routinely return datetimes in the integration user's session timezone or a fixed
vendor timezone rather than UTC, and different endpoints of one API disagree. A
cursor built from unnormalised local times moves *backwards* an hour at a DST
fall-back (replay — survivable) and *forwards* at spring-forward (**permanent
skip**). Check what timezone each endpoint returns; do not assume they match.

**A record may carry `position: None`, and sometimes it must.** `sink.rs:313`
stages nothing when the position is absent. `mongo.rs:224-234` uses this during a
snapshot. The general rule, which is a silent gap if broken:

> **When one source event fans out into several `ChangeRecord`s, only the last
> may carry a position. The earlier ones must carry `None`.**

The loop stages a position after each individual append (`sink.rs:311-323`) and
`Resume::At(P)` means "everything through P is done". Emit A and B both carrying
P, die after A is appended and P committed, and the restart resumes after P —
**B is never delivered.** Not a duplicate; a permanent loss. A merge fanning out
into tombstones plus a survivor has exactly this shape, as does any batched
response where you hand every record the same page cursor.

**A composite cursor is legitimate — encode it, don't concatenate it.** The
string is opaque, so serialise a struct into it when you must carry more than one
value (a schema id alongside a replay id; a window plus an offset within it; a
record cursor plus a delete cursor). `mongo.rs:77-88` is the precedent: JSON,
round-tripped, interpreted by nothing but the source. **Version the encoding** —
a connector upgrade that changes the cursor's shape turns every stored position
into a parse failure, which surfaces as `UnusablePosition`, which means
`when_needed` silently re-snapshots the whole org on deploy and `initial`/`never`
refuse to start. A version tag plus a tolerant reader for the old shape costs
nothing and stops a deploy looking like a data-loss incident.

## Step 5 — Snapshot, and the thing that will bite you

`SnapshotMode` (`source.rs:41-58`) and the resume policy are the framework's, not
yours. Your `changes()` must: capture the streaming position **first**, emit
existing records as `Op::Read` with `Snapshot::True`, tag the final one
`Snapshot::Last`, then stream from the captured position (`source.rs:119-129`,
`sqlite.rs:327-360`).

**Know this before you start:** a snapshot cannot be resumed. `offsets.rs:103-108`
reads back a position staged mid-snapshot and **throws it away**. That is correct
for a table scan measured in milliseconds. For a SaaS backfill measured in hours,
combined with the loop being fatal on error (`sink.rs:305`), it means **every
transient failure during a backfill discards the entire backfill** — and the
restart hits the same rate limit at the same point. That is a livelock, not a
retry loop.

You cannot lie your way out of it. Encoding a snapshot cursor into the opaque
position string and staging it with `snapshot_in_progress == false` reintroduces
the silent gap that flag exists to prevent.

### First ask whether you have a snapshot at all

**If your backfill and your incremental poll are literally the same query with an
advancing cursor, there is no snapshot phase and this problem does not apply to
you.** Deploy `snapshot_mode = "never"` with a configured `start_from` date:
every record is `Snapshot::False` and carries a real position, offsets commit on
the ordinary interval, and a restart resumes at the next record. A multi-hour
backfill becomes exactly resumable with no framework change and nothing
dishonest — a cold start with `start_from` unset still begins at the live tail,
which is `never`'s documented meaning (`source.rs:48-51`), and asking for history
took an explicit line of TOML.

Two conditions, both of which you must check rather than assume:

- **The mechanism must be uniform.** This works only where one query serves both
  history and live changes. It does **not** apply when the backfill endpoint
  differs from the incremental one — and that is the common case, not the rare
  one (Salesforce backfills via Bulk API 2.0 and streams via CDC replay ids;
  HubSpot backfills via the list endpoint and polls via search or the journal).
  Two mechanisms means two cursor namespaces, which means a real snapshot phase,
  which means this section applies in full.
- **Say it in the config docs.** `start_from` under `never` stretches "history
  that predates the connector is not captured". It is the right call, but a
  reader of `source.rs:48-51` will not expect it, so the variant's doc comment
  must state it.

If you genuinely have two mechanisms, then:

1. **Make the backfill survivable.** Retry inside the HTTP client (below) so
   transient failures never reach the loop. This is the whole mitigation
   available today.
2. **Make the backfill smaller.** Windowed queries with a bounded page size, and
   a configurable start date so an operator can backfill "since 2024" rather than
   "since the beginning".
3. **Say it in the README.** "A backfill of N records takes ~T and cannot be
   resumed; a failure restarts it." An operator who knows that schedules it
   differently.
4. **Propose `Resume::Snapshotting(String)`** — see
   `docs/ingress-connector-contract.md` §5 G2. This is the framework change worth
   making. Do not implement it unilaterally in a connector branch.

## Step 6 — Rate limits, retries and tokens

**The connector loop does not retry.** `sink.rs:305` returns the error and
`main.rs:52-58` propagates it out of `main`. The only `sleep` in the crate is the
Postgres WAL heartbeat. So:

- **Retry belongs in your HTTP client**, not in the stream and not in the trait.
  Respect `Retry-After`, exponential backoff with jitter, a bounded attempt
  count. Only the exhausted case becomes `CdcError::Backend`.
- **A 429 is not `UnusablePosition`.** Under `snapshot_mode = "when_needed"` that
  misclassification triggers a full re-snapshot of the org on a transient
  throttle — a rate limit converted into an outage.
- **A 401 is not `UnusablePosition` either, and must never reach the stream.**
  The client owns the token: refresh proactively on a clock *before* expiry, and
  reactively on a single 401 with one retry. Guard the refresh so N concurrent
  401s cause one refresh. A `ChangeStream` must never yield an error because an
  access token expired.
- **A failed *refresh* is fatal and should be.** Revoked refresh token, rotated
  client secret, disabled connected app — `CdcError::Backend` naming the
  credential is exactly right. Do not add a `CdcError::AuthExpired` variant.
- **Periodic non-record work lives inside the stream.** Nothing in `CommitSource`
  runs between polls. If you need a refresh timer, own it in the stream body the
  way `postgres.rs:844-851` owns its heartbeat tick.
- **Quota is shared.** One process per object type (below) multiplies the number
  of quota consumers against one org. Put an explicit request-budget knob in the
  config and do the arithmetic in the module docs.

## Step 7 — One process per object type

This is not negotiable and it is not a trait limitation — it is the config and
the offset store:

- `ConnectorConfig` holds one `topic`, one `source`, one `state_dir`
  (`config.rs:36-59`).
- `offset_path()` is `{state_dir}/{topic}.offsets.json` — keyed by **topic only**
  (`config.rs:221-223`).
- `OffsetStore::open` refuses a file whose connector/entity differ
  (`offsets.rs:66-75`).
- `TopicWriter::claim` takes an exclusive `flock` per (state_dir, topic)
  (`sink.rs:116-132`).

So: **one connector process, one object type, one topic, and a distinct topic or
state_dir per object type.** Contacts and Companies pointed at the same topic and
state dir fail at startup. That is the framework working.

**Do not build a multiplexing source** that interleaves *object types* behind one
`CommitSource`. Its cursor would be a composite of N independent cursors in one
string, and the first object type to fail takes the others' cursors with it.

**Carve-out: one entity with two feeds is fine, and is often forced.** Where
deletes come from a different endpoint than records — a separate deleted-records
query, with its own date column, its own clock and its own retention — the
composite cursor is unavoidable, not a shortcut. That is one entity with a record
feed and a tombstone feed, not multiplexing. The distinction that matters is
blast radius: coupling a record feed to *its own* delete feed is acceptable
because neither is useful without the other, whereas coupling two object types
means an outage in one silently stalls the other. Encode both cursors (per the
rule above), and test that positions from the two feeds interleave and stay
distinct.

Note for the queue choice: the single-writer `flock` guard exists **only on the
merkql sink** (`main.rs:69-77`). The `ksql`/repository path (`main.rs:80-93`) has
no lock. Two misconfigured connectors on one ksql topic will both run.

## Step 8 — Certify, don't improvise

`src/cert.rs` is a **certification suite for `CommitSource` implementations**,
and every existing backend's integration test drives it rather than writing its
own approximation (`cert.rs:1-9`). Yours does too, against a `wiremock` fake.

`CertStore` needs two methods (`cert.rs:21-31`):

- `write(envelope)` → make the fake serve a source record that derives to this
  envelope.
- `source()` → a fresh source pointed at the fake.

### Read this before you plan on `certify()` passing

**`certify()` as written cannot pass for a connector that follows the id rule in
Step 3, and that is a defect in `cert.rs`, not in your connector.** The suite
hardcodes the literal envelope ids it expects: `["pre-1", "pre-2"]`
(`cert.rs:123`), `"live-1"` (`cert.rs:135`), `"during-downtime"`
(`cert.rs:215`), `"after-start"` (`cert.rs:243`). Those hold for the three
database sources, where `write(envelope)` stores that exact envelope and the
source reads it back unchanged. An ingress connector *derives*
`{system}:{type}:{key}`, so `record.key()` returns
`salesforce:Contact:0035…` and three of the four sub-tests fail on the id
comparison before reaching the property they exist to check.

Do **not** resolve this by injecting an identity derivation into the test — that
certifies nothing and disables the assertion you most need. Do **not** edit the
shared `cert.rs` on a connector branch.

Until the framework is fixed (proposed: a defaulted
`fn envelope_id(&self, logical: &str) -> String` on `CertStore`, applied at each
assertion site — see `docs/ingress-connector-contract.md` §5 G9):

- run `certify_positions_are_present_and_distinct` (`cert.rs:160-186`) **as-is**
  — it asserts nothing about ids and is the most valuable of the four for a SaaS
  source;
- reimplement the other three in your own test file, asserting the *same
  properties* against your derived ids, and say in a comment that they are
  transliterations of `cert::certify_*` pending the `CertStore` hook.

Recording that as a known gap is the point. Silently not certifying is the
anti-pattern.

Two further adaptations:

1. `certify_snapshot_then_stream` asserts the mark is `["cert"]`
   (`cert.rs:148-153`) — configure the fake source with those tokens.
2. `certify_positions_are_present_and_distinct` (`cert.rs:160-186`) is the test
   that catches a watermark cursor. **Make your fake serve three records sharing
   one modification timestamp**, because that is the production case. This is the
   single most valuable test in the suite for a SaaS source.
3. `PATIENCE` is 20s and `certify_never_mode_skips_history` wants quiet for 750ms
   then a live record (`cert.rs:71`, `cert.rs:226-246`). Configure a short poll
   interval in the test. If you cannot, your interval is hardcoded — fix that.

If a sub-test genuinely cannot apply, run the others individually (they are all
`pub`) and document in the test file which one is excluded and why. Never
silently skip certification.

## Failure modes that must be tested — checklist

Every one of these fails silently in a naive implementation. A connector without
all of these is not reviewable.

- [ ] **Id canonicalisation.** Two spellings of one source record → one envelope id.
- [ ] **Id collision across object types.** Two records with the same numeric id but different types → two distinct envelope ids.
- [ ] **Id collision across composite-key components.** `("A","BC")` and `("AB","C")` → distinct ids.
- [ ] **Id stability across an update.** Same record, modified → same envelope id, second version.
- [ ] **Identity mutation.** If the source renames records (merge, convert, promote): the rename event produces a tombstone per old id **and** the survivor under the new id, and only the last of those records carries a position.
- [ ] **Fan-out positions.** One source event yielding N records → the first N-1 carry `position: None`. Assert it directly; a crash between them is otherwise a permanent loss.
- [ ] **Version ordering.** Two versions of one record delivered **out of order** → the newer one still wins a read. This is the `created_at` test, and it fails for any connector stamping `Utc::now()`.
- [ ] **Open-bucket watermark.** A record timestamped inside the open time bucket is not delivered until the ceiling passes it; a low-id record committed late into an already-passed bucket still arrives. Without both, the composite keyset silently skips.
- [ ] **Timezone/DST.** Timestamps parsed from the source's timezone, not assumed UTC; a cursor spanning a DST transition neither skips nor stalls.
- [ ] **Cursor encoding version.** Round-trip is the identity; a cursor of the wrong shape or version → `UnusablePosition`, never `Backend`, never a silent reinterpretation.
- [ ] **Cursor ties.** Three records sharing one modification timestamp → three distinct positions, all three delivered, none re-delivered forever. (`cert::certify_positions_are_present_and_distinct` covers the distinctness; add the "all three delivered" case explicitly.)
- [ ] **Snapshot→stream handover.** A record written *during* the backfill is delivered (duplicate is fine, absence is not). `cert::certify_snapshot_then_stream`.
- [ ] **Resume across downtime.** Write while nothing is watching → delivered on restart. `cert::certify_resume_delivers_only_what_follows`.
- [ ] **Expired cursor → `UnusablePosition`.** Fake returns the vendor's expired-cursor error → the source returns `CdcError::UnusablePosition`, not `Backend`. Then: `initial`/`never` refuse to start; `when_needed` re-snapshots. (`sink.rs:586-635` is the shape.)
- [ ] **429 → retry, not `UnusablePosition`, not death.** Fake returns 429 with `Retry-After`, then 200 → the stream delivers the record.
- [ ] **401 mid-stream → refresh and continue.** Fake returns 401 once, then 200 after a token refresh → the stream never yields an error.
- [ ] **Result-window cap.** Fake serves more records than the API's per-query ceiling → all of them arrive. This is the test that catches a backfill that silently stops at 10,000.
- [ ] **Pagination boundary.** Exactly one full page, and one page plus one record.
- [ ] **Deletes.** If the source surfaces them: a delete → an envelope with `deleted: true` and the *same* id as the create. If it does not: an explicit test asserting the documented behaviour, so the limitation is visible in the test names.
- [ ] **New custom field mid-stream.** Fake starts returning an extra field → it appears in the payload. If the API requires naming fields, this test must fail before the property-list refresh is implemented.
- [ ] **No network.** `wiremock` only. Every test in the file.

**The one legitimate live test.** A fake returns canned JSON for any request, so
it cannot validate the things most likely to be wrong about a *query*: that the
dialect accepts your predicate, that `OFFSET` is honoured rather than silently
ignored, that a column selected plainly does not lose its time-of-day, that the
ordering uses an index. Those are silent failures unreachable from wiremock. A
separate, explicitly `#[ignore]`d tenant test whose only job is to assert the
query parses and orders as expected is **correct and encouraged** — label it in
the test name and a comment as documentation of the dialect, not certification.
That is different from the anti-pattern below, which is an `#[ignore]`d test
standing in for the contract tests. One documents a dialect; the other means the
contract is untested in CI.

## Shared-file edit points

All in-flight connectors touch the same five places and will conflict there.
Whoever merges second resolves.

| File | Edit |
|---|---|
| `merkql-connect/src/config.rs:112-155` | new `SourceConfig` variant — non-secret coordinates only |
| `merkql-connect/src/config.rs:205-219` | arms in `entity()` and `connector_name()` |
| `merkql-connect/src/lib.rs:101-110` | `#[cfg(feature = "…")] pub mod …;` |
| `merkql-connect/src/main.rs:104-150` | arm in `open_source` |
| `merkql-connect/Cargo.toml:39-44`, `:46-62` | feature + deps. `reqwest` is currently a **dev**-dependency only (`:57`) and `wiremock` is **absent entirely** — both need adding. |

Decide deliberately whether the feature joins `default` (`Cargo.toml:40`). The
crate's argument for defaults-on (`Cargo.toml:27-28`) is that a connector binary
is built once and pointed at whichever *store* a deployment runs; that argument
is much weaker for a SaaS source that drags an HTTP or gRPC stack into every
build.

**Secrets:** the config variant holds instance URL, object type, portal/client
id. Every credential comes from the environment, as `main.rs:83-88` does for
Confluent. Note that `SourceConfig::Postgres { conn }` (`config.rs:141-154`) puts
a password-bearing string in the TOML — the repo is not yet consistent with this
rule, so do not cite it as precedent.

**Distinguish the secret from where the secret lives.** The rule is "no secret
material in the TOML", not "every credential is literally an env var". A PEM
private key (JWT-bearer / certificate auth) is legitimately a *file*: put the
**path** in the environment, the key on disk with restrictive permissions, and
never the key body in the config. A path in the TOML is a grey area — prefer the
environment so that the config file stays deployable as ordinary configuration.

**If the vendor rotates the refresh token on each use**, the connector must
persist the new one. `state_dir` (`config.rs:51`) is connector-writable and a
source may own its own file beside the offset file. **Do not widen the offset
file** (`offsets.rs:24-34`) — it is typed, fenced by connector+entity, and
designed on the assumption that losing it is safe. A credential is not.

**One canonical home for the operator facts.** Step 1's seven answers, the
retention window, the delete behaviour and the backfill cost are the same facts.
Put them in the **module doc comment** — that is the house style (`sqlite.rs:1-53`,
`postgres.rs:1-78`) and it is next to the code that must stay true to them — and
have the README link to it rather than restate it. Two prose copies drift, and
the one an operator reads will be the stale one.

## Anti-patterns

- **A bare timestamp watermark as the cursor.** Ties skip records permanently
  (`>`) or replay forever (`>=`). Fails `cert.rs:160-186`, and in production it
  loses exactly the bulk edits an operator most wants to see.
- **A composite keyset with no ceiling.** Reads the open time bucket, and skips
  permanently any record committed into it out of id order. The fix for the
  watermark bug, reintroducing the watermark bug.
- **`Utc::now()` as `created_at` on an unordered feed**, or the record's
  *creation* date instead of its *modification* date. Both make version
  resolution wrong, silently and unrepairably by replay.
- **Assuming the API returns the whole record.** Where fields are opt-in, a
  connector that never re-reads the schema stops carrying new fields forever,
  with no error and no notification.
- **Ignoring an event type you do not recognise** — a gap event, a merge event, a
  type you have not mapped. On a change feed, an unhandled event is a silent hole.
- **Giving every record in a batched response the same page cursor.** A crash
  mid-batch loses the rest of the batch permanently.
- **Widening the offset file to hold connector state or a rotated credential.**
  It is fenced and it is designed to be safe to lose. Own a file in `state_dir`.
- **The raw source-system id as the envelope id.** Collides across object types,
  or across id encodings, or both. Silently merges unrelated entities into
  version chains.
- **An id containing a mutable field.** Every edit forks a new entity. Looks like
  it works until someone counts rows.
- **Hashing the record to make an id.** Same failure, plus the delete tombstone
  becomes unwriteable.
- **Classifying a 429 or a 401 as `UnusablePosition`.** Under `when_needed` this
  converts a throttle into a full org re-snapshot, which produces more throttling.
- **Retrying inside `changes()` by swallowing errors and continuing.** A retry
  that loses its place is a gap. Retry the *request*, never the *stream*.
- **A hardcoded poll interval.** Uncertifiable (`cert.rs:71`), untunable, and it
  hides the connector's real latency and quota cost.
- **Silently polling an API that has a change feed you failed to open.** That is
  `CdcError::NoFeed` (`source.rs:94-98`) — refuse to start.
- **A backfill that pages until the API stops returning results**, against an API
  with a total-results ceiling. Stops at the ceiling and reports success.
- **Requesting a fixed list of fields and never refreshing it**, against an API
  where fields are opt-in. New custom fields silently never arrive.
- **Relying on `Envelope::created_at` or `source.ts_ms` for domain time.** The
  first is connector wall-clock; the second does not survive a repository sink
  (`sink.rs:26-39`).
- **A multiplexing source covering several object types.** One offset string
  cannot hold N cursors.
- **Tests against a real tenant, or a `#[ignore]`d integration test standing in
  for certification.** Neither runs in CI, so neither exists.
- **Extending `CommitSource`, `Resume` or `CdcError` in a connector branch.**
  Those are shared; propose the change (`docs/ingress-connector-contract.md` §8)
  rather than landing it beside a connector.
- **A connector whose docs do not state its cursor retention window, its delete
  behaviour, and its backfill cost.** Those three are the operator's entire
  runbook, and only the author knows them.

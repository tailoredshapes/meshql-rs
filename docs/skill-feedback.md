# Skill feedback — what the meshql skills got wrong, or never said

Raised while building `docs/cost-model-dynamodb.md`: a client-facing cost and
performance model for `meshql-dynamo`, validated against real AWS DynamoDB in
`us-east-1` on 2026-07-30. Everything below is something the skills either state
incorrectly or are silent on where silence misleads.

Skills reviewed: `meshql-patterns`, `merkql-architecture`, `meshql-iron`
(`.claude/skills/`).

---

## 1. `meshql-patterns` invariant 5 is true about behaviour and badly wrong about cost

> "Choosing a backend is a question of what you already run, what you know, and
> what you like — **not** of capability: there is no practical difference in
> behaviour between the implementations."

The behavioural claim is correct and the certification suite earns it. But a
reader takes "not a question of capability" as "the choice is a matter of taste",
and on cost it is not remotely a matter of taste. Measured, not estimated:

- One `find_all` whose template has no `"id"` key is a **full table Scan** on
  DynamoDB. At 1 million versions of 1 KiB that is 125,000 RRU — **$0.0156 per
  call**. At one call per second it is **$40,500/month**.
- The identical template on SQLite is one indexed statement against a file, at a
  marginal cost indistinguishable from zero.

That is a factor of roughly 10⁶ in operating cost between two backends the skill
describes as interchangeable, for a query the skill's own examples use
(`getFarms`, `getCoopsByFarm`). It is not a corner case: **74 of the 156
configured queries across `examples/*` are exactly this shape.**

**Suggested fix.** Keep invariant 5's behavioural claim, and add a sentence that
draws the boundary honestly: *behaviour is certified identical; cost and latency
are not, and differ by orders of magnitude for the same query. Pick the backend
on behaviour and taste; then check the cost model for the query shapes you
actually configured.* A pointer to `docs/cost-model-dynamodb.md` would do it.

## 2. The skills never say that `limit` does not bound cost

`meshql-patterns` documents `limit` as a query argument, and `searcher.rs`
correctly documents that the limit is applied **last**, after version resolution
and after visibility filtering, so that an invisible envelope cannot consume a
limit slot. That ordering is right and is a certified invariant.

Nothing anywhere says what it costs. Because the limit is applied last, it
**cannot be pushed into the store**, so `find_all` with `limit: 1` reads and pays
for the whole table. Measured: a `limit: 1` search over 400 versions metered
**51.5 RRU — byte for byte identical to the unlimited search.**

Every developer's intuition is that adding a limit makes a query cheaper. Here it
makes it cheaper to *transmit* and not one unit cheaper to *run*. This belongs in
`meshql-patterns` next to the limit's description, because it silently converts
"paginate the list view" from an optimisation into a no-op.

## 3. Nothing says reads are eventually consistent, and it is observable

No adapter documentation, and neither skill, mentions consistency. `meshql-dynamo`
never sets `ConsistentRead`, so every `Query` and `Scan` is eventually
consistent. This is a deliberate and defensible choice — it halves the read
bill — but it is observable behaviour, not an implementation detail:

> two successive `Repository::list` calls, issued moments after a write burst,
> returned different record sets.

That cost real time to diagnose in `tests/capacity_cost.rs`, where it presented
as a metering bug. `meshql-patterns` invariant 2 says "reads return the latest
non-deleted version at-or-before the requested time" with no freshness caveat,
which reads as a strict guarantee. The `meshql-iron` skill's "honesty: as-of
freshness" section is the natural home for a note that read-your-writes is not
guaranteed on every backend.

## 4. `Envelope` payload equality is not what it looks like

Not a skill claim, but a trap the skills would have saved: `Stash` is
`serde_json::Map`, and `meshql-dynamo`'s `convert::map_to_object` builds it by
walking the AWS SDK's `HashMap<String, AttributeValue>`, whose iteration order is
randomised per process. So two reads of byte-identical data are **equal as
values** and **different as serialised strings**. Comparing envelopes by
`serde_json::to_string` produces a test that fails intermittently for reasons
unrelated to what it is testing. It did here.

Worth a line in `references/storage-adapters.md`: compare payloads by value,
never by rendered JSON.

## 5. `stage-0.md`'s reason for not building the GSI path is wrong

Not a skill, but it is the document the skills' readers are pointed at, and the
error changes a build estimate. `sociallymeshy/docs/refresh/stage-0.md` says
arbitrary-attribute search has no cheap expression

> "without per-projection indexes **the cert template language has no way to
> declare**".

The template language does not need a way to declare them, because **the
templates are the declaration.** Every query template is a fixed string handed to
`RootConfig::builder().singleton(..)` / `.vector(..)` at build time in
`main.rs` — source-controlled, deployed configuration, fully known at startup.
Walking the configured `RootConfig`s yields the complete, closed set of fields
any deployment will ever filter on. Across all of `examples/*` that set has
**ten members**.

This matters beyond pedantry. A *derived* index set cannot drift out of sync with
the queries, because both are generated from the same configuration — a strictly
stronger property than the hand-maintained list `stage-0.md` imagines, and a
cheaper thing to build. It also means the adapter can **fail at startup** when a
configured template names an unindexed field, instead of silently degrading to an
O(V) Scan, which is how a deployment comes to believe it is indexed when it is
not.

## 6. `merkql-architecture` is right that merkql is embedded, and silent on the consequence for retention

The skill's central correction — merkql is an embedded Rust library, not a
deployed service — is correct and worth its prominence. What it does not say is
the thing that decides an architecture: merkql's log is retained **indefinitely**,
and that is its single durable advantage over DynamoDB Streams, whose retention
is **24 hours**.

But the comparison is more favourable to DynamoDB than the 24-hour figure
suggests, and the skill gives a reader no way to see it: on DynamoDB the *table*
is the log and retains every version forever; Streams' 24 hours bound only the
**incremental tail**. A worker down longer than a day does not lose history, it
loses the ability to resume incrementally and must full-replay — and a full
replay is a Scan, which at 1M versions costs **1.6 cents**. The skill should say
that a consumer's retention requirement is about *resumability*, not about
durability, because on a table-backed log those are different questions.

---

## Smaller notes

- **`meshql-patterns` "Pick your scale"** (invariant 6) suggests SQLite for dev
  and "Postgres, Mongo, or Kafka for prod". DynamoDB is absent from the list
  despite `meshql-dynamo` existing and passing every cert. Worth adding, with the
  cost caveat attached.
- **No skill mentions write amplification from secondary indexes.** Measured: at
  1 KiB envelopes each GSI adds exactly 1 WRU per write, so one index doubles the
  write bill and two treble it. Any future indexed-field feature needs this
  stated where the feature is configured.
- **`envelope_order` is ascending on `(created_at, id)` of the *resolved*
  version**, and the skills present that as a neutral ordering choice. It is not
  neutral: because an id's resolved version can sit anywhere in the order, no
  backend can stop reading early, even with a limit. A newest-first contract
  would make `getAll(limit: n)` an `O(n)` backwards walk on DynamoDB instead of
  an `O(V)` Scan. That is a large cost consequence of an ordering decision, and
  it is not written down anywhere.

# What `meshql-dynamo` costs

A cost and performance model for running meshql on Amazon DynamoDB, so you can
compute your own monthly bill from your own workload before you commit to a
backend.

Every capacity figure below was **measured against real AWS DynamoDB in
`us-east-1`**, not estimated. Measurements span **V = 1,000 to V = 1,000,000
versions** — three decades. Figures quoted at larger `V` (the 233-million and
933-million cases in §9 and §12) are **extrapolated** from that range. The
extrapolation is safe: capacity is exactly linear in bytes examined, page count
matched `ceil(V·S / 1 MiB)` precisely at all four measured sizes, and by-id read
cost was constant throughout. V = 10,000,000 was deliberately **not** populated,
because it would have cost real money to prove a line that is already straight. DynamoDB reports the capacity each request consumed
if the request asks for it; `meshql-dynamo` can now ask
(`meshql_dynamo::metering`), and `meshql-dynamo/tests/capacity_cost.rs` asserts
that the model in this document equals what DynamoDB actually billed. Where a
number is extrapolated or taken from a published rate card rather than measured,
it says so.

**The one-paragraph version.** Reads by id are excellent and essentially free:
one round trip and half a read unit, no matter how many versions the record has.
Writes are one unit per kilobyte. The cost risk is entirely in *search*: a query
template with no `"id"` key would be a full table scan whose price is proportional
to the total number of versions in the table — and because meshql is append-only,
that number only ever grows. Left alone, that is a cliff.

**It is no longer left alone.** meshql query templates are static configuration,
so the adapter derives the index set from the same `RootConfig` that generates
the queries, provisions it, and refuses to start if a configured template would
need a scan. A foreign-key search measured at **6.0 read units** against the
**122,254** its scan cost at a million versions. What still cannot be indexed is
`getAll` / `list`, and that is the one operation left to design around.

---

## 1. The rates this model uses

Fetched from the **AWS Price List Query API** on **2026-07-30** for **`us-east-1`**,
on-demand billing. Re-check them before quoting a client; AWS moves them.

| Rate | Usage type | Price |
|---|---|---|
| Read request unit (RRU) | `ReadRequestUnits` | **$0.125 per million** |
| Write request unit (WRU) | `WriteRequestUnits` | **$0.625 per million** |
| Table storage | `TimedStorage-ByteHrs` | **$0.25 per GB-month**, first **25 GB free** |
| Provisioned write capacity | `WriteCapacityUnit-Hrs` | $0.00065 per unit-hour |
| Provisioned read capacity | `ReadCapacityUnit-Hrs` | $0.00013 per unit-hour |
| DynamoDB Streams reads | `USE1-Streams-Requests` | $0.20 per million, first 2.5M free |
| Time to Live (TTL) deletes | *(no SKU exists)* | **free — see §9** |

The free tiers are in the API data as separate price dimensions, not just in
marketing copy: storage is `0–25 GB-Mo @ $0.00`, Streams is
`0–2,500,000 requests @ $0.00`. Note that AWS has restructured its free tier for
newly-opened accounts; if yours does not have the always-free allowance, set it
to zero in the model. `Rates::ON_DEMAND_US_EAST_1` in the crate exposes
`free_storage_gib` for exactly that reason.

Two units you must not confuse:

- one **WRU** covers a write of an item up to **1 KB** (1024 bytes);
- one **RRU** covers a **strongly** consistent read of up to **4 KB**, or **two**
  eventually consistent reads of up to 4 KB.

`meshql-dynamo` never sets `ConsistentRead`, so every read is eventually
consistent and costs **half** a unit per 4 KB. That halves your read bill. It
also means read-your-writes is not guaranteed — two successive `list()` calls
moments after a write burst can return different sets. That is observable, and it
is the correct trade for this contract, but you should know it is being made.

---

## 2. Per-operation capacity — measured

`S` is the average item size in bytes, `V` the total number of versions in the
table (**every** version, including superseded ones and tombstones), `M` the
number of distinct ids, `k` the number of ids in a batch.

| meshql operation | DynamoDB API | Round trips | Capacity | Verified |
|---|---|---|---|---|
| `create` / `update` | 1 × `PutItem` | 1 | `ceil(S/1024)` WRU | exact |
| `remove` (tombstone) | 1 read + 1 `PutItem` | 2 | 0.5 RRU + `ceil(S/1024)` WRU | exact |
| `read(id, tokens, at)` | 1 × `Query` | 1 | **0.5 RRU** | exact |
| `read_many(k ids)` | k × `Query`, concurrent | k (parallel) | **0.5k RRU** | exact |
| `find` / `find_all`, template **has** `"id"` | 1 × `Query` | 1 | **0.5 RRU** | exact |
| `find` / `find_all`, **indexed** payload field | 1 × index `Query` + `Query` per candidate | 1 + candidates (bounded concurrency) | `f·V·G/8192 + f·M/2` RRU | exact |
| `find` / `find_all`, unindexed and no `"id"` | `Scan`, paginated | `ceil(V·S / 1 MiB)`, **serial** | `V·S / 8192` RRU | ±3% |
| `list(tokens)` / `{}` | `Scan`, paginated | `ceil(V·S / 1 MiB)`, **serial** | `V·S / 8192` RRU | ±3% |

The indexed row is the one that changed: with a plan attached there is no
unindexed non-`id` search, because a template naming a field with no index is
refused at startup rather than served by a scan.

Things worth reading twice, all of them measured rather than reasoned:

**A by-id read costs 0.5 RRU no matter how deep the version history is.** Tested
against an id with 50 versions, at four different `at:` cutoffs including the
first and last version: 0.5 RRU and one `Query` every time. The zero-padded sort
key means the first item walked backwards from the cutoff already *is* the
answer. This is better than every SQL adapter, which need a window function.

**The scan is charged on the aggregate, not per item.** DynamoDB sums the items a
request processed and rounds *that total* to 4 KB. So 1 KiB items cost an
**eighth** of an RRU each inside a scan, not a half. Getting this wrong overstates
scan cost eightfold. Measured: 400 versions totalling 411,670 bytes metered 51.5
RRU, against a model of 50.5.

**A `FilterExpression` does not reduce what you pay.** The temporal cutoff
`sk < :hi` is pushed into the scan, but DynamoDB charges for bytes *examined*, not
bytes returned. Measured: a search with `at: 2000-01-01` that resolved **zero**
records metered **51.5 RRU — identical, to the unit,** to the search that
returned everything.

**A `limit` does not reduce what you pay either.** meshql applies the limit last,
after version resolution and after token filtering, so that an invisible envelope
cannot consume a limit slot. That ordering is a certified invariant and it is
right. The consequence is that it cannot be pushed into the store. Measured: a
`limit: 1` search metered **51.5 RRU — identical** to the unlimited one.
Pagination is not a cost control here.

**The write side is exact; the read side runs slightly under.** An item the model
calls 1024 bytes bills exactly 1 WRU and one it calls 1025 bills 2, so
`item_size_bytes` is right to the byte for writes. Scans meter a little above it,
and the two measurements of *how much* do not fully agree:

| Measurement | Items | Granularity | Implied read-side overhead |
|---|---|---|---|
| Calibration tables, N = 200 and 400 | 1,033 B each | ±10 B/item | 11.5–21.7 B/item (1.1–2.1%) |
| Benchmark table, N = 999,993 | ~1,000 B each | ±0.004 B/item | ~1.3 B/item (0.13%) |

The million-item measurement is three orders of magnitude more precise and should
be believed; the residual disagreement with the small-`N` figure is unexplained
and is most likely a few bytes of item-shape difference the size model does not
capture. **Engineering guidance: treat the scan formula as a floor and allow 3%.**
Every scan assertion in the test suite is written as "at least the model, and no
more than 3% above it", which still catches every error that matters — getting
the rounding per-item instead of aggregate is 8×, costing `V` instead of `M` is
10×, and charging strongly-consistent rates is 2×.

---

## 3. Your monthly bill, closed form

Workload variables, all per your deployment:

| Symbol | Meaning |
|---|---|
| `w` | sustained writes per second (including tombstones) |
| `q_id` | by-id reads per second (each id in a `read_many` counts once) |
| `q_s` | searches per second that take the **scan** path |
| `V` | total versions in the table |
| `M` | distinct ids |
| `r = V/M` | versions per id |
| `S` | average item size, bytes |
| `n` | number of secondary indexes (§7) |
| `T` | 2,592,000 — seconds in a 30-day month |

```
monthly_usd =
    w · T · ceil(S/1024) · (1+n) · 0.625e-6      # writes, incl. index amplification
  + q_id · T · 0.5 · 0.125e-6                    # by-id reads
  + q_s · T · (V·S/8192) · 0.125e-6              # scan-path searches
  + max(0, V·(S+100)/2^30 − 25) · 0.25           # storage, after the free 25 GB
```

The `+100` is DynamoDB's documented per-item storage overhead; it does not affect
capacity, only storage.

**At S = 1 KiB the whole thing collapses to four constants you can do in your
head:**

| Term | Monthly cost |
|---|---|
| Writes | **$1.62 per sustained write/sec** (per index, again) |
| By-id reads | **$0.162 per sustained read/sec** |
| Scan searches | **$0.0405 × q_s × V** |
| Storage | **$0.25 per GiB beyond 25**, growing (§8) |

---

## 4. The decision rule: `q·V`

Search is the only term with a product in it, and it is the only term that can
run away. Everything else is linear in a rate you control. So the number that
decides whether DynamoDB suits a meshql deployment is

> **`q · V`** — searches per second, times total versions in the table.

**The constant, corrected.** At 1 KiB items:

```
cost per search       = V/8 RRU = V × 1.5625e-8 USD
monthly at q per sec  = q · V × 0.0405 USD
```

so the price is **$40.50 per month per *thousand* `q·V`** — that is,
**$40,500 per million `q·V`**.

A figure of $40.50 per *million* `q·V` has been circulating internally. It is
**wrong by a factor of 1,000**, and the error is a units slip: 2.592 × 10⁶
seconds per month, not 2.592 × 10³. The arithmetic, checked against the meter:
one search per second over a table of one million 1 KiB versions examines
125,000 RRU per search, which is $0.015625 a search, which is **$40,500 a
month**. The measured run in `capacity_cost.rs` extrapolates a V=400 scan to
$0.0161 per search at V=1,000,000 — within 3% of that, from the meter.

This matters enormously, because it moves every threshold down by three orders of
magnitude:

| `q·V` | Monthly scan bill | Verdict |
|---|---|---|
| 10³ | $40 | fine |
| 10⁴ | $405 | noticeable; budget for it |
| 10⁵ | $4,050 | a judgement call, and probably no |
| 10⁶ | $40,500 | no |
| 10⁷ | $405,000 | no |

Concretely: **one search per second against a table with 25,000 versions in it
costs about $1,000 a month.** The unindexed scan path is not a thing to grow
into. It is a thing that is already too expensive at what feels like a small
table.

**But `q·V` is not your fate**, because most meshql searches need never take the
scan path at all. That is §6 and §7.

---

## 5. What meshql actually queries

The cliff above is real for *arbitrary* attribute search. meshql does not do
arbitrary attribute search.

Every query template is a fixed string handed to `RootConfig::builder()` at
build time, in source-controlled configuration. Nothing about a request chooses a
field to filter on. Across every entity in `examples/*` there are **156 configured
queries**, and they reduce to **13 distinct template strings**:

| Shape | Count | Path today |
|---|---|---|
| `{"id": "{{id}}"}` | 69 | `Query` — 0.5 RRU |
| `{"payload.<field>": "{{arg}}"}` | 74 | **`Scan`** — `V·S/8192` RRU |
| `{}` (`getAll`) | 13 | **`Scan`** — `V·S/8192` RRU |

There are **no multi-condition templates and no operators other than equality**.
The 74 single-field templates name exactly **ten** distinct fields — eight foreign
keys (`farm_id`, `coop_id`, `hen_id`, `container_id`, `consumer_id`, and the
camelCase variants), plus `name`, `zone`, `date`.

Two consequences follow, and they are the heart of this document.

**The index set is derivable, not a tuning exercise — and it is now derived.**
Because templates are static, walking the configured `RootConfig`s at startup
yields the complete, closed set of fields any deployment will ever filter on. An
index set derived from the same configuration that generates the queries cannot
drift out of sync with them — a strictly stronger property than a hand-maintained
list. It also means the adapter can **fail at startup** if a configured template
names an unindexed field, rather than silently degrading to an O(V) scan. Silent
fallback is how a deployment comes to believe it is indexed when it is not.

This is what `meshql_dynamo::IndexPlan` does, and a deployment declares nothing
beyond the config it already has:

```rust
use meshql_dynamo::DynamoCollection;

let coops = DynamoCollection::open(None, "coops", &coop_config).await?;
```

`coop_config` is the same object the graphlette gets. `DynamoCollection` builds
the repository and the searcher from one derived plan, so the half that writes
the promoted attributes and the half that reads the index cannot disagree —
which is the failure worth engineering against, because a repository that does
not promote makes every indexed search return *nothing*, silently. §7a is what
happens when someone builds the halves separately anyway.

**`getAll` is different in kind**, not merely in degree, and no index can fix it.
That is §8.

---

## 6. Indexing a payload field: the two-phase query

**Shipped.** The indexed fields are *derived* from the deployment's own query
templates (§5) rather than listed — a physical consequence of the configuration
that changes cost and not semantics. Each derived field is promoted to a
top-level `ix_{field}` attribute on write, and gets a `KEYS_ONLY` global
secondary index with hash key `ix_{field}` and range key `sk`. Only **string**
payload values are promoted, which is a soundness condition rather than a
shortcut: the matcher compares JSON values, so `"42"` never equals `42`, so a
record holding a number at an indexed path can never match a string predicate and
its absence from the index is correct.

A query then runs in **two phases**:

```
phase 1   GSI Query: ix_field = :value AND sk < :cutoff   →  candidate ids
phase 2   query_latest(id) for each distinct candidate    →  resolved versions
          re-check the predicate against the resolved version
```

### Phase 2 is required. It is not an optimisation you can skip.

A GSI returns the *versions* whose indexed value matched. That is not the set of
records whose **resolved** version matches: a record that used to be
`kind = tool` is still in the `tool` partition of the index forever.

The tempting shortcut is an `ALL`-projection index, resolving latest-per-id
*inside* the index results and re-checking the predicate there — cost `f × scan`,
no per-id floor. **It is unsound**, and the probe in
`meshql-dynamo/tests/index_cost.rs` demonstrates it against real data rather than
arguing it:

```
id "mover":   v1 (kind = tool)  →  v2 (kind = widget)
id "stayer":  v1 (kind = tool)

GSI query kind = tool           returns v1 of "mover", v1 of "stayer"
group within the index          "mover" resolves to v1
re-check predicate on v1        kind = tool — matches → RETURNED  ✗
```

Measured output, reproduced against the **shipped** searcher in
`tests/index_cost.rs`: phase 1 alone offers `{mover, stayer}` and the shipped
two-phase search returns `{stayer}`. `mover`'s actual resolved version is `v2`,
whose kind is `widget`. The index **cannot see v2** — v2 lives in the `widget`
partition of the index — so no amount of re-checking inside the result set can
detect the error. This is the same class of bug that
`test_searcher_auth_latest_version_controls_visibility` exists to catch, and
`tests/searcher_cert.rs::the_index_cannot_resurrect_a_superseded_version` pins
it on DynamoDB Local as part of the ordinary suite.

Since a projected index buys nothing, **`KEYS_ONLY` is the right projection**.
Both halves of that were checked rather than assumed:

- *Buys nothing:* `store::query_index_candidates` reads exactly one attribute
  out of an index response — `pk`, the base table's hash key — which
  `KEYS_ONLY` projects. Every candidate is re-read from the base table in phase
  2 regardless. There is nothing a wider projection could supply.
- *Costs more:* at 1 KiB items an `ALL` index and a `KEYS_ONLY` one both round
  to the same kilobyte and cost identically, which is what the earlier
  measurement found and why it must not be generalised. **At 3 KiB items,
  measured: a `KEYS_ONLY` index entry costs 1 WRU and an `ALL` entry costs 4 —
  a second full copy of the item, four times the index capacity**, plus the
  storage forever.

### What it costs

With `f` the fraction of versions matching the predicate, and `G` the index item
size:

```
scan       = V·S / 8192
two-phase  = f·V·G / 8192   +   f·M / 2
             └ phase 1 ┘        └ phase 2 ┘
```

`G` was **measured at 74–82 bytes** for a `KEYS_ONLY` index. Phase 2 metered
**exactly 0.5 RRU per candidate id**, as predicted.

Both cases measured at V = 1,000, S ≈ 1 KiB:

| Corpus | `r` | `f` | Phase 1 | Phase 2 | Two-phase | Scan | Ratio |
|---|---|---|---|---|---|---|---|
| projection-like, 10 kinds | 10 | 0.1 | 1.0 RRU | 5.0 RRU | **6.0 RRU** | 127.0 RRU | **21× cheaper** |
| event-like, 3 kinds | 1 | 0.33 | 3.0 RRU | 167.0 RRU | **170.0 RRU** | 127.0 RRU | **1.34× dearer** |

The model predicted 6.05 RRU and 1.36× respectively. It is accurate to about 1.5%.

**At production scale the gap is the whole argument.** A foreign-key search
returning ten records from a table of one million versions over 100,000 ids:
phase 1 reads 100 index entries (1.0 RRU), phase 2 resolves 10 ids (5.0 RRU) —
**6.0 RRU**. The scan it replaces was **measured** at **122,254 RRU** (§11). That
is a **20,000× reduction**, and it converts a $40,500/month search workload into
one costing **$2.00/month**. The index does not move the cliff further away; it
removes `V` from the cost function altogether — two-phase cost depends on how many
records *match*, not on how many exist.

### The counterintuitive part: an index can make it worse

Phase 2 pays a **0.5 RRU floor per candidate id**, because a point `Query` rounds
up to 4 KB. A scan reads the same bytes in bulk at 0.5 RRU per 4 KB. **At 1 KiB
items a point read is therefore four times dearer per byte than the identical
bytes inside a scan.** The index wins only when `f` is small enough to pay for
that floor:

> index wins iff `f · (0.0095 + 1/(2r)) < 0.125`

| versions per id `r` | index wins while |
|---|---|
| 1 | `f < 0.245` |
| 2 | `f < 0.479` |
| 4 | `f < 0.923` |
| **≥ 5** | **always** |

And `r` is not a free parameter — it is determined by what kind of entity you
indexed:

- **meshql event entities are create-only.** A correction is a new event, never
  an edit, so events sit at **`r = 1`**.
- **meshql projections are folded repeatedly** by workers, so they sit at large
  `r`.

So: **index projections freely. On event entities, index only selective fields.**
A low-cardinality predicate on an event entity — `byStatus` over three values,
`byKind` over five — can cost *more* than the scan it replaced, and it adds write
amplification on top. "I added an index and it got slower and dearer" is a real
outcome here, and it is measured above, not hypothetical.

The rule of thumb that falls out: **index high-cardinality fields freely**
(`byName`, `bySlug`, `byEmail`, foreign keys — `f` is tiny and the win is
enormous); **think twice about low-cardinality fields on event entities**, where
a scan may be both cheaper and simpler.

Note also that the 4× point-read penalty is a function of item size: at `S` = 4 KiB
the floor and the bulk rate are equal and the penalty vanishes entirely. Small
items are what make phase 2 expensive.

---

## 7. What an index costs to keep

**Write amplification.** Every index is a second item written on every write,
rounded up to *its own* kilobyte. Measured:

Re-measured through the shipped `DynamoRepository`, so these are what a
deployment is billed and not what a hand-built imitation was billed:

| Plan | Metered WRU for one <1 KiB write |
|---|---|
| no indexes | **1** |
| one field | **2** |
| two fields | **3** |
| two fields, one **absent** from the payload | **2** |
| two fields, **both absent** | **1** |

So at 1 KiB envelopes **each index adds exactly 1 WRU per write — one index
doubles your write bill, two treble it**. In money: **+$1.62/month per sustained
write/sec per index**, the same unit as the entire base write cost.

**Sparse indexes are free — confirmed.** A version whose payload lacks the
indexed field writes no index entry and pays no index capacity at all, and the
both-absent row shows this is a genuine zero rather than a rounding artefact.
**Optional fields are free to index**, which matters because a derived index set
indexes every field any template mentions, including the ones most records do
not carry.

At 1 KiB an `ALL` projection costs the same WRU as `KEYS_ONLY` — both round to
the same kilobyte — and that coincidence is the whole reason the earlier
measurement showed no difference. At 3 KiB items the `ALL` entry costs **4 WRU
against `KEYS_ONLY`'s 1**. Use `KEYS_ONLY`; §6 shows a wider projection is never
even read.

**A trap worth naming.** Promoting a field to `ix_{field}` adds bytes to the
*base* item. Measured: an envelope sitting at exactly 1024 bytes cost 1 WRU
unpromoted, and **3 WRU** promoted-and-indexed — because the 12 extra bytes of
`ix_kind` pushed the base item over the kilobyte boundary, costing a WRU *before*
the index write. If your items cluster near a kilobyte multiple, promotion alone
can add 50% to your write bill.

**Does the index pay for itself?** Saving per search is roughly `(1−f)·V/8` RRU;
cost is `w` extra WRU per second. Break-even:

> **`q · (1−f) · V  >  40 · w`**

(A figure of `40,000 · w` has circulated; it carries the same factor-1,000 slip as
the `q·V` constant. The correct threshold is 40.)

That bar is extremely low. At one search per second, one write per second and
`f` small, it is cleared at **V > 40 versions**. In practice: if you search a
field at all, index it — unless you are in the `r = 1` low-selectivity corner
above.

**The 20-index limit.** DynamoDB allows **20 GSIs per table** by default. What
rescues a wide deployment is one-table-per-collection: the ten indexed fields
across `examples/*` are spread over thirteen entities, so a typical table needs
one or two indexes, not ten. But a single wide projection entity could exceed 20,
so `IndexPlan::derive` **fails at startup with a message naming every field**
rather than discovering it at `CreateTable` time or, worse, silently scanning.

---

## 7a. The four ways an indexed deployment can be wrong, and what stops each

Every one of these produces *silence* if unguarded — a search that returns fewer
records than exist, with no error — which is why each is a refusal to start
rather than a warning. Each was verified by breaking it and watching a test go
red; a guard that cannot fail is decoration.

| What goes wrong | What stops it |
|---|---|
| A template names a field with no index | Refused at query time, naming the field, the table and the template. Never a scan. |
| More than 20 derived indexes | `IndexPlan::derive` fails at startup, naming the fields. |
| The repository does not promote what the searcher indexes | `DynamoCollection` builds both from one config; and opening a table whose `meshql_ix_*` indexes disagree with the handle's plan is refused. |
| An index is added to a table that already holds data | Refused: promotion happens on write, so stored versions carry no `ix_` attribute and the new index cannot see them. `meshql_dynamo::migrate_indexes` rewrites the promoted attributes and *then* creates the indexes, so there is never an interval in which a half-built index is queryable. |

The migration is `O(V)` — one `Scan` plus one `PutItem` per stored version,
about **$0.63 per million versions** — and it is paid once per new index, not
per query. Compare that to `O(V)` on *every* search, which is what §4 is about.

A template whose keys are neither `id` nor `payload.…` is refused at startup
too, but for a different reason: it matches nothing on *every* meshql backend
(see the matcher), so it is a configuration bug rather than a cost problem. At
runtime the same shape returns empty with no request at all, which is exactly
what the scan would have returned, for nothing.

---

## 8. The irreducible case: `getAll` and `list(tokens)`

`{}` templates and `Repository::list(tokens)` are "every record this caller can
see". They cost `V·S/8192` RRU, and **no index can fix them.**

**Why not.** Visibility is `authorized_tokens`, which is a DynamoDB **list**
attribute (`L`). GSI keys must be scalar — `S`, `N` or `B`. A list attribute
cannot be a key, so token-visibility is not indexable at all. On top of that,
visibility is not equality: empty tokens mean public, `"*"` means everyone, and
otherwise it is set intersection. There is no key condition that expresses it.

**And the ordering forbids stopping early.** meshql orders by
`(created_at, id)` **ascending, on the resolved version**. An id's resolved
version can sit anywhere in that order regardless of where its first version sits,
so you cannot know a record's position until you have resolved it — which means
no backend can stop reading early, even with a limit. (Had the contract been
newest-first, `getAll(limit: n)` would be an `O(n)` backwards walk over a
`(constant, sk)` index instead of an `O(V)` scan. That is a large cost
consequence of an ordering decision, and it is a contract change, not an adapter
change.)

**So how expensive is it really?** At 1 KiB items, one call costs `V/8` RRU:

| V | Cost of one `getAll` | At 1/hour | At 1/minute | At 1/sec |
|---|---|---|---|---|
| 10,000 | $0.000125 | $0.09/mo | $5.40/mo | $324/mo |
| 100,000 | $0.00125 | $0.90/mo | $54/mo | $3,240/mo |
| 1,000,000 | $0.0125 | $9/mo | $540/mo | $32,400/mo |
| 100,000,000 | $1.25 | $900/mo | $54,000/mo | $3.24M/mo |

**What to tell a client.** `getAll` is an *export*, not a *view*. Specifically:

- **Never put it on a per-request path.** The difference between a $155/month
  deployment and a $2,800/month one, in §11's growth example, is one `getAll` per
  hour.
- **A limit does not help** — measured, §2. Pagination does not bound it.
- **Bound `V` instead**, with a temporal horizon (§9). This is the only lever that
  reduces the cost rather than the frequency.
- **Cache it.** It is a whole-table snapshot; if you need it often, materialise it
  on a schedule and serve the materialisation.
- **Replace it with a scoped query.** In most applications "everything" is really
  "everything in this workspace", which is an indexable foreign key and belongs in
  §6.

**The one partial mitigation, and its sharp edge.** If — and only if — a
deployment guarantees that `authorized_tokens` is exactly `[tenant]` for every
envelope, a scalar `ix_tenant` attribute can be indexed and "list everything I can
see" becomes a two-phase query on `tenant`. Be clear about what that is: it is a
**deployment-level invariant that the adapter cannot verify**, and it is wrong the
moment anything is public, shared with two tokens, or marked `"*"`. It must never
be a default, and it must never be applied *instead of* the token filter — only as
a candidate-narrowing step ahead of it, with the certified intersection rule still
applied to the resolved version. The permissive-by-default behaviour of an empty
token set is deliberate design (it is what `meshql-cert`'s authorization suite
certifies), not an oversight to optimise away.

---

## 9. Storage compounding, and what actually compounds

meshql is append-only: `V` only ever grows. So a *constant* write rate adds a
*constant increment* to a *growing* base.

At 1 KiB items, one sustained write per second is 2,592,000 versions per month =
**2.71 GiB per month**, which adds **$0.68 per month, every month** —
so the *increment* is constant while the *bill* accelerates from zero.

(An internally circulated figure of $0.66 is right; the small difference is
whether you count DynamoDB's 100-byte per-item overhead and whether "GB" means
10⁹ or 2³⁰. The honest range is $0.62–$0.73. This is the one figure that was
already correct.)

Because of the 25 GB free allowance, storage is **$0 until about 24 million
versions** — roughly 9 months at one write per second.

### Three years, two write rates

| | `w` = 1 write/sec | `w` = 10 writes/sec |
|---|---|---|
| Versions after 36 months | 93.3 million | 933 million |
| Table size | 97.7 GiB | 977 GiB |
| Storage bill, month 12 | $1.89/mo | $75.15/mo |
| Storage bill, month 36 | **$18.17/mo** | **$237.90/mo** |
| Cumulative 3-year storage | ≈ $252 | ≈ $4,293 |

**And here is the part clients do not anticipate.** Storage compounding is
trivial. What compounds catastrophically is that **search cost is proportional to
storage**. The same table, with a single unindexed search running once every 100
seconds (`q` = 0.01):

| Month | `w`=1: V | Storage/mo | **Scan search/mo** | `w`=10: V | Storage/mo | **Scan search/mo** |
|---|---|---|---|---|---|---|
| 1 | 2.6M | $0 | **$1,050** | 25.9M | $0.53 | **$10,498** |
| 12 | 31.1M | $1.89 | **$12,595** | 311M | $75.15 | **$125,955** |
| 36 | 93.3M | $18.17 | **$37,786** | 933M | $237.90 | **$377,865** |

Storage grows from $0 to $18. The unindexed search on the same data grows from
$1,050 to $37,786 — **two thousand times the storage line**, on identical data. If you take one
number from this document, take this one: on DynamoDB, append-only storage is
cheap and append-only *search* is not, and they grow together.

Indexing (§6) removes the `V` term from search entirely; the two-phase cost
depends on `f·V` and `f·M`, i.e. on how many records *match*, not on how many
exist. That is what turns the middle column above from a cliff into a flat line.

---

## 10. The mitigation ladder

Cheapest first.

### (a) A temporal horizon, via DynamoDB TTL — free, ~an hour to build

**TTL deletions consume no write capacity.** Two independent confirmations:

1. Enumerating all 792 DynamoDB usage types in the Price List API and grepping for
   `ttl|expir|delete|timetolive` returns **zero matches**. There is no TTL SKU.
2. The developer guide, at
   <https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/TTL.html>:
   > "DynamoDB automatically deletes expired items within a few days of their
   > expiration time, **without consuming write throughput**."

So a horizon is a genuine cost lever and not merely a deferral: add a numeric
`ttl` attribute at write time, enable TTL on it, and `V` stops growing without
bound. Both the storage term and — much more importantly — the `q·V` search term
become **constant** rather than linear in age. At `w` writes/sec and a horizon of
`H` days, `V` settles at `w · H · 86,400`.

Two caveats, and they matter:

- **This deletes history.** `at:` queries older than the horizon stop resolving,
  and a full projection replay (§12) is truncated at it. TTL and
  indefinite-replay are in direct tension; choose deliberately.
- Deletion is asynchronous, "within a few days" — so the horizon is soft, and `V`
  is bounded approximately, not exactly.

This is something **merk-aws cannot do**: nothing deletes sealed segments, so a
merkql log grows without bound by construction. It is a real and specific
advantage of the DynamoDB backend.

### (b) Parallel `Scan` — latency only, no cost change — **shipped, opt-in**

`scan_latest` chains `LastEvaluatedKey`, so its pages are strictly **serial**.
DynamoDB's `Segment` / `TotalSegments` partitions a scan so `n` workers can walk
disjoint slices concurrently. Because capacity is charged on bytes examined and
the segments partition the same bytes, **RRU is unchanged — measured invariant to
0.012% across a 64× segment range.**

The expected caveat was that a *small* table would behave differently, since each
segment's final page rounds up to its own 4 KB boundary and there the rounding is
the whole bill. **Measured, it does not, at four segments.** A three-item table
meters 2.0 RRU serially and 2.0 RRU at four segments — a serial `Scan` is already
charged per partition, so four segments merely re-partition a rounding that was
being paid anyway. Sixteen segments on the same table costs 9.5 RRU, so the
penalty is real, but it starts above the table's own partition count and not at
four.

The speedup is **~2.6×, not `n`×** (§11). It plateaus at four segments against a
consumer-side bandwidth/CPU ceiling, so it turns a 45-second scan at V=1M into a
17-second one and no further.

`DynamoSearcher::with_scan_segments(4)` turns it on for the paths that remain
scans. **The default is one**, and the reason is not cost: on a table small
enough to fit in a page four round trips buy nothing, and above that the win
belongs to an export choosing it deliberately rather than to every request. Do
not expect it to rescue a large table — that is what §10(c) is for.

### (c) Derived indexes + GSI — **built**; fixes both

§5, §6, §7 and §7a. This is the one that removes `V` from the search term. The
index set is derived from the deployment's own `RootConfig`, so a deployment
declares nothing: `DynamoCollection::open(endpoint, table, &config)`.

It is two-phase (§6), equality-only, `KEYS_ONLY`, and it fails at startup on an
unindexed configured field and on more than 20 indexes (§7a).

The certification suites run **twice**, once unindexed and once with the indexes
derived from the templates those very suites use — repository, repository
authorization, searcher and the end-to-end authorization feature. Indexing is a
change to what a search *costs* and it has to be no change at all to what a
search *means*; running the certs against one path only would certify half an
adapter, and the half left out is the half where a superseded version can leak
back into a result set.

---

## 11. Latency

Measured from an **arm64 `provided.al2023` Lambda in `us-east-1` at 2048 MB**,
against on-demand tables in the same region, at four table sizes with ~1 KiB
items and ten versions per id.

**Vantage check.** The same binary, the same 100 samples, the same table, run
from the Lambda and from a workstation:

| Operation | Lambda p50 | Workstation p50 | Ratio |
|---|---|---|---|
| `PutItem`, 1 KiB | **3.35 ms** | 32.82 ms | 9.8× |
| by-id `Query` | **2.37 ms** | 30.78 ms | 13.0× |

In-region point operations land at 2.4–3.4 ms, inside the expected 3–10 ms band,
and about ten times better than the workstation — the same shape as the prior
in-region S3 Express result in this account (7.6 ms against ~42 ms). **The vantage
is sound**, and roughly 30 ms of workstation round trip would otherwise have
contaminated every figure below.

### The table

| Operation | V=1.2k | V=10k | V=100k | V=1M | n |
|---|---|---|---|---|---|
| `create` p50 | 3.83 | 3.56 | 3.49 | 3.84 | 150 |
| `create` p99 | 5.88 | 6.42 | 6.99 | 7.73 | |
| by-id read p50 | 2.30 | 2.15 | 2.50 | **2.14** | 200 |
| by-id read p99 | 4.00 | 3.52 | 4.77 | 3.55 | |
| `read_many` k=1 p50 | 2.06 | 2.07 | 2.51 | 1.94 | 200 |
| `read_many` k=10 p50 | 3.53 | 3.44 | 3.85 | 3.55 | 200 |
| `read_many` k=10 p99 | 23.29 | 25.95 | 22.37 | 24.83 | |
| `read_many` k=100 p50 | 22.32 | 21.61 | 22.53 | 23.98 | 200 |
| `read_many` k=100 p99 | 97.31 | 52.02 | 117.55 | **281.46** | |
| **search p50** | **59.6** | **430** | **4,475** | **45,033** | 30/30/30/10 |
| **search p99** | 90.0 | 487 | 4,963 | 46,300 | |

All figures in milliseconds. Zero query misses in 89,600 by-id queries.

**By-id reads are flat in `V`** — 2.14 ms at a million versions against 2.30 ms
at twelve hundred, across an 826× range. The sort-key design does exactly what it
claims.

**`read_many` is not free concurrency.** k=100 has a p50 of 22 ms, about ten
times k=1, and a p99 that reaches **281 ms** at V=1M. A hundred concurrent
`Query` calls saturate the connection pool, and the fan-out is bounded by the
slowest of the hundred, not by one round trip. Budget `read_many` off its p99.

### The O(V) sequential-scan claim: confirmed, and worse than predicted

| V | Predicted pages `ceil(V·S/1 MiB)` | **Measured pages** | p50 wall | RRU |
|---|---|---|---|---|
| 1,210 | 2 | **2** | 59.6 ms | 149.5 |
| 10,000 | 10 | **10** | 430 ms | 1,224 |
| 100,000 | 96 | **96** | 4,475 ms | 12,227 |
| 999,993 | 956 | **956** | **45,033 ms** | 122,254 |

The page count matched the prediction **exactly, four times out of four**, and was
identical on every one of the 100 scan runs — no variance whatsoever. So round
trips are predictable without measuring them.

Wall clock is `pages × RTT` at a flat 21–23 pages/second, i.e. **~47 ms per
serialised round trip** — twenty times a point `Query`, because each page carries
a full 1 MiB instead of one item, and the pages cannot overlap because each needs
the previous response's `LastEvaluatedKey`.

**The "multiple seconds at V=1M" prediction understates it by an order of
magnitude. A search at V=1M takes 45 seconds.** That exceeds the API Gateway
29-second timeout, most load-balancer defaults, and every interactive budget there
is. A single non-`id` search at a million versions is not a slow request; it is an
unservable one.

### Parallel scan: the money claim holds, the latency claim does not

`Segment` / `TotalSegments`, run concurrently and merged latest-per-id in Rust.
Prototyped in `dynamocost-bench`; **`scan_latest` itself was not changed.**

| Segments | V=100k wall | V=1M wall | Speedup | **V=1M RRU** |
|---|---|---|---|---|
| 1 | 4,613 ms | 45,403 ms | 1.00× | **122,254.5** |
| 4 | 1,759 ms | 17,256 ms | **2.63×** | **122,254.0** |
| 16 | 1,704 ms | 16,974 ms | 2.67× | **122,258.5** |
| 64 | 1,727 ms | 17,578 ms | 2.58× | **122,269.0** |

**RRU invariance: confirmed, emphatically.** Across a 64× change in segment count
the capacity moved by **0.012%**. Capacity is charged on bytes examined and the
segments partition the same bytes, so parallelism here is *free*. The slight
upward drift is exactly explicable — more segments means more partial final pages,
each rounding up to its own 4 KB boundary, visible in the page count climbing
956 → 986 while RRU barely moves.

**And it holds further down than expected.** The natural objection is that a
small table is all rounding, so the invariance should break there. Measured on a
three-item table: **2.0 RRU serial, 2.0 RRU at four segments**, 9.5 at sixteen. A
serial `Scan` is already charged per partition — 2.0 RRU for 600 bytes is four
roundings, not one — so four segments re-partition a cost that was being paid
anyway. The penalty begins above the table's own partition count.

**Linear speedup: refuted.** The wall clock falls 2.6× from one segment to four
and then stops dead; sixteen and sixty-four are no faster than four. The plateau
sits at ~58 MB/s at both V=100k and V=1M, which is a **consumer-side** ceiling,
not a DynamoDB one: re-running on a 10 GB Lambda moved it to 83 MB/s, still flat
from 16 to 64 segments, with peak memory use of only 378 MB. It is bandwidth
and/or the CPU cost of deserialising a gigabyte into `Envelope`s — those two were
not separable with the instrumentation available.

So: **use four segments, and do not bother with more.** Parallel scan turns 45
seconds into 17 for identical money, which is worth having, but it does not make
`V`=1M searchable. Only an index does that.

## 12. Worked examples

All at S = 1 KiB, `us-east-1` on-demand, 30-day month, with the 25 GB storage
allowance.

### A. A small internal application

An asset register for 40 staff. `w` = 0.01 writes/sec, `q_id` = 2 reads/sec, one
foreign-key search per page view (`q_fk` = 1/sec) returning ~10 records, an admin
export three times an hour. After two years: **V = 622,000**, M = 62,000, r = 10.

| Line | Unindexed | With 1 GSI |
|---|---|---|
| Writes | $0.02 | $0.03 |
| By-id reads | $0.32 | $0.32 |
| FK search | **$25,191** | **$1.94** |
| `getAll` (3/hour) | $25.19 | $25.19 |
| Storage (0.65 GiB) | $0 | $0 |
| **Total** | **≈ $25,220/mo** | **≈ $27.50/mo** |

The entire question is the index. Note that even in the good column the largest
line is the admin export.

### B. A growth-mode product

`w` = 5 writes/sec, `q_id` = 200 reads/sec, `q_fk` = 20 searches/sec, two indexed
fields on the busy table, `getAll` moved off the request path to once a day.
After 18 months: **V = 233 million**, M = 23.3 million, r = 10.

| Line | Cost |
|---|---|
| Writes, 5/sec × (1 base + 2 indexes) | $24.30 |
| By-id reads, 200/sec | $32.40 |
| FK searches, 20/sec two-phase (6.0 RRU each) | $38.88 |
| Storage, 244 GiB | $54.80 |
| `getAll`, once a day at V=233M | $109.40 |
| **Total** | **≈ $260/mo** |

Two hundred reads a second and twenty searches a second, for the price of a small
RDS instance. Move that `getAll` to hourly and it becomes **$2,775/mo** — one line
item dominating every other combined.

### C. Where DynamoDB is the wrong answer

A multi-tenant SaaS dashboard whose landing view is "everything in my workspace",
5,000 tenants, 50 requests/sec, **V = 50 million**, M = 5 million.

| Approach | Monthly | Note |
|---|---|---|
| Unindexed — `Scan` per request | **$101,250,000** | 6.25M RRU per request |
| Tenant-scoped GSI, two-phase | **$9,720** | ~600 RRU/request; requires an authorization assumption the adapter cannot verify |
| Postgres on RDS + 300 GB gp3 storage | **≈ $200–400** | one indexed query; reads are free once the instance is bought |

This is an honest loss. Even the best DynamoDB shaping is **roughly 25–50× more
expensive** than Postgres here, and it gets there only by assuming `authorized_tokens` is
always a single tenant token — which breaks the moment a record is shared or made
public. And because meshql applies `limit` last, paginating the dashboard does not
reduce the cost at all: you resolve all 1,000 candidate ids to return the first
20.

The shape to recognise: **a workload whose primary access pattern is "list what
this principal can see", at any meaningful rate, is wrong on DynamoDB.** Token
visibility is a list attribute and cannot be indexed. Either restructure the data
model (a per-principal index table maintained by a projection worker — real
engineering) or use a backend that can index a set, such as Postgres with a GIN
index on the token array.

---

## 13. Against the alternatives

Same workload as example B (`w`=5/sec, `q_id`=200/sec, `q_fk`=20/sec, V=233M).

| | DynamoDB | merk-aws (S3 Express One Zone) | Postgres / RDS | Mongo / Atlas | SQLite |
|---|---|---|---|---|---|
| Write path | $24.30 | $34.10 | included | included | included |
| Storage | $54.80 (244 GiB, 25 free) | $24.47 (222 GiB @ $0.11) | $34.50 (300 GB gp3) | included in tier | ~$27 (EBS) |
| Read path | $71.28 metered | **cannot serve reads** | included | included | included |
| Instance | none | none | ~$165/mo (est.) | ~$400+/mo M40 (est.) | host only |
| **Total** | **≈ $260/mo** | **≈ $59/mo, write path only** | **≈ $200/mo** | **≈ $400+/mo** | host only |
| Scales to zero | yes | yes | no | no | n/a |
| Per-id read | 1 round trip, 0.5 RRU | n/a | 1 indexed query | 1 indexed query | 1 indexed query |
| Arbitrary attribute search | needs an index or it is O(V) | n/a | free | free | free |
| HA / multi-writer | built in | yes | yes (Multi-AZ) | yes | **no** |

**Only the DynamoDB and S3 columns are Price List API figures.** The verified RDS
rates are `db.t4g.medium` Single-AZ PostgreSQL at $0.065/hour and gp3 storage at
$0.115/GB-month; the larger instance sizes and every Atlas figure are rate-card
recollection, not fetched, and are marked `(est.)`. Treat them as ±50% and price
them properly before quoting a client.

### On merk-aws specifically

The comparison is **not like for like**, and that is the most important thing to
say about it. merkql is an embedded Rust library, not a service; it has no query
surface. A merk-aws deployment serves reads out of *projections*, which live in
some other store. So the $59/month above is a write path plus storage, and you
must add a read store to it. DynamoDB's $259 includes serving 200 reads and 20
searches per second.

Three findings from `sociallymeshy/docs/refresh/benchmark-0.md` carry directly
into this comparison:

**merk-aws pays about 2 billed write requests per event, permanently.** Conflicts
there are not contention — they are **stale-cache misses**. `append_batch` derives
the append offset from the writer's cached view and refreshes only after
rejection, so the conflict rate is set by the number of concurrent writer
*processes*, not by load. Measured: **44.5% conflicts at eight events per second**
across eight partitions — two orders of magnitude below saturation — statistically
indistinguishable from the 55.8% at saturation. Backing load off by 25× changed
nothing. **62% of that benchmark's S3 request bill was conflicts.**

DynamoDB has no equivalent. A `PutItem` is a single-item write with no
read-modify-write, so there is nothing to conflict on, and one write is exactly
one WRU. **Per event: $0.625 per million on DynamoDB against $2.63 per million on
merk-aws** — DynamoDB is **4.2× cheaper per event**, and does not degrade with
writer count.

(That $2.63 supersedes benchmark-0.md's $6.80 per million. The benchmark used
$0.0025/1,000 for S3 Express PUTs; the Price List API today returns
**$0.00113/1,000** under usage type `Requests-XZ-Tier1`. The Express One Zone
request rates have come down substantially — storage is $0.11/GB-month, GETs are
$0.00003/1,000 — and any comparison using the older figures overstates merk-aws's
request bill by about 2.3×.)

**Batching changes the comparison, and the gateway cannot use it.** Batch-100
gives 0.19 write requests per record — twelve times cheaper per event, and it
would make merk-aws cheaper than DynamoDB per event. But p50 rises from ~8 ms to
**305 ms**, and a meshql gateway has one event per request and returns `201`
meaning committed, so there is no batching opportunity on the write path at all.
Batching also drops events at low partition counts: below four partitions, batch-1
`send` exhausts its 64 retries and **loses events outright** — 2.5% at one
partition and sixteen writers. Any merk-aws deployment needs at least four
partitions before it is merely correct.

**Latency.** merk-aws append p50 is 7.6 ms single-writer but **~30 ms with any
real concurrency**, because every conflict costs a re-read and a backoff. A
DynamoDB `PutItem`, measured in-region on the same class of vantage, is **3.35 ms
p50 and 7.7 ms p99, independent of concurrency** — there is no conflict term to
pay. That is roughly **9× better at p50** under the conditions a real gateway
runs in.

---

## 14. Can DynamoDB replace merk-aws as the event log?

This is the live architectural question, so here is a straight answer with the
objections addressed rather than skirted.

**The envelope table already is an append-only log.** `pk` = entity id, `sk` =
`{created_at_nanos:019}#{uuid}`, nothing is ever mutated or hard-deleted, and the
sort key is a total order within a partition. That is the same structure merkql
provides, in a managed service, with a query surface attached.

**Ordering: no loss.** DynamoDB Streams orders records per **partition key**.
merkql orders per **partition**, with routing `hash(key) % num_partitions`. Since
`meshql-dynamo` uses the envelope id as `pk` and `MerkqlRepository::create`
hardcodes the envelope id as the record key, both give **per-entity ordering and
no global order**. The guarantees are equivalent; this costs nothing.

**Retention: the objection is weaker than it looks.** Streams retain **24 hours**,
against merkql's indefinite retention. But that 24 hours bounds only the
*incremental tail*, not the history: **the table retains every version forever**.
A worker down for a week has not lost data; it has lost the ability to resume from
its Streams cursor. Its options are:

- **Full replay from the table.** A `Scan`, `O(V)` paid once per rebuild rather
  than per query. At V = 233 million that is **$3.65 and, with a parallel scan, a
  few seconds**. Compare that to paying `O(V)` on *every* search, which is what
  §4 is about. Once per rebuild is a completely different economic proposition.
- **Resume from the table directly.** Because `sk` is a total order, a consumer
  can persist a watermark and query forward from it — no Streams involved, and
  therefore no retention limit at all. Doing this efficiently across all
  partitions needs a temporal index (a GSI with a bucketed constant hash key and
  `sk` as range), which costs +1 WRU per write and needs its hash sharded to avoid
  a hot partition — a single index partition caps around 1,000 WCU/sec.

So the honest form of the constraint is: **Streams' 24 hours costs you incremental
resumability, not durability, and the table gives you a way to buy resumability
back.** merkql's indefinite retention is a genuine advantage, but a much narrower
one than "24 hours versus forever" suggests.

**Where DynamoDB is actually better:** consumer checkpointing. merkql's consumer
groups have two confirmed defects — `auto_commit: true` plus a failing handler is
permanent silent loss, and commit offsets are last-write-wins across partitions of
the same group, so concurrent commits clobber each other. DynamoDB Streams with a
Lambda event-source mapping has neither: checkpointing is managed, failures retry,
and there is no shared mutable offset map to lose a write.

**Where merk-aws stays better:** indefinite retention with no extra index;
merkle-proof verifiability of the log, which DynamoDB does not offer at all;
cheaper storage ($0.11 vs $0.25 per GB-month); and no coupling of log retention to
a TTL decision you may want to make for cost reasons (§10a).

### Recommendation

**Yes — use DynamoDB as the event log for meshql deployments, unless you need
verifiable log integrity or guaranteed indefinite replay.**

It is cheaper per event (4.2×), lower-latency, free of merk-aws's permanent ~2×
conflict amplification, has no minimum partition count to avoid dropping events,
serves reads directly instead of requiring a separate projection store, and has
sounder consumer checkpointing. Adopt Streams for the low-latency tail and accept
that a consumer down more than 24 hours full-replays from the table — a
manoeuvre that costs single-digit dollars.

Keep merk-cloud for the case it is actually built for: **a log whose integrity
must be provable and whose history must be replayable indefinitely.** That is a
narrower brief than "the event log for the platform", and it is a real one.

The tension to hold explicitly: enabling TTL (§10a) to control search and storage
cost is exactly what forfeits indefinite replay. Do not enable it on an entity
whose history a projection may need to rebuild from.

---

## 15. Measuring your own workload

Do not take this document's word for your bill. Attach the meter:

```rust
use meshql_dynamo::{CapacityMeter, DynamoRepository, DynamoSearcher, Rates};

let meter = CapacityMeter::new();
let repo = DynamoRepository::new(None, "farms").await?.with_meter(meter.clone());
let searcher = DynamoSearcher::new(None, "farms").await?.with_meter(meter.clone());

// ...serve traffic...

let report = meter.snapshot();
println!("{report}");
println!("cost so far: ${:.6}", report.cost_usd(&Rates::ON_DEMAND_US_EAST_1));
```

The report breaks capacity down by `PutItem` / `Query` / `Scan` and counts round
trips, so a non-zero `Scan` line tells you immediately that some template is
missing an index. `CapacityReport::minus` gives per-interval figures without
resetting.

It is **off by default**: with no meter attached, `ReturnConsumedCapacity` is
never set and the request on the wire is unchanged. Metering cannot alter which
items a request matches — it adds a field to the *response* — and
`tests/capacity_cost.rs::metering_does_not_change_results` runs every read path
through metered and unmetered handles over one table and requires identical
results.

---

## 16. What was validated, and how

`meshql-dynamo/tests/capacity_cost.rs` — **66 checks, all passing**, real AWS,
comparing predicted capacity to `ConsumedCapacity`.
`meshql-dynamo/tests/index_cost.rs` — **29 checks, all passing**, real AWS: what
the **shipped** derived-index path costs. It replaces the design probe that
preceded it, which built its tables and ran its two-phase query by hand because
the design was then a recommendation. A cost model measured against a
hand-rolled imitation of the code is a cost model for the imitation.

Two of that suite's predictions were **wrong and were corrected by the meter**,
which is the reason for running it rather than reasoning about it: an `ALL`
projection was expected to cost the same as `KEYS_ONLY` (it costs 4× the index
capacity at 3 KiB items), and four parallel scan segments were expected to cost
more than one on a small table (they cost exactly the same).

**Every assertion was verified to be capable of failing**, by breaking what it
guards and watching a test go red:

| Mutation | What turned red |
|---|---|
| Remove the eventually-consistent halving from `metering::read_units` | **10** capacity checks, across every read path |
| An unindexed field falls back to a `Scan` instead of erroring | 2 unit tests + 1 cert |
| The repository stops promoting `ix_` attributes | **16** searcher certs — the `::indexed` half only — and 1 guard test |
| Allow a table to carry indexes the handle does not maintain | 3 guard tests |
| Allow an index on a populated table without migrating | 1 guard test |
| Remove the 20-index limit | 1 unit test |
| Version resolution returns the *oldest* version — the answer the index-only shortcut gives | 12 certs, including `the_index_cannot_resurrect_a_superseded_version` |

Two things claimed here to be *cost* optimisations rather than correctness
guards were checked the same way, by breaking them and confirming that
**nothing** turned red: intersecting several conditions' candidate sets instead
of unioning them, and bounding phase 1 by the temporal cutoff. Both are
selectivity, and phase 2 is what makes the answer right either way. (The first
attempt at the second of these turned 18 tests red and looked like a refutation.
It was not: deleting `AND #sk < :hi` from the key condition leaves the
expression's attribute names unused, which DynamoDB rejects outright. Widening
the bound to infinity instead — the mutation that actually tests the claim —
turned nothing red.)

Both **skip and exit 0** without `MESHQL_DYNAMO_COST_TESTS=1` and usable
credentials, and both refuse to run against `MESHQL_DYNAMO_ENDPOINT`. That last
guard matters: the certification suites all point at DynamoDB Local by default,
**and DynamoDB Local does not meter** — it returns no `ConsumedCapacity` at all.
"Passes all certs" attests semantics and says nothing whatsoever about cost.
That is the gap this document exists to close.

Run them:

```sh
MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 \
  cargo test -p meshql-dynamo --test capacity_cost
MESHQL_DYNAMO_COST_TESTS=1 AWS_REGION=us-east-1 \
  cargo test -p meshql-dynamo --test index_cost
```

Both create only `dynamocost-*` tables, drop them, and verify by listing that
none remain — **waiting out `DELETING` first**, which the earlier version did
not, so it once reported a leftover for a table that was already on its way out.
Verifying that teardown was *requested* is not verifying that it happened.
Together they cost well under a cent to run.

The semantic suites need no AWS at all: they run against DynamoDB Local, twice
each, indexed and not.

```sh
cargo test -p meshql-dynamo
```

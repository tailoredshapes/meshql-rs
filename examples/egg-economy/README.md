# egg-economy

A fully **event-sourced** meshql system, and the worked reference for the
[domain-design methodology](../../.claude/skills/meshql-patterns/references/domain-design.md).

The whole point in one line: **the only thing anyone writes is a business event
(a verb); every domain model (a noun) is materialized by a worker.** There are
no directly-written nouns — not even the "actors" like `farm` or `hen`.

```
  POST /<verb>/api ─► event mesh (Mongo) ─► CDC connector ─► egg-events topic
                                                                   │
                        noun mesh (Mongo) ◄── worker (fold) ◄───────┘
                                │
                        GET /<noun>/graph
```

## Verbs (business events) — the only write surface

Front ends `POST` these; each is an immutable fact. Every event mesh is
create-only.

| Verb | Builds |
|---|---|
| `build_farm` | farm |
| `build_coop` | coop |
| `buy_hens` (batch, `count = N`) | hen ×N |
| `move_hen_to_coop` | hen (coop) |
| `retire_hen` | hen (status) |
| `build_container` | container |
| `register_consumer` | consumer |
| `eggs_laid` | hen_productivity, farm_output |
| `eggs_stored` | container_inventory |
| `eggs_withdrawn` | container_inventory |
| `eggs_transferred` | container_inventory (source + dest) |
| `eggs_consumed` | container_inventory |

## Nouns (domain models) — read-only, worker-built

The actor-nouns (`farm`, `coop`, `hen`, `container`, `consumer`) and the
analytic-nouns (`hen_productivity`, `container_inventory`, `farm_output`) all
expose read-only graphlettes at `/<noun>/graph`. **They have no restlette** —
the workers are their only writers, so there is no public way to write a noun.

## How events reach the workers: storage-layer CDC, not application code

Getting events onto the log is **infrastructure, not a write-path hook**. A front
end does exactly one write — `POST` the event, which commits to the event mesh's
store. A CDC connector then observes the store's change feed and mirrors each
*committed* write onto the `egg-events` topic.

Why not publish from a restlette after the write? Because that's a dual write
with no shared transaction: crash between the DB commit and the publish and the
event is lost, silently corrupting every noun built from it. CDC derives the
event *from the committed write* — "if the event doesn't fire post-write, it
will" — and at-least-once delivery after commit is the datastore's job. Since the
folds are deterministic and idempotent, at-least-once + idempotent = exactly-once
*effect*.

- `src/source.rs` — the connector. Ships a portable `RepositoryTail` (observes the
  committed event collections) so the example runs anywhere; a production
  deployment configures a native Mongo change-stream connector instead. Either
  way, no restlette is touched.

## Workers: fold verbs into nouns

One worker per noun. Each owns a `Projector` (`src/projectors.rs`) — a pure
`reset` / `apply` / `snapshot` fold — and a noun repository. `Worker::run_once`
(`src/worker.rs`) rebuilds the noun from the entire log, so **catch-up and
replay-from-scratch are the same code path**. That is what makes a noun safe to
drop and rebuild.

## Replay: new nouns from history

Because events are immutable and every read is temporal, the log is the source of
truth and nouns are disposable:

- A **new noun** can be built at any time by pointing a fresh worker at the
  existing log from offset zero — it materializes from events recorded long
  before it existed.
- A noun is **rebuilt** by discarding it and replaying; the fix for a fold bug is
  "correct it and replay," never a hand-patch.

`tests/pipeline.rs` proves all of this end to end on an embedded merkql broker
plus in-memory SQLite — no Mongo, no Kafka: events fold into the expected actor
and analytic nouns, replay rebuilds them identically, a latecomer projection is
built from pre-existing history, and re-running a worker is a no-op.

```
cargo test -p egg-economy --test pipeline
```

## Running against Mongo

`src/main.rs` wires the full deployment on MongoDB. It expects a reachable Mongo
(`MONGO_URI`, default `mongodb://127.0.0.1:27017`) and writes the event log under
`MERKQL_DIR` (default `./egg-events-log`). It spawns the CDC connector and the 8
workers as background tasks, then serves the event restlettes + audit graphlettes
and the read-only noun graphlettes.

```
MONGO_URI=mongodb://127.0.0.1:27017 PORT=5088 cargo run -p egg-economy
```

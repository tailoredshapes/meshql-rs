# Domain Design: Events, Projections, and Workers

This is the methodology meshql is built for. The immutable Envelope, temporal reads, and per-entity mesh aren't incidental features — they exist so that a system can be modeled as **events that happened** plus **domain models derived from those events**. If you internalize one pattern from this skill, make it this one.

## The verbs and the nouns

There are exactly two kinds of thing, and the whole methodology is the bridge between them. `examples/egg-economy/` is the worked reference — a fully event-sourced system.

| Kind | What it is | Written by | Examples |
|---|---|---|---|
| **Business event** (verb) | An immutable fact that happened | **Front ends** — the *only* write surface | `build_farm`, `buy_hens`, `move_hen_to_coop`, `eggs_laid`, `eggs_transferred` |
| **Domain model** (noun) | A model derived by folding events | **Workers only** — never written directly | `farm`, `hen`, `container` (actor-nouns); `hen_productivity`, `container_inventory`, `farm_output` (analytic-nouns) |

The key realization ("¿por qué no dos?"): **you keep both, and workers translate verbs into nouns.** Crucially, there are *no directly-written nouns* — even the actor-nouns you'd instinctively `POST` (a farm, a hen) are materialized by a worker from a business event (`build_farm`, `buy_hens`). The only thing anyone ever writes is "this happened." Everything you query is derived.

## The design workflow

1. **Identify the business events first.** What *happens* in the domain? A farm is built, hens are bought, a hen moves coop, eggs are laid/stored/transferred/consumed. Name them as facts. These are the only writes.
2. **Model the nouns.** What does the application query — both entities (`hen`, `container`) and analytics ("how productive is this hen?", "what's in this container?"). Each is a domain model built from events.
3. **Create event meshes** (standard wiring, `adding-an-entity.md`). Create-only; front ends `POST` here.
4. **Create noun meshes.** Same wiring, but nothing writes them through the public API — workers write them via their repository.
5. **Build one worker per noun.** Each worker consumes the events it needs, folds them into its noun, and writes it. One worker owns one noun so nouns can be added, rebuilt, and scaled independently.

## The write-path rule (the crux)

**Front ends write business events. They never write domain models — not even the actor-nouns.**

- The only public write surface is the **event** restlettes (`POST /build_farm/api`, `POST /eggs_laid/api`, …).
- Noun meshes are read-only to the outside world. Their `/api` write routes are closed by **infrastructure** (network policy: only worker principals reach them) and **convention** (no application code `POST`s a noun). Workers write nouns through the repository, out of band.
- The application reads nouns via their **graphlettes** (`/hen/graph`, `/hen_productivity/graph`), never computing them on the read path.

A front end cannot put the system into an inconsistent state because it cannot express a noun write at all — only "this happened."

## The event → log bridge is storage-layer CDC, never application code

Workers consume events from a log (a merkql topic, a Kafka topic). Getting events *onto* that log is the one part people get wrong. **It is change-data-capture from the storage layer — not a hook in the write path.**

- The front end does exactly one write: `POST` the event, which commits to the event mesh's store. Done.
- The store's change feed (Mongo change streams, a WAL; the Debezium model — or merkql configured to tail the store) mirrors each *committed* write onto the topic. This is configured infrastructure. **No restlette is touched; there is no `post_create` publish.**

Why this and not an application-level publish after the write:

- **No dual write.** A "write the DB, then publish the event" hook is two writes with no shared transaction. Crash in between and the row exists but the event never fired — the log is now a lie, and every projection built from it is silently wrong. CDC derives the event *from the committed write*, so the event cannot be lost relative to the data: "if the event doesn't fire post-write, it will." At-least-once delivery after commit is the datastore/CDC layer's responsibility, not the mesh's.
- **Correct order and time.** Events carry the store's commit order and timestamp, so replay and temporal reads reflect what actually happened, not application wall-clock during a request.

Because folds are deterministic and idempotent, at-least-once delivery is enough: replaying a duplicate event recomputes the same noun. At-least-once + idempotent fold = exactly-once *effect*. The connector sits behind an `EventSource` trait so you **pick your scale** (invariant 6): `examples/egg-economy/src/source.rs` ships a portable poll-based tail (in-process, no infra) and a native change-stream connector is the distributed form — same trait, same delivery contract, different deployment weight. See `worker.rs` for the fold side.

## The worker

A worker is an independent process (not part of `meshql-server`; see the compose-your-own-binary model). It owns one projector and one noun repository, and its loop is:

1. **Consume** business events from the log, in commit order.
2. **Fold** them into domain state (`Projector::apply`).
3. **Write** the noun via its repository — a normal `create`, a new immutable Envelope version each time the noun advances.

Because a worker rebuilds its noun from the whole log, "catch up" and "replay from scratch" are the *same code path* (`examples/egg-economy/src/worker.rs`, `Worker::run_once`) — which is exactly what makes a noun safe to drop and rebuild. merksql is a ksqlDB-style engine over embedded event logs and is a natural home for the fold; `MerksqlSearcher::scan_latest` (`meshql-merksql/src/searcher.rs`) already subscribes from `Earliest` and folds a topic to a latest-per-id view under a `created_at <= cutoff` filter.

## Replay: new nouns from historical events

Because events are immutable and every read is temporal, **the event log is the source of truth and nouns are disposable.** This unlocks the payoff:

- **A new noun can be built at any time from history.** Realize months later you want `farm_output_by_breed`? Define the mesh, write a worker, point it at the existing event log from offset zero. The noun materializes from events recorded long before it existed — no migration, no backfill, no lost data. `examples/egg-economy/tests/pipeline.rs` proves exactly this: a latecomer projection is built from events that predate it.
- **A noun can be rebuilt from scratch** by discarding it and replaying — the fix for a worker bug is "correct the fold and replay," not "hand-patch corrupted state." The same test drops all nouns and rebuilds them identically from the log.
- **Point-in-time nouns** fall out for free: fold the log with a `cutoff` and you have the domain model as it stood at any past instant.

This is only sound because nothing bypasses the event log — which is exactly why the write-path rule and the storage-layer CDC bridge are enforced by infrastructure, not left to discipline.

## Why meshql fits this uniquely well

- **Immutable, versioned Envelopes** = an append-only event log, for free, on every backend.
- **Temporal reads** = replay-to-a-cutoff and point-in-time projections, for free.
- **Per-entity meshes** = events and projections are independently deployable, swappable, and rebuildable.
- **CQRS-by-convention** (REST writes / GraphQL reads) = the event-in / projection-out split is already the grain of the framework.

## Anti-patterns specific to this pattern

- A front end (or any code outside a worker) writing to a noun mesh — including the actor-nouns. If you're tempted to `POST /hen/api`, model the `buy_hens` event instead.
- **Publishing events from the application after the write** (a restlette `post_create` hook, a "save then emit" in a handler). That is a dual write with no shared transaction — a crash between the two loses the event and silently corrupts every downstream noun. Events come from the storage/CDC layer, post-commit.
- Updating or deleting an event after the fact — correct with a compensating event instead.
- Computing a domain model on the GraphQL read path instead of materializing it in a worker.
- A noun with no worker, kept in sync by hand.
- Treating a noun as the source of truth — if the log and the noun disagree, the log wins and the noun gets rebuilt.
- A worker fold that isn't deterministic/idempotent (depends on wall-clock, random, or read-back state) — it breaks replay and makes at-least-once delivery unsafe.

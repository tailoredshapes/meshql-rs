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

## Run an actual event storm — don't skip straight to code

Steps 1–2 above are usually done badly when done silently in your head. **Event storming** (Alberto Brandolini's technique — sticky notes on a wall, or the written equivalent here) is a concrete process for doing them properly, and its categories map almost one-to-one onto meshql's own concepts. Produce this as a real, written artifact (a markdown doc is fine) *before* wiring anything — not a mental pass you skip straight through:

| Event storm artifact | meshql concept |
|---|---|
| **Domain Events** (past tense: "Article Published", "Hens Bought") | Event-mesh entities — the verbs. Each becomes a create-only restlette. |
| **Commands** (imperative: "Publish Article", "Buy Hens") | The REST payload shape of a `POST` to the corresponding event restlette. |
| **Actors / Roles** (who issues a command — an editor, a customer, a system) | Casbin roles (`meshql-casbin`, see the "Auth beyond NoAuth" entry in `SKILL.md`'s decision guide) — who's authorized to write which event. Don't default to `NoAuth` once you've named actors; wire the roles you just identified. |
| **Policies** ("whenever X happens, then Y should happen") | **Workers.** A policy *is* a worker's fold-and-react loop — this is the most direct mapping in the table. A multi-step policy (event → triggers a new command → new event) is a worker that, after folding, also issues a follow-up write via REST. |
| **Aggregates** (the consistency boundary around a command) | The event mesh's write-side boundary — what one event commits atomically, and what a restlette's validators should actually check. |
| **Read Models / Views** (what someone needs to see) | Domain-mesh projections — the nouns from step 2 above, one worker each. |
| **Hotspots** (open questions, conflicting opinions, "we're not sure yet") | Write them down explicitly in the design artifact and flag them for the human. Don't silently resolve a hotspot with a guess — that's exactly the kind of decision that should surface, not disappear into an implementation choice nobody reviewed. |

**Ground the storm in reality, not a blank page.** If an established product already exists in the same problem space, look at how it actually models the domain (its real entities, its real event/state lifecycle) before inventing your own from scratch — a green-field brainstorm reliably misses events and edge cases that someone else already discovered the hard way. Adapt the domain shape, not the code.

Once the storm is done, steps 3–5 above are close to mechanical: Domain Events → event meshes, Read Models → noun meshes, Policies → workers.

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
- **Correct order and time.** The bridge appends committed writes in commit order, carrying the store's timestamp, so replay and temporal reads reflect what actually happened rather than application wall-clock during a request. Note what the bridge's job *is*: appending. The authority on order is the topic itself — see "The queue is the ordering authority" below — so nothing here needs to sort, buffer, or reconcile before publishing.

Because folds are deterministic and idempotent, at-least-once delivery is enough: replaying a duplicate event recomputes the same noun. At-least-once + idempotent fold = exactly-once *effect*.

**Be precise about what "idempotent" means here, because it is easy to over-read and the over-reading is dangerous.** A fold is idempotent over a *whole log* — replay the log twice from an empty projection and you get the same noun. That is NOT the same as being idempotent *per event*, and most useful folds are not: an accumulator like `message_count += 1` recomputes correctly from zero and inflates if you hand it the same event twice against state that already includes it.

Under a worker that re-reads the log from offset zero every tick, the distinction never surfaces — every tick resets and re-folds, so per-event idempotence is never exercised. **The moment a worker consumes incrementally, it becomes live**, because at-least-once delivery means a redelivered batch lands on state that may already include it.

The fix is not to make every fold per-event idempotent (often impossible without storing seen-ids forever). It is to treat **the consumed offset and the fold state as one atomic artifact**: checkpoint them together, restore them together, and commit *after* the projection write. Then a redelivered batch always lands on the state that predates it, and the fold recomputes rather than accumulates. A worker with nowhere durable to keep fold state must also keep no offset — committing a position for in-memory state means restarting with an empty projector at a non-zero offset, which silently corrupts the noun rather than failing.

Note also that the checkpoint is generally **not** derivable from the published projection: a projector usually holds state it deliberately never publishes (a live-session set, a pending-request set, an explicitly-granted role). Rehydrating from the projection alone loses exactly that. The connector sits behind an `EventSource` trait so you **pick your scale** (invariant 6): `examples/egg-economy/src/source.rs` ships a portable poll-based tail (in-process, no infra) and a native change-stream connector is the distributed form — same trait, same delivery contract, different deployment weight. See `worker.rs` for the fold side.

**If merkql is itself the event meshlette's primary store, there is no separate bridge to build at all — don't go looking for one.** This paragraph's "CDC mirrors committed writes onto the topic" framing describes what's needed when the event mesh's primary store is something else (Mongo, Postgres — a real database that isn't itself a log, so a real bridge like Debezium or a poll-tail has to derive a log from it). merkql doesn't have this problem: it *is* a log natively (per "Why meshql fits this uniquely well," below — immutable, ordered, append-only). If a new event entity's `Repository`/`Searcher` is `meshql-merkql`'s `MerkqlRepository`/`MerkqlSearcher` directly, a restlette `POST` already *is* a commit to the topic — the write and the log append are the same operation, not two operations bridged by CDC. This is almost always the simpler, correct choice for a **new** event entity with no prior data to migrate.

The `meshql-changes` crate's `SearcherTail`/`ChangeHub`/`run_merkql_sink` machinery (used by the `merkql-worker-pipeline` reference) is a *different* tool for a *different* job: mirroring an **already-existing** entity's writes — already backed by Mongo/Postgres/whatever — onto a merkql topic, without migrating that entity off its current store. Reach for it only when you're layering event-sourcing onto an entity that already has a primary store you don't want to move; it is not a prerequisite for using merkql as an event mesh's storage, and its absence from a given dependency snapshot doesn't mean merkql itself is unavailable — check whether `meshql-merkql`'s direct `Repository`/`Searcher` resolves before concluding merkql can't be used.

## The queue is the ordering authority

**Ordering is provided implicitly. You do not have to build it.** Events arrive at N event meshlettes; the queue topics provide the ultimate ordering; workers receive those events in a defined order and are presented with a predictable domain. **Nothing needs to arrive pre-sorted — the append to the topic is what defines the order.**

This is the single most reliably reinvented part of the platform. Do not build machinery to reconstruct a total order before publishing. Two separate builds recently did exactly that:

- one derived a cursor from a storage engine's physical row id (`_id INTEGER PRIMARY KEY`) and pinned itself to SQLite to get it — see `references/storage-adapters.md`, "Don't reach underneath the abstraction";
- the other funnelled every entity through a **single-partition** topic sorted by `(created_at, table, _id)`, with a parking buffer to hold events that arrived out of that order.

Both solved a problem the queue had already solved, and each paid for it: the first with engine portability, the second with throughput plus a buffer that is now a piece of stateful infrastructure someone has to reason about on restart.

If you find yourself writing a comparator, a re-sequencer, or a hold-back buffer on the produce side, stop: the correct amount of ordering code above the queue is none.

## Ordering versus throughput versus durability is queue configuration — and it belongs to the domain

meshql deliberately does not choose between them for you. It **cannot**: it does not know what matters about your domain, and there is no defensible default.

- **If causal order matters**, configure the queue for it. **Partitioning by aggregate id** keeps causally-related events — a note and the likes on it; an asset and its lifecycle transitions — in one partition, and therefore ordered, without serialising the whole deployment through a single partition. That is usually the right answer, and it is a configuration decision, not a component you write.
- **If throughput or durability matter more**, configure for those instead — more partitions, different acks/replication, different retention — and accept the weaker cross-aggregate ordering that comes with it.

Whatever you choose, **write down in the architecture document what you prioritised and what you gave up.** It is a domain decision, so it belongs where the domain is described — next to the event storm artifact — not implied by a partition count in a deploy manifest that nobody reads as a design statement. A reader six months later must be able to tell the difference between "we chose per-aggregate order over throughput" and "nobody thought about it."

## A refused command is a domain object, not an error response

Events are events. If a user tries to put an aggregate into an illegal state, **then that is what they did**, and it is recorded in that event meshlette forever. You do not get to un-happen it, and a check that refuses the write at the door throws away the fact that someone tried.

It is the **worker's** responsibility to find those state errors and respond accordingly — by emitting a **rejection** into its own projection. A rejection carries:

- a link to the aggregate it concerns,
- the source event id, the actor, and the timestamp,
- what was attempted, and why the domain refused it.

Because it is an ordinary projection entity, it is queryable by clients through the ordinary graphlette — so a UI can show *"your assignment was refused because the asset is disposed"* instead of leaving the user with a write that appeared to succeed. Think of it as a sophisticated, client-accessible dead-letter queue rather than a log line.

It recurses cleanly: triaging a rejection is itself an event (`*_acknowledged`) folded into the same projection, so the queue gets worked without anything being mutated in place. **Correction is a new event, all the way down.**

### The consequence for admission checks

A gate in front of the event meshlettes refuses only what is refusable **without domain state**:

- **authorization** — may this caller write this kind of event at all (`CasbinAuth::authorize_action`, tokens);
- **schema validity** — does the payload match the entity's JSON Schema;
- **create-only** — an event mesh takes `POST` and nothing else.

Anything that needs the aggregate's *current state* — is this asset already disposed, is this channel archived, does this holder still exist — is the **worker's** job, and it produces a rejection object, not a 4xx. A `ValidatorFn` runs before the fold and has no view of the projection; if you find yourself wanting to read a projection from a validator, you have found the boundary.

Note that `teamchat` currently does stateful checks at admission: its `write_gate` (`teamchat-server/src/gate.rs`) reads channel affiliation and returns 403. That is a defensible choice for chat, where immediate feedback while someone is typing matters more than it does in an asset register, and where a refusal has little downstream meaning worth querying later. But it is a **deliberate divergence** from the pattern above, and it should be named as one in that product's architecture document rather than copied into the next build because it was the shape that happened to be lying around.

## The worker

A worker is an independent process (not part of `meshql-server`; see the compose-your-own-binary model). It owns one projector and one noun repository, and its loop is:

1. **Consume** business events from the log, in the order the log defines (see "The queue is the ordering authority").
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
- **A worker that commits a consumed offset without durably checkpointing its fold state alongside it.** The two are one decision (see above). Committing the offset alone means a restart resumes at a position with an empty projector, which silently produces a wrong noun rather than an obvious failure.
- **Machinery to reconstruct a total order before publishing** — a re-sequencer, a parking/hold-back buffer, a single-partition funnel, a physical-row-id cursor. The topic append defines the order; this is code written against a problem that was already solved.
- **A partition/retention/acks choice made silently.** Ordering vs throughput vs durability is a domain decision — record in the architecture document what you prioritised and what you gave up.
- **A stateful domain check performed at admission and reported as a 4xx**, discarding the fact that the user tried. Admission refuses only what's refusable without domain state; everything else is a rejection object in a projection, queryable by the client.

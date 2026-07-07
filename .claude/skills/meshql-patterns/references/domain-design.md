# Domain Design: Events, Projections, and Workers

This is the methodology meshql is built for. The immutable Envelope, temporal reads, and per-entity mesh aren't incidental features — they exist so that a system can be modeled as **events that happened** plus **domain models derived from those events**. If you internalize one pattern from this skill, make it this one.

## The taxonomy: three kinds of mesh

Every entity is one of three kinds. Classify it before you write its schema — the kind dictates its write path and lifecycle. `examples/egg-economy/` labels all 13 of its entities this way (see the `Actors` / `Events` / `Projections` sections of `src/main.rs`).

| Kind | What it is | Lifecycle | Example |
|---|---|---|---|
| **Actor** | A long-lived participant with identity | Created, occasionally corrected | `farm`, `hen`, `container`, `consumer` |
| **Event** | An immutable fact that happened at a time | **Create-only** — never updated or deleted | `lay_report`, `storage_deposit`, `consumption_report` |
| **Projection** (domain model) | A read model computed from events | Written *only* by a worker | `hen_productivity`, `container_inventory`, `farm_output` |

Actors and events are your inputs. Projections are the domain models your application actually queries.

## The design workflow

1. **Identify events first.** What *happens* in the domain? Eggs are laid, deposited, transferred, consumed. Events are the ground truth; everything else is derived. Name them in the past tense — they are facts, not commands.
2. **Model the domain (projections).** What questions does the application ask? "How productive is this hen?" "What's in this container right now?" Each answer is a projection.
3. **Create meshes for events and actors.** Standard entity wiring (see `adding-an-entity.md`). Event meshes are create-only by convention — no update/delete path is exercised.
4. **Create meshes for the projections.** Same wiring, but nothing writes to them through the public API.
5. **Build one worker per domain model.** A worker consumes the relevant event mesh(es), folds them into the domain entity, and writes the projection mesh. One worker owns one projection — keep them independent so they can be added, rebuilt, or scaled separately.

## The write-path rule (the crux)

**Front ends write events. They never write domain models.**

- Writes go to **event** restlettes (`POST /lay_report/api`) and **actor** restlettes.
- Projection meshes are read-only to the outside world. Their `/api` write routes are closed off by **infrastructure** (gateway/network policy: only worker service accounts can reach projection write endpoints) and by **convention** (nothing in application code POSTs to a projection).
- The application reads domain models via projection **graphlettes** (`/hen_productivity/graph`), never computes them on the read path.

This is CQRS taken to its conclusion: the write model (events) and the read model (projections) are physically different meshes, connected only through workers. A front end cannot put the system into an inconsistent state because it cannot express a domain-model write at all — only "this happened."

## The worker

A worker is an independent process (not part of `meshql-server`; see the compose-your-own-binary model). Its loop:

1. **Consume** events from the event mesh, in order, with temporal awareness.
2. **Fold** them into domain state (the projection payload).
3. **Write** the projection via its repository — a normal `create`, producing a new immutable Envelope version each time state advances.

The natural substrate is the **event log itself**. When events are stored on `meshql-merksql` / `meshql-ksql`, the event mesh is backed by a merkql broker topic; `MerksqlSearcher::scan_latest` already subscribes from `Earliest` and folds the log to a latest-per-id view under a `created_at <= cutoff` filter (`meshql-merksql/src/searcher.rs`). A worker is that same consume-and-fold loop, but writing a projection instead of answering a query. merksql is a ksqlDB-style engine over embedded event logs — it *is* the projection engine; you supply the fold.

For lower-volume domains a worker can also be a `post_create` side effect on the event restlette (`build_restlette_router_ext`, see `adding-an-entity.md` §5): each new event synchronously nudges the projection. Prefer a real consumer for anything that must survive restarts or replay.

## Replay: new domains from historical events

Because events are immutable and every read is temporal, **the event log is the source of truth and projections are disposable.** This unlocks the payoff:

- **A new domain model can be built at any time from history.** Realize months later you want `farm_output_by_breed`? Define the mesh, write a worker, point it at the existing `lay_report` log from offset zero. The projection materializes from events that were recorded long before the domain model existed — no migration, no backfill script, no lost data.
- **A projection can be rebuilt from scratch** by discarding it and replaying — the fix for a worker bug is "correct the fold and replay," not "hand-patch corrupted state."
- **Point-in-time projections** fall out for free: fold the log with a `cutoff` and you have the domain model as it stood at any past instant.

This is only sound because nothing bypasses the event log. The write-path rule above is what guarantees the log is complete — which is *why* it's enforced by infrastructure, not left to discipline.

## Why meshql fits this uniquely well

- **Immutable, versioned Envelopes** = an append-only event log, for free, on every backend.
- **Temporal reads** = replay-to-a-cutoff and point-in-time projections, for free.
- **Per-entity meshes** = events and projections are independently deployable, swappable, and rebuildable.
- **CQRS-by-convention** (REST writes / GraphQL reads) = the event-in / projection-out split is already the grain of the framework.

## Anti-patterns specific to this pattern

- A front end (or any code outside a worker) writing to a projection mesh.
- Updating or deleting an event after the fact — correct with a compensating event instead.
- Computing a domain model on the GraphQL read path instead of materializing it in a projection worker.
- A projection with no worker, kept in sync by hand.
- Treating a projection as the source of truth — if the log and the projection disagree, the log wins and the projection gets rebuilt.

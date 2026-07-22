---
name: merkql-architecture
description: What merkql actually is (an embedded Rust library, not a deployed service) and what that implies for cross-language meshql deployments. Use whenever choosing a storage/event-log backend, designing a CDC/worker pipeline, or reasoning about which pieces of a deployment can be Java/TS vs. must be Rust.
---

# merkql: architecture and language implications

**This corrects a recurring misconception** (has come up more than once): merkql is *not* a standalone, separately-deployed service that any language connects to over a network, the way Postgres or Kafka are. Verified directly against the source repo (`/tank/repos/tailoredshapes/merkql/`, both `README.md` and `PRODUCT.md`):

> An embedded event log **for Rust**... No JVM. No ZooKeeper. **No network**. Just a directory on disk.

> **Network protocol** — merkql is an embedded library, not a server. If you need remote producers/consumers, put an API layer in front of it.

It gives you Kafka's *programming model* (topics, partitions, consumer groups, offsets, merkle-tree tamper-evidence) without Kafka's *deployment model* — but the price is that it's a Rust library call (`Broker::open(...)`), not a wire protocol. There is no client for any other language, and none is planned as part of merkql itself — "if you need remote producers/consumers, put an API layer in front of it" is a deliberate design boundary, not a gap to be filled.

## What this means for a meshql deployment

Two distinct pieces touch merkql directly, and both inherit the Rust constraint. Everything downstream of them does not:

1. **The storage connector** (a meshlette's `Repository`/`Searcher` implementation, e.g. `meshql-rs`'s `meshql-merkql` crate) — writes envelopes into merkql. Must be Rust, because it's calling the embedded library in-process.
2. **A worker that consumes from merkql** (reads/polls a topic to process events, e.g. for a CDC → projection pipeline) — must *also* be Rust, same reason.

But a worker's *output* — writing the resulting projection update — goes over REST to that projection's own restlette (see the "single writer" invariant: workers never touch storage directly, only through a meshlette's REST surface). REST is a network protocol, language-agnostic. **So the worker process itself must be Rust (to read from merkql), but the meshlette it writes the result to can be Java, TypeScript, or Rust — merkql's language constraint does not propagate past the REST boundary.**

Concretely, for a farm-style event → projection pipeline backed by merkql:
- `lay_report` (the event) must live on a Rust meshlette, since something needs to read those events out of merkql.
- The worker reading `lay_report` events is a Rust process.
- `hen_productivity` (the projection the worker writes to) can be a Java, TS, or Rust meshlette — the worker just POSTs to whatever REST surface the manifest declares for it.
- Any *other* meshlette in the same deployment (`farm`, `coop`, `hen`) that doesn't use merkql has no language constraint at all — Mongo/Postgres/MySQL/SQLite all have real multi-language client libraries.

## merkql is optional, not a framework default

Nothing about meshql requires merkql. Debezium + Kafka is a fully valid, proven alternative for the same CDC/event-sourcing role (already used in the Java `legacy` example's anti-corruption-layer pipeline: MongoDB → Kafka → transformers → REST), with far broader language support — a Java or TS worker can consume from Kafka natively via existing client libraries, no Rust required anywhere in the pipeline. A deployment can even run both mechanisms for different parts of its domain simultaneously, though that's real added operational complexity, not a recommended default.

**Choose merkql when**: you want zero-infrastructure event sourcing (no Docker, no cluster, starts in microseconds), tamper-evident/auditable logs (merkle proofs), or a single-node/edge/embedded deployment — and you're fine with the connector + worker being Rust.

**Choose Kafka/Debezium (or another CDC mechanism) when**: you need workers in a non-Rust language, real distributed/multi-node infrastructure, or want to avoid a Rust-only piece in the pipeline entirely.

## Anti-patterns to flag

- Describing merkql as "standalone," "a queue service," "deployed separately," or implying any language can connect to it over a network — it cannot, by design.
- Assuming a merkql-backed worker or connector could be written in Java/TS — it cannot; only the REST-facing side of a pipeline touching merkql is language-agnostic.
- Assuming merkql is required for event sourcing in meshql generally — it's one of several valid backend choices, and the *pattern* (events → CDC → worker → REST write to a projection) works identically regardless of which backend implements the "CDC" step.

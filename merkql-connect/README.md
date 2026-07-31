# merkql-connect

Change-data-capture from a meshql envelope store onto a merkql topic.

```
merkql-connect /etc/merkql-connect/lay_report.toml
```

One binary, one config file, one topic. It is modelled on Debezium: it snapshots
what already exists, then streams committed writes, carrying each one in a
`before`/`after`/`source`/`op`/`ts_ms` envelope that names the store's own
position.

It supports **SQLite, MongoDB and PostgreSQL**, and it is a **separate process**
— not a library, not part of `meshql-server`, and not tied to Rust on the write
side. A Java (`meshql`) or TypeScript (`meshobj`) deployment writing to Postgres
or Mongo runs this connector exactly as a Rust one does.

---

## The topology

```
  meshql service (restlettes)  ──writes──▶  Postgres / Mongo / SQLite
   (Rust, Java or TypeScript)                        │
                                                     │  CDC — this process
                                                     ▼
                                              merkql-connect
                                                     │
                                            sole writer │ append
                                                     ▼
                                            merkql topic (1 partition)
                                                     │
                                       many reader processes
                                                     ▼
                                  workers ──POST──▶ projection restlettes
```

The meshql service writes **only to the database**. The connector is the only
process that appends to merkql. Workers are readers, and merkql supports readers
without limit.

### Why the separate process is the point

merkql is **single-writer per process**. `Partition::next_offset` is an in-memory
counter advanced only by in-process appends, so two writer processes each believe
they own the next offset and silently overwrite each other's records. Nothing in
merkql detects this. The log simply ends up missing records, which downstream is
indistinguishable from "those writes never happened."

Deploying the CDC as its own process **resolves** that constraint instead of
fighting it. One writer, many readers — exactly the topology merkql is built for,
satisfied by construction rather than by discipline. Four separate builds had run
into the constraint before this crate existed; each had solved it with a comment.

`TopicWriter` makes it structural as well: claiming a topic takes an exclusive
`flock` on a lock file beside the offset store, held for the writer's lifetime.
A second connector aimed at the same topic **fails at startup** rather than
quietly corrupting the log. The lock is per open file description, so it catches
a second process as well as a second writer inside one process.

The same shape is why the connector is language-independent. It talks to the
database and to merkql, and to nothing else — no restlette dependency, no shared
process, no in-language API. What wrote the row is irrelevant to it.

### Why CDC and not a `post_create` hook

- **No dual write.** The application writes exactly once, to the store. The event
  is *derived* from the committed write, so there is no window in which the row
  exists but the event was lost. A restlette `post_create` hook is a dual write
  with no shared transaction: crash between the commit and the publish and the
  event never fires, the log is a lie, and every projection folded from it is
  silently wrong.
- **Correct order and time.** Records carry the store's commit order and the
  store's timestamp, not the application's wall-clock during a request, so replay
  and temporal reads agree with what actually happened.

### When you do *not* need this

If the event meshlette's `Repository` is already `meshql-merkql`'s
`MerkqlRepository` (or `meshql-merk` over merk-cloud), the restlette `POST` **is**
the topic append. There is nothing to bridge, and running a connector would make
a second writer. See "Two topologies" in the `meshql-patterns` skill.

---

## Quick start — SQLite, no Docker

The smallest working deployment is two processes and no infrastructure.

```toml
# /etc/merkql-connect/lay_report.toml
topic      = "lay_report"
merkql_dir = "/var/lib/merkql"
state_dir  = "/var/lib/merkql-connect"

[source]
type   = "sqlite"
path   = "/var/lib/meshql/lay_report.db"
entity = "lay_report"
```

```sh
# 1. the meshql service, writing envelopes into lay_report.db
./my-meshql-service &

# 2. the connector, watching that file
merkql-connect /etc/merkql-connect/lay_report.toml &

# 3. workers, reading /var/lib/merkql
./hen-productivity-worker &
```

The connector snapshots the rows already in the table (`op: r`), then streams
every subsequent commit (`op: c`).

**The SQLite database must be a real file in WAL mode**, and the connector needs
read access to its *directory*, not only the file — see "SQLite" below.

---

## Configuration reference

The config is TOML. One file per connector; one connector per topic.

### Top level

| Key | Required | Default | Meaning |
|---|---|---|---|
| `topic` | yes | — | The merkql topic to replicate onto. One topic per event meshlette. Created with **one partition** if absent; an existing topic with more is refused. |
| `merkql_dir` | yes | — | The merkql store's data directory (`BrokerConfig`). |
| `state_dir` | yes | — | Where the connector keeps `<topic>.offsets.json` and `<topic>.writer.lock`. |
| `snapshot_mode` | no | `initial` | `initial`, `never` or `when_needed`. See below. |
| `offset_commit_interval_ms` | no | `1000` | How often a position is written to disk. Larger means fewer fsyncs and a longer replay after a crash. |
| `heartbeat_interval_ms` | no | `10000` | **PostgreSQL only.** How often an idle connector advances its replication slot. See the WAL hazard below. |

### `[source]`

`type = "sqlite"`

| Key | Required | Default | Meaning |
|---|---|---|---|
| `path` | yes | — | Path to the database file. Must exist; an in-memory database cannot be watched. |
| `table` | no | `envelopes` | The physical envelope table. `meshql-sqlite` hardcodes `envelopes`. |
| `entity` | yes | — | The logical mesh name reported in `source.entity`. |

`type = "mongo"`

| Key | Required | Meaning |
|---|---|---|
| `uri` | yes | Connection string. Must reach a replica set or sharded cluster. |
| `database` | yes | Database name. |
| `collection` | yes | The envelope collection. |
| `entity` | yes | The logical mesh name reported in `source.entity`. |

`type = "postgres"`

| Key | Required | Default | Meaning |
|---|---|---|---|
| `conn` | yes | — | A libpq connection string. The role needs rights to create a publication and a replication slot. |
| `table` | no | `envelopes` | The physical envelope table. |
| `entity` | yes | — | The logical mesh name reported in `source.entity`. |
| `slot` | yes | — | Replication slot name. One slot per connector. **Dropping the connector means dropping the slot.** |
| `publication` | yes | — | Publication covering `table`. Created if absent. |

Table, slot and publication names are validated as plain identifiers (letters,
digits, underscore, not starting with a digit) because they are interpolated into
SQL. Anything else is refused before it reaches the database.

### Snapshot modes

The mode decides two things: whether a cold start replays history, and what
happens when a **stored position becomes unusable** — a dropped replication slot,
an expired Mongo resume token, a rebuilt or restored database.

| Mode | Cold start | Unusable stored position |
|---|---|---|
| `initial` (default) | Snapshot, then stream | **Refuse to start.** The operator asked for one snapshot; taking a second silently would republish all history with nobody deciding to. |
| `never` | Start at the live tail; history is not captured | **Refuse to start.** |
| `when_needed` | Snapshot, then stream | Re-snapshot and carry on. |

An unusable position is **never** a silent restart from somewhere else. That is
the failure this whole crate exists to prevent: starting from the live tail after
losing a position skips everything in between, and a hole in an event log is
undetectable and unrepairable downstream.

Choose `when_needed` for an unattended deployment that must keep running, and
accept that a transient failure may re-emit history. Folds are idempotent over a
whole log, so this is safe — but it is not free, and duplicates hit every
downstream worker. Choose `initial` when you would rather page someone.

---

## Worked examples

### SQLite

`meshql-sqlite` uses one database file per repository with a table called
`envelopes`, so a deployment runs one connector per event meshlette.

```toml
topic      = "lay_report"
merkql_dir = "/var/lib/merkql"
state_dir  = "/var/lib/merkql-connect"
snapshot_mode = "when_needed"

[source]
type   = "sqlite"
path   = "/var/lib/meshql/lay_report.db"
table  = "envelopes"
entity = "lay_report"
```

Requirements:

- The database is a **file**, opened in **WAL** mode by the writing service.
- The connector process can read the file **and list its parent directory** — the
  watch is on the directory, because SQLite deletes and recreates `-wal`/`-shm`
  at checkpoint boundaries and a watch on a deleted inode goes quiet forever.
- The filesystem supports inotify. See the hazard below.

### MongoDB

```toml
topic      = "lay_report"
merkql_dir = "/var/lib/merkql"
state_dir  = "/var/lib/merkql-connect"

[source]
type       = "mongo"
uri        = "mongodb://mongo-0:27017/?replicaSet=rs"
database   = "farm"
collection = "lay_reports"
entity     = "lay_report"
```

Requirements:

- **A replica set or a sharded cluster.** Change streams do not exist on a
  standalone `mongod`. A single-node replica set is enough.
- An oplog long enough to cover the connector's worst expected downtime. When it
  is not, the resume token ages out (`ChangeStreamHistoryLost`) and the snapshot
  mode decides what happens next.

The connector stores the server's resume token verbatim as its position. It never
invents one of its own.

### PostgreSQL

```toml
topic      = "lay_report"
merkql_dir = "/var/lib/merkql"
state_dir  = "/var/lib/merkql-connect"
heartbeat_interval_ms = 10000

[source]
type        = "postgres"
conn        = "host=db user=cdc password=… dbname=farm"
table       = "envelopes"
entity      = "lay_report"
slot        = "merkql_lay_report"
publication = "merkql_lay_report_pub"
```

Requirements:

- **`wal_level = logical`.** It is not runtime-settable; set it in
  `postgresql.conf` and restart. The connector checks at startup and refuses to
  run without it, rather than failing later with an opaque error.
- A role permitted to create a replication slot and a publication.

On first start the connector creates, idempotently:

- the publication, covering `table`;
- a logical replication slot using `pgoutput`;
- a statement-level `AFTER INSERT` trigger on `table` that calls
  `pg_notify('merkql_connect_<entity>', '')`.

The `NOTIFY` is only a wake-up edge and carries **no data**. The slot is the
source of truth: it retains WAL until the connector confirms records durable, so
a dropped notification costs latency and nothing else. This is why the usual
"`LISTEN`/`NOTIFY` is unsafe for CDC" objection does not apply here — no change
ever travels over the notification.

Records are read with `pg_logical_slot_peek_binary_changes`, which does **not**
consume. The slot is advanced separately, only after an offset commit has reached
disk, so a crash anywhere before that replays a batch instead of losing it.

A record's position is the transaction's **commit end LSN**, so every record in
one transaction shares one position. Logical decoding replays whole transactions;
a position inside one would replay it on every restart.

---

## Two operational hazards

These are the two ways a healthy-looking deployment causes an outage. Read both.

### 1. A PostgreSQL replication slot pins WAL — and the *idle* deployment is the dangerous one

PostgreSQL cannot recycle any WAL segment at or after a slot's
`confirmed_flush_lsn`. A slot that stops advancing grows the WAL directory
without bound until the disk fills and **the whole cluster stops** — not just the
watched table, the cluster.

The counterintuitive part: this bites hardest when **the watched table is quiet**.
The slot only advances when this connector advances it, and it only has cause to
advance when it sees changes it cares about. Meanwhile every other table in the
database keeps generating WAL that the slot is pinning. A low-traffic event mesh
in a busy database is the worst case, not the best one.

That is what `heartbeat_interval_ms` is for. On each tick with nothing to deliver,
the connector captures `pg_current_wal_lsn()`, peeks up to it, and — only if that
came back empty — advances the slot to it, clamped to what it has actually
reported durable. Capturing the target *before* peeking is load-bearing: reversed,
a write landing between an empty peek and the target read would be advanced past
and lost.

Consequences to plan for:

- **The heartbeat only runs while the connector is running.** A *stopped*
  connector still pins WAL, and pins it at whatever the slot last confirmed. A
  connector down over a weekend can take the database with it.
- **Retiring a connector means dropping its slot.** Deleting the config file, the
  unit, or the container does nothing. Run:
  ```sql
  SELECT pg_drop_replication_slot('merkql_lay_report');
  ```
  Dropping the slot discards everything it was holding, so only do it when the
  connector is genuinely retired — restarting one whose slot was dropped is an
  unusable position, handled per `snapshot_mode`.
- **Alert on the backlog**, do not wait to notice:
  ```sql
  SELECT slot_name,
         pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)) AS retained
    FROM pg_replication_slots;
  ```
  The connector shouts on stderr at startup when its own slot is holding more
  than 512 MiB, which is exactly when an operator can act — but startup is the
  only time it looks, so an external alert is not optional.
- Set `heartbeat_interval_ms` to something the WAL volume can tolerate. The
  default of 10 s is a floor on how long WAL stays pinned during idleness, not a
  poll interval; a notification still wakes the connector immediately.

### 2. SQLite's inotify edge is not available on every filesystem

The SQLite source has no timer. It watches the database's directory with inotify
and reads through an ordinary connection when woken. If the platform cannot give
it real filesystem notifications — an unsupported filesystem, an exhausted
inotify watch limit, a build where `notify` resolves to its polling watcher — the
connector returns `NoFeed` and **refuses to start**.

There is deliberately no fallback to a timer. A connector that quietly degrades
to polling is a connector whose latency and load characteristics have silently
stopped matching what was deployed, and nothing reports it.

In practice this means: **do not put the SQLite database on a network filesystem.**
NFS, SMB, some FUSE mounts and some container overlay configurations either do not
deliver inotify events for remote writes or do not deliver them at all. Local
disk, and a writer in the same kernel namespace as the connector.

The same loudness applies to a missing database file: `merkql-connect` will not
start against a path that does not exist, because a connector watching nothing
reports healthy forever.

---

## Operating

### State directory

Per topic, `state_dir` holds:

- `<topic>.offsets.json` — the last native position whose record was appended.
  Written atomically (temp file, fsync, rename, fsync the directory).
- `<topic>.writer.lock` — the exclusive `flock` proving this is merkql's only
  writer. Released when the process exits.

Both must be on durable local storage that survives a restart. Losing the offset
file is a cold start, which under `initial` or `when_needed` replays the entire
history onto the topic.

The offset file records which connector and entity wrote it. Pointing a Mongo
connector at a SQLite connector's offset file is refused, not silently adopted.

### One connector, one topic, one partition

`create_topic` returns early on an existing topic **without checking its partition
count**, so the connector checks explicitly and refuses a topic with more than one
partition. This is not a limitation to work around: merkql routes by hashing the
producer key, the key is the Envelope id, and Envelope ids are unique per record.
Raising the partition count scatters one aggregate's records across partitions
with no ordering between them — strictly worse than one partition's total order.

### Restarts and duplicates

The loop's rule is **append, then commit the position**, with positions committed
periodically. A crash between the two replays records; it never skips them. The
contract is at-least-once *after commit*, and every design decision in the crate
resolves in favour of re-delivering rather than skipping.

Consumers must therefore tolerate duplicates. Folding a whole log is idempotent,
which is what makes this safe — see `domain-design.md` in the `meshql-patterns`
skill for the difference between "idempotent over a log" and "idempotent per
event", which matters for any worker that consumes incrementally.

An interrupted snapshot resumes as a cold start rather than as a stream. A
position committed mid-snapshot names a row *inside* the snapshot, and resuming a
live stream there would begin it at a point the stream never reached.

### Retiring or moving a connector

1. Stop the process.
2. **PostgreSQL only:** drop the replication slot, or the database keeps paying
   for it. Optionally drop the publication and the `<slot>_notify_trg` trigger.
3. Remove the state directory's files for that topic if the topic is also going.

Moving a connector to another host means moving `state_dir` with it. Starting
fresh elsewhere is a cold start.

---

## What lands on the topic

Debezium's change-event envelope, serialized as JSON, keyed by the **Envelope id**
— the same key `meshql-merkql` uses, so a CDC-fed topic and a
merkql-as-primary-store topic partition identically.

```json
{
  "op": "c",
  "ts_ms": 1751892345456,
  "after": {
    "id": "hen-1",
    "payload": { "eggs": 3 },
    "created_at": "2025-07-07T12:05:45.123Z",
    "deleted": false,
    "authorized_tokens": ["farm"]
  },
  "source": {
    "connector": "sqlite",
    "entity": "lay_report",
    "ts_ms": 1751892345123,
    "position": "42",
    "snapshot": "false"
  }
}
```

| Field | Meaning |
|---|---|
| `op` | `r` snapshot read, `c` create. `u`/`d` exist in the type because the wire shape is Debezium's; these sources never emit them — meshql envelopes are append-only, and a *deletion* arrives as a `c` carrying `deleted: true`. |
| `ts_ms` | When the **connector** emitted the record. |
| `before` | Always absent. Envelopes are immutable, so there is no prior image. |
| `after` | The committed envelope. |
| `source.connector` | `sqlite`, `mongodb` or `postgresql`. |
| `source.ts_ms` | When the **store** committed the write. Domain time; use this, not the top-level `ts_ms`. |
| `source.position` | The store's native cursor: a SQLite rowid, a Mongo resume token, a Postgres LSN. Carried verbatim so a consumer can dedupe against the store's numbering rather than trusting ours. |
| `source.snapshot` | `false`, `true`, or `last`. Three-valued, not a boolean: `last` marks the final snapshot record, and it is the first snapshot position that is safe to resume a live stream from. |

A consumer reads `op`, `after` and `source` and does not care which backend
produced the record. That interchangeability is enforced by a shared
certification suite (`src/cert.rs`) which every backend's integration test drives,
rather than each writing its own approximation of the contract.

---

## Building

All three backends are on by default, because a standalone connector binary is
normally built once and pointed at whichever store a deployment runs.

```sh
cargo build --release -p merkql-connect
```

To build a smaller binary for one backend:

```sh
cargo build --release -p merkql-connect --no-default-features --features sqlite
```

A config naming a source the binary was not built with fails at startup with a
message telling you which feature to rebuild with.

---

## Tests

58 tests. Each backend is proven end to end — a real restlette `POST`, a real
database commit, a real connector feed, a real merkql broker, a real consumer, a
real worker fold, and a `GET` proving the projection landed.

```sh
cargo test -p merkql-connect                       # unit tests
cargo test -p merkql-connect --test sqlite_pipeline # no infrastructure needed
cargo test -p merkql-connect --test mongo_pipeline  # testcontainers: mongod --replSet
cargo test -p merkql-connect --test postgres_pipeline # testcontainers: wal_level=logical
```

The Mongo and Postgres pipelines need Docker. The SQLite pipeline does not.

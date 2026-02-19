# MeshQL-RS Performance Report

**Date**: 2026-02-19
**Tool**: k6 v1.5.0 | **Host**: Linux 6.18.2 | **Rust**: 1.85

## Setup

| Component | Configuration |
|-----------|---------------|
| **Server** | meshql-rs MongoDB perf_server (13 entities, 19 internal resolvers) |
| **MongoDB** | 3 replica sets (actors, events, projections) — mirrors Java egg-economy |
| **Entities** | 5 actors, 5 events, 3 projections — same schema as Java |

The Rust perf server uses the same shard distribution as the Java egg-economy:
- **Shard 1** (actors): farm, coop, hen, container, consumer
- **Shard 2** (events): lay_report, storage_deposit, storage_withdrawal, container_transfer, consumption_report
- **Shard 3** (projections): container_inventory, hen_productivity, farm_output

---

## Validation Gate

Before any performance numbers are collected, `tests/validate.js` runs 101 correctness checks:
- REST CRUD round-trips (POST/GET/PUT/DELETE)
- GraphQL getAll returns expected counts with non-null fields
- GraphQL getById returns matching entity
- Filtered queries (getByFarm, getByCoop) return correct FK matches
- Federation depth 2 and 3 return populated nested arrays
- Upward resolvers (Hen→Coop) populated
- Cross-entity resolvers (Consumer→ConsumptionReports, Container→Inventory)

If any check fails, `run-all.sh` aborts before performance tests run.

---

## Baseline Latency (Smoke: 1 VU, 10s)

### HTTP Request Duration (ms) — Rust vs Java (MongoDB, 3-shard RS)

| Test | Metric | Java | Rust | Speedup |
|------|--------|-----:|-----:|--------:|
| **REST CRUD** | avg | 4.82 | 0.75 | 6.4x |
| | med | 4.09 | 0.68 | 6.0x |
| | p95 | 8.75 | 1.56 | 5.6x |
| **GraphQL** | avg | 3.34 | 0.85 | 3.9x |
| | med | 2.90 | 0.94 | 3.1x |
| | p95 | 6.61 | 1.23 | 5.4x |
| **Federation** | avg | 2.42 | 0.87 | 2.8x |
| | med | 1.97 | 0.94 | 2.1x |
| | p95 | 4.20 | 1.21 | 3.5x |
| **Mixed** | avg | 2.89 | 0.82 | 3.5x |
| | med | 2.03 | 0.81 | 2.5x |
| | p95 | 5.59 | 1.35 | 4.1x |

**Error rate**: 0% across all tests.

### GraphQL Query Latency (ms)

| Query Type | Metric | Java | Rust | Speedup |
|------------|--------|-----:|-----:|--------:|
| **getById** | avg | 2.53 | 1.14 | 2.2x |
| **getAll** | avg | 4.39 | 1.40 | 3.1x |
| **filtered** | avg | 3.02 | 0.88 | 3.4x |

### Federation Depth Scaling (ms)

| Depth | Metric | Java | Rust | Speedup |
|-------|--------|-----:|-----:|--------:|
| **Depth 2** (farm+coops) | avg | 4.58 | 2.29 | 2.0x |
| **Depth 3** (farm+coops+hens) | avg | 13.37 | 3.85 | 3.5x |
| **Depth 3+parallel** | avg | 16.16 | 4.45 | 3.6x |

---

## Load Test (10 VUs, 55s ramp)

### HTTP Request Duration (ms) — Rust vs Java

| Test | Metric | Java | Rust | Speedup |
|------|--------|-----:|-----:|--------:|
| **REST CRUD** | avg | 1.84 | 1.38 | 1.3x |
| | med | 1.53 | 0.88 | 1.7x |
| | p95 | 4.64 | 4.58 | 1.0x |
| **GraphQL** | avg | 1.76 | 0.69 | 2.6x |
| | med | 1.20 | 0.67 | 1.8x |
| | p95 | 3.16 | 1.11 | 2.8x |
| **Federation** | avg | 3.09 | 1.96 | 1.6x |
| | med | 3.09 | 2.08 | 1.5x |
| | p95 | 4.58 | 3.04 | 1.5x |
| **Mixed** | avg | 10.79 | 1.16 | 9.3x |
| | med | 2.01 | 0.81 | 2.5x |
| | p95 | 26.30 | 2.90 | 9.1x |

### GraphQL Query Latency Under Load (ms)

| Query Type | Metric | Java | Rust | Speedup |
|------------|--------|-----:|-----:|--------:|
| **getById** | avg | 2.38 | 0.88 | 2.7x |
| | p95 | 6.00 | 2.00 | 3.0x |
| **getAll** | avg | 12.61 | 0.69 | 18.4x |
| | p95 | 27.00 | 1.00 | 27.0x |
| **filtered** | avg | 2.66 | 0.67 | 4.0x |
| | p95 | 4.00 | 1.00 | 4.0x |

### Federation Depth Under Load (ms)

| Depth | Metric | Java | Rust | Speedup |
|-------|--------|-----:|-----:|--------:|
| **Depth 2** | avg | 2.31 | 1.86 | 1.2x |
| | p95 | 3.00 | 3.00 | 1.0x |
| **Depth 3** | avg | 4.10 | 2.46 | 1.7x |
| | p95 | 4.00 | 3.00 | 1.3x |
| **Depth 4** | avg | 3.99 | 2.70 | 1.5x |
| | p95 | 5.00 | 4.00 | 1.2x |

---

## Key Findings

### 1. Smoke (1 VU): Rust 2-6x faster — framework overhead dominates

At baseline with no contention, Rust's advantage is clearest in simple operations (REST CRUD 6.4x, GraphQL p95 5.4x). The gap narrows for federation (2-3x) as MongoDB round-trips become the bottleneck.

### 2. Load (10 VU): Rust 1.3-2.6x faster — MongoDB becomes the bottleneck

Under concurrency, REST CRUD converges to 1.3x as both implementations spend most time waiting on MongoDB. GraphQL queries hold at 2.6x. Federation queries converge to 1.2-1.7x.

### 3. Java has a fat tail under load — Rust does not

The most striking load result is mixed workload: Java avg=10.79ms but med=2.01ms (5.4x ratio), indicating GC pauses or connection contention in the tail. Rust avg=1.16ms, med=0.81ms (1.4x ratio) — much tighter distribution.

### 4. getAll serialization is a Java bottleneck

Under load, Java's `getAll` averages 12.61ms vs Rust's 0.69ms (18.4x). This grows with accumulated data — Java's GraphQL-Java + Jackson serialization overhead scales worse than Rust's async-graphql + serde.

### 5. Federation depth scaling is more efficient in Rust

Java's federation cost per resolver level is ~2ms under load. Rust's is ~0.5-1ms. At depth 3+, this compounds: Java 4ms vs Rust 2.5ms.

### 6. Infrastructure matters more than language for absolute numbers

Switching from bare single-node MongoDB to 3-shard replica set roughly doubled Rust latencies (REST avg: 0.31ms → 0.75ms). The MongoDB protocol overhead (write concerns, topology) dominates at sub-millisecond operation times.

---

## Notes

- Both Java and Rust load tests ran on 2026-02-19 against identical 3-shard MongoDB replica sets.
- Java app ran inside Docker compose. Rust server ran on bare metal connecting to Docker MongoDB via localhost. This gives Rust a slight advantage on network latency.
- Databases were cleaned between each test run to prevent data accumulation from skewing results.
- Both use NoAuth, no JSON schema validation on REST.

---

## Reproducing

```bash
# Start 3-shard MongoDB
docker compose -f performance/docker-compose.yml up -d

# Wait for replica sets to be healthy
docker compose -f performance/docker-compose.yml ps

# Build and run perf server
cargo run -p meshql-mongo --features perf --bin perf_server --release

# Run full suite (validation gate + performance)
./performance/run-all.sh http://localhost:5088 smoke

# Load test
./performance/run-all.sh http://localhost:5088 load
```

Raw k6 JSON summaries are in `performance/results/`.

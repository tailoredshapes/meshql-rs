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

## Key Findings

### 1. Rust is 2-6x faster than Java with identical MongoDB infrastructure

Simple operations (REST CRUD, GraphQL queries) see the largest gains (4-6x) because framework overhead dominates. Federation queries converge toward 2-3x as MongoDB round-trips become the bottleneck.

### 2. Federation depth scaling is more efficient in Rust

Java's federation cost per resolver level is ~5-6ms. Rust's is ~1-2ms. At depth 3, this compounds: Java pays ~13ms vs Rust's ~4ms for the same traversal.

### 3. Infrastructure matters more than language for absolute numbers

Switching from a bare single-node MongoDB to a 3-shard replica set roughly doubled Rust latencies (REST avg: 0.31ms → 0.75ms). The MongoDB protocol overhead (write concerns, topology) dominates at sub-millisecond operation times.

---

## Notes

- Java results from `meshql/performance/results/mongo-*.json` (Feb 15, 2026). Java app ran inside Docker compose.
- Rust server ran on bare metal connecting to Docker MongoDB via localhost. This gives Rust a slight advantage on network latency vs the Java app running inside the compose network.
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

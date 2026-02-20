import http from "k6/http";
import { check, sleep, group } from "k6";
import { Trend, Counter } from "k6/metrics";

const API_URL = __ENV.API_URL || "https://uo8scixwwd.execute-api.us-east-1.amazonaws.com";

// Custom metrics
const coldStartDuration = new Trend("cold_start_duration", true);
const restCreateDuration = new Trend("rest_create_duration", true);
const restReadDuration = new Trend("rest_read_duration", true);
const graphqlQueryDuration = new Trend("graphql_query_duration", true);
const federatedQueryDuration = new Trend("federated_query_duration", true);

export const options = {
  scenarios: {
    // Scenario 1: Warm CRUD — sequential REST create/read cycle
    warm_crud: {
      executor: "constant-vus",
      vus: 1,
      duration: "30s",
      exec: "warmCrud",
      tags: { scenario: "warm_crud" },
    },
    // Scenario 2: Warm GraphQL — federated queries
    warm_graphql: {
      executor: "constant-vus",
      vus: 1,
      duration: "30s",
      startTime: "35s",
      exec: "warmGraphql",
      tags: { scenario: "warm_graphql" },
    },
    // Scenario 3: Concurrent load — 10 VUs sustained
    concurrent_load: {
      executor: "constant-vus",
      vus: 10,
      duration: "60s",
      startTime: "70s",
      exec: "concurrentLoad",
      tags: { scenario: "concurrent_load" },
    },
    // Scenario 4: Write contention — concurrent writes to same entity type
    write_contention: {
      executor: "constant-vus",
      vus: 5,
      duration: "30s",
      startTime: "135s",
      exec: "writeContention",
      tags: { scenario: "write_contention" },
    },
  },
  thresholds: {
    "rest_create_duration{scenario:warm_crud}": ["p(95)<500"],
    "graphql_query_duration{scenario:warm_graphql}": ["p(95)<500"],
  },
};

const headers = { "Content-Type": "application/json" };

// --- Scenario functions ---

export function warmCrud() {
  // Create a farm
  const createRes = http.post(
    `${API_URL}/farm/api`,
    JSON.stringify({ name: `Perf Farm ${Date.now()}` }),
    { headers }
  );
  restCreateDuration.add(createRes.timings.duration);
  check(createRes, { "farm created (201)": (r) => r.status === 201 });

  // Query it back via GraphQL
  const queryRes = http.post(
    `${API_URL}/farm/graph`,
    JSON.stringify({ query: "{ getAll { id name } }" }),
    { headers }
  );
  graphqlQueryDuration.add(queryRes.timings.duration);
  check(queryRes, {
    "graphql 200": (r) => r.status === 200,
    "has data": (r) => JSON.parse(r.body).data !== null,
  });

  sleep(0.1);
}

export function warmGraphql() {
  // Federated query: farm → coops → hens
  const res = http.post(
    `${API_URL}/farm/graph`,
    JSON.stringify({
      query: "{ getAll { id name coops { id name hens { id name } } } }",
    }),
    { headers }
  );
  federatedQueryDuration.add(res.timings.duration);
  check(res, {
    "federated 200": (r) => r.status === 200,
    "no errors": (r) => JSON.parse(r.body).errors === null,
  });

  sleep(0.1);
}

export function concurrentLoad() {
  // Mix of writes and reads
  const coin = Math.random();

  if (coin < 0.3) {
    // 30% writes
    const res = http.post(
      `${API_URL}/farm/api`,
      JSON.stringify({ name: `Load Farm ${__VU}-${__ITER}` }),
      { headers }
    );
    restCreateDuration.add(res.timings.duration);
    check(res, { "concurrent create 201": (r) => r.status === 201 });
  } else if (coin < 0.7) {
    // 40% simple GraphQL
    const res = http.post(
      `${API_URL}/farm/graph`,
      JSON.stringify({ query: "{ getAll { id name } }" }),
      { headers }
    );
    graphqlQueryDuration.add(res.timings.duration);
    check(res, { "concurrent query 200": (r) => r.status === 200 });
  } else {
    // 30% federated GraphQL
    const res = http.post(
      `${API_URL}/farm/graph`,
      JSON.stringify({
        query: "{ getAll { id name coops { id name } } }",
      }),
      { headers }
    );
    federatedQueryDuration.add(res.timings.duration);
    check(res, { "concurrent federated 200": (r) => r.status === 200 });
  }

  sleep(0.05);
}

export function writeContention() {
  // All VUs write to the same entity type simultaneously
  const res = http.post(
    `${API_URL}/hen/api`,
    JSON.stringify({
      name: `Hen ${__VU}-${__ITER}`,
      coop_id: "00000000-0000-0000-0000-000000000000",
      breed: "Rhode Island Red",
    }),
    { headers }
  );
  restCreateDuration.add(res.timings.duration);
  check(res, { "contention create 201": (r) => r.status === 201 });

  sleep(0.05);
}

// --- Cold start test (run separately) ---
// Usage: k6 run --iterations 1 -e SCENARIO=cold_start k6-test.js
export function coldStart() {
  const start = Date.now();
  const res = http.post(
    `${API_URL}/farm/graph`,
    JSON.stringify({ query: "{ getAll { id name } }" }),
    { headers }
  );
  const elapsed = Date.now() - start;
  coldStartDuration.add(res.timings.duration);
  console.log(`Cold start total: ${elapsed}ms, HTTP duration: ${res.timings.duration}ms`);
  check(res, { "cold start 200": (r) => r.status === 200 });
}

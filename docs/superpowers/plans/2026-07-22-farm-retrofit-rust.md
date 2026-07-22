# Farm Event-Sourcing Retrofit (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **This is the Rust leg of a three-language retrofit.** The same domain redesign lands in `meshql` (Java) and `meshobj` (TypeScript) via sibling plans in this same directory: `2026-07-22-farm-retrofit-java.md` and `2026-07-22-farm-retrofit-ts.md`. All three implement the same approved spec (`docs/superpowers/specs/2026-07-22-farm-event-sourcing-retrofit-design.md`) independently — this plan touches only `meshql-rs`.
>
> **Do not implement this on `main` directly.** Per `superpowers:using-git-worktrees` / `superpowers:subagent-driven-development`, create a dedicated worktree first (Task 1 below does this, following the repo's existing convention — see `.worktrees/meshql-changes` for precedent). All tasks in this plan assume you are working inside that worktree.

**Goal:** Retrofit `examples/farm` from plain CRUD to a partial event-sourced shape — `lay_report` becomes a create-only domain event, `hen_productivity` becomes a new read-mostly projection entity — with per-entity Casbin authorization and a from-scratch manifest generator, so farm gives the `domain` field work (and the other two languages) a real event/projection example to work from.

**Architecture:** `lay_report`'s JSON/GraphQL schemas migrate to `{henId, eggs, timeOfDay}` and its restlette's `update`/`delete` verbs are denied by policy (not removed as routes — Casbin denies them). `hen_productivity` is wired as an ordinary restlette+graphlette pair, aggregate fields `{henId, totalEggs, lastLaidAt}`. Auth moves from one shared `Arc<dyn Auth>` for the whole `ServerConfig` to three separate `CasbinAuth` instances (actors, lay_report, hen_productivity), each with its own embedded policy — wired by hand-assembling restlette routers outside the `ServerConfig` convenience path (no framework signature changes needed; `build_restlette_router` already takes its own `Auth` per call). `meshql-restlette`'s dispatch changes from one `"write"` action string to `"create"`/`"update"`/`"delete"`, which is the only change to shared framework code. A from-scratch manifest generator (mirroring `egg-economy`'s) is added to `examples/farm`, checking file existence rather than `egg-economy`'s `ALL_VERBS` split — every farm entity gets both `graph` and `api` surfaces in the manifest, `hen_productivity` included, per the spec's "always advertise both surfaces" correction.

**Tech Stack:** Rust 2021, axum 0.7, `casbin` 2.20 (via `meshql-casbin`), MongoDB (via `meshql-mongo`), `jsonschema` 0.26 (manifest conformance), `testcontainers`/`testcontainers-modules` (Mongo integration tests), `reqwest` (HTTP test client), `cucumber`/BDD conventions are *not* used here — `examples/farm`'s existing test convention (see `meshql-mongo/tests/farm_cert.rs`, `meshql-restlette/tests/header_cert.rs`) is plain `#[tokio::test]` + `reqwest` against a spawned in-process server, which this plan follows.

---

## Decisions this plan makes (spec left these open)

1. **`hen_productivity` aggregate fields: `{henId, totalEggs, lastLaidAt}`.** Minimal by design (YAGNI) — total eggs laid all-time, and the timestamp of the most recently folded `lay_report`. Per-day/per-week breakdowns are additive (new optional JSON Schema properties, non-breaking) if a future need arises. The actual fold logic (a worker reading `lay_report` events and writing `hen_productivity`) is the companion `merkql-worker-pipeline` spec's job, not this one — `hen_productivity` here is wired exactly like any other restlette+graphlette pair; nothing in this plan writes to it automatically.
2. **`timeOfDay` is a full ISO-8601 timestamp** (`{"type": "string", "format": "date-time"}`), not a category like `"morning"`/`"evening"`. It replaces `date` as "when this happened," matching "a hen laid N eggs at this time."
3. **Auth-dispatch mechanism: hand-assemble restlette routers outside `ServerConfig`, no framework signature changes.** `build_restlette_router`/`build_restlette_router_ext` (`meshql-restlette/src/routes.rs`) and `GraphletteRouter::build_with_auth` (`meshql-graphlette/src/schema_builder.rs`) already accept their own `Arc<dyn Auth>` per call, independent of `ServerConfig`. The only thing that forces *one* shared `Auth` across every restlette is the `ServerConfig` + `build_app_with_auth`/`run_ext` convenience path (`meshql-server/src/lib.rs`), which loops `config.restlettes` applying a single `auth` argument. Rather than adding an `Option<Arc<dyn Auth>>` field to `RestletteConfig` (which would force every one of the ~15 other call sites across the workspace — `meshql-mongo`, `meshql-sqlite`, `meshql-postgres`, `meshql-mysql`, `meshql-merkql`'s cert tests, `examples/egg-economy*`, `examples/farm-azure` — to add a new field to a struct literal, for zero benefit to any of them), this plan leaves `RestletteConfig` and `ServerConfig` **untouched**. Instead: `examples/farm` populates `ServerConfig.restlettes` with an **empty vec** (graphlettes only go through `ServerConfig`), and hand-builds an `axum::Router` merging five `build_restlette_router(path, repo, entity_specific_auth)` calls — three different `Arc<dyn Auth>` instances across the five restlettes — then passes that hand-built router as the `extra` argument to `run_ext(config, extra)`. This is the exact mechanism `examples/egg-economy` already uses for its own extra routes (SSE `/changes`, `/manifest`) — `examples/farm` is simply putting restlette routers in `extra` instead of `config.restlettes`. Zero changes to `meshql-core`, `meshql-server`, `meshql-graphlette`. The only shared-framework change in this plan is item 4 below, confined to `meshql-restlette/src/routes.rs`.
4. **Verb-granular action strings: change the three `authorize_action(&tokens, "write")` call sites in `meshql-restlette/src/routes.rs`** (create/update/delete handlers) to pass `"create"`/`"update"`/`"delete"` respectively. Confirmed safe: `grep` across the workspace shows `CasbinAuth` is not wired into any restlette anywhere today (only its own crate's unit tests call `authorize_action` directly with literal strings, unaffected by this change) and `Auth::authorize_action`'s default impl always returns `true` (so `NoAuth`-backed restlettes — every other example in the workspace — are behaviorally unaffected regardless of which string is passed). This is genuinely the "just vary the string" case the spec anticipated for Rust.
5. **Casbin policy shape: three `CasbinAuth::from_strings` instances, one shared `model.conf`, three policy `.csv` files, all `include_str!`-embedded** (matching how GraphQL/JSON schemas are already embedded in `examples/farm/src/main.rs` — no filesystem path dependency at runtime). All three instances wrap `NoAuth` as the inner identity source (`examples/farm` has no real identity/edge-header system today, and building one is out of scope for this retrofit — see decision 6). `NoAuth::get_auth_token` always returns `["*"]`; a `g, *, fe` binding in the actor and lay_report policies grants role `fe` to every caller by default, which is what makes "general/FE callers" (no token) work end-to-end over real HTTP. The `hen_productivity` policy grants `worker` create/update but has **no `g, *, ...` binding at all**, so it denies every verb to the default `"*"` caller — proving "FE callers are not authorized, of any verb, on hen_productivity" via a real HTTP 403, with no extra plumbing.
6. **No new identity-extraction middleware.** A real deployment would grant the `worker` role via a trusted edge header that populates `AuthContext`'s `groups` (a mechanism `CasbinAuth::get_auth_token` already implements — see `meshql-casbin/src/lib.rs:119-125`) or via `StashKeyAuth` + a `g` binding for a known service-account id. Building that identity plumbing for `examples/farm` is out of scope here (this retrofit is about wiring *existing* Casbin, not building new identity infrastructure) — the worker-role grant path is proven with a direct unit test against the `hen_productivity` `CasbinAuth` instance (`authorize_action(&["worker".into()], "create")`), and the FE-denial path is proven over real HTTP (decision 5). This is called out explicitly rather than hand-waved: **when the companion `merkql-worker-pipeline` worker is built, it will need this middleware (or an equivalent) to actually authenticate as `worker` in production** — flagged as follow-up work, not silently assumed away.
7. **No README task.** `examples/farm` has no `README.md` today (`find` confirms). The task brief says "update if one exists" — it doesn't, so there is nothing to update. Not creating one (out of scope, and this repo's CLAUDE.md forbids proactively creating docs).
8. **`examples/farm-azure` is untouched.** It's a separate deployment (merkql-backed) with its own copy of `farm`/`coop`/`hen`/`lay_report` — the spec's stated scope is `examples/farm` only. Flagged here so the implementing engineer doesn't confuse the two.

---

### Task 1: Create the dedicated worktree

**Files:** none (repo-level setup)

- [ ] **Step 1: Create the worktree and branch**
  ```bash
  cd /tank/repos/tailoredshapes/meshql-rs
  git worktree add .worktrees/farm-retrofit-rust -b farm-retrofit-rust
  ```
- [ ] **Step 2: Verify**
  ```bash
  git worktree list
  ```
  Expected output includes a new line:
  ```
  /tank/repos/tailoredshapes/meshql-rs/.worktrees/farm-retrofit-rust  <sha> [farm-retrofit-rust]
  ```
- [ ] **Step 3: Do all remaining work inside the worktree.** From here on, every file path in this plan is relative to `/tank/repos/tailoredshapes/meshql-rs/.worktrees/farm-retrofit-rust/` (the worktree checkout), not the main checkout. `cargo` commands below should be run with `--manifest-path .worktrees/farm-retrofit-rust/Cargo.toml` if invoked from the main checkout's directory, or `cd` into the worktree first — this plan writes commands assuming you've `cd`ed into the worktree root.
  ```bash
  cd /tank/repos/tailoredshapes/meshql-rs/.worktrees/farm-retrofit-rust
  ```
- [ ] **Step 4: No commit for this task** (nothing changed yet — the worktree itself isn't a commit). Proceed to Task 2.

---

### Task 2: Migrate `lay_report` schema to `{henId, eggs, timeOfDay}`

**Files:**
- Modify: `examples/farm/config/json/lay_report.schema.json`
- Modify: `examples/farm/config/graph/lay_report.graphql`
- Modify: `examples/farm/config/graph/hen.graphql`
- Modify: `examples/farm/src/main.rs:71-77` (lay_report_config)
- Test: `examples/farm/tests/lay_report_schema_cert.rs` (new)

This is a breaking schema change (`date`→`timeOfDay`, `count`→`eggs`), so write the test first against the *new* shape — it will fail against the current server (old field names) until the schema/wiring change lands.

- [ ] **Step 1: Write the failing test.** Create `examples/farm/tests/lay_report_schema_cert.rs`:
  ```rust
  //! Proves lay_report's REST payload shape is {henId, eggs, timeOfDay} —
  //! the breaking schema migration in the farm-event-sourcing-retrofit spec.
  //! Uses the same spawn-a-real-server-and-hit-it-with-reqwest convention as
  //! meshql-restlette/tests/header_cert.rs and meshql-mongo/tests/farm_cert.rs.

  use meshql_core::NoAuth;
  use meshql_mongo::MongoRepository;
  use meshql_restlette::build_restlette_router;
  use std::sync::Arc;
  use testcontainers::runners::AsyncRunner;
  use testcontainers_modules::mongo::Mongo;

  async fn spawn_lay_report_server(mongo_uri: &str) -> String {
      let db = format!("lay_report_schema_{}", uuid::Uuid::new_v4().simple());
      let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);
      let repo = Arc::new(
          MongoRepository::new(mongo_uri, &db, "lay_reports", Arc::clone(&auth))
              .await
              .unwrap(),
      );
      let router = build_restlette_router("/lay_report/api", repo, auth);

      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
      tokio::spawn(async move {
          axum::serve(listener, router).await.unwrap();
      });
      addr
  }

  #[tokio::test]
  async fn accepts_hen_id_eggs_time_of_day_shape() {
      let container = Mongo::default().start().await.unwrap();
      let port = container.get_host_port_ipv4(27017).await.unwrap();
      let mongo_uri = format!("mongodb://127.0.0.1:{port}");
      let addr = spawn_lay_report_server(&mongo_uri).await;

      let resp = reqwest::Client::new()
          .post(format!("{addr}/lay_report/api"))
          .json(&serde_json::json!({
              "henId": "hen-1",
              "eggs": 2,
              "timeOfDay": "2026-07-22T08:00:00Z"
          }))
          .send()
          .await
          .unwrap();
      assert_eq!(resp.status(), 201);
      let body: serde_json::Value = resp.json().await.unwrap();
      assert_eq!(body["henId"], "hen-1");
      assert_eq!(body["eggs"], 2);
      assert_eq!(body["timeOfDay"], "2026-07-22T08:00:00Z");
      // The old shape's fields must not be echoed back.
      assert!(body.get("date").is_none());
      assert!(body.get("count").is_none());
  }
  ```
  Note: this test constructs the restlette router directly (schema validation is not enforced at the restlette layer today — `examples/farm/src/main.rs` passes `serde_json::json!({})` as `schema_json`, i.e. no-op validation — see main.rs lines 108-127). This test therefore proves the *payload shape convention*, not schema-validation rejection of the old shape. That matches the existing repo convention (no example currently enables JSON Schema validation on its restlettes).
- [ ] **Step 2: Run test to verify it currently fails to build/is inapplicable.** This test doesn't yet reference anything that doesn't exist, so it will actually *pass* today (nothing in the restlette layer validates field names) — the real regression signal is in Step 4 of `main.rs`'s GraphQL/JSON schema files being wrong. To make Step 2 meaningful, temporarily confirm the *schema files* still say `date`/`count`:
  ```bash
  cargo test -p farm --test lay_report_schema_cert 2>&1 | tail -20
  grep -n "date\|count" examples/farm/config/json/lay_report.schema.json examples/farm/config/graph/lay_report.graphql
  ```
  Expected: the test passes (restlette has no schema enforcement), but the grep shows the **committed schema files still describe the old `date`/`count` shape** — i.e., the documentation/schema-as-contract is stale relative to what Step 1 just proved the wire format actually is. That staleness is what this task fixes.
- [ ] **Step 3: Update the JSON Schema.** Replace `examples/farm/config/json/lay_report.schema.json`:
  ```json
  {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
      "henId": { "type": "string" },
      "eggs": { "type": "integer", "minimum": 0 },
      "timeOfDay": { "type": "string", "format": "date-time" }
    },
    "required": ["henId", "eggs", "timeOfDay"]
  }
  ```
- [ ] **Step 4: Update the GraphQL schema.** Replace `examples/farm/config/graph/lay_report.graphql`:
  ```graphql
  type LayReport {
      id: ID
      henId: String
      eggs: Int
      timeOfDay: String
      hen: Hen
  }

  type Hen {
      id: ID
      name: String
  }

  type Query {
      getLayReport(id: ID, at: Float): LayReport
      getLayReports(at: Float): [LayReport]
      getLayReportsByHen(id: ID, at: Float): [LayReport]
  }
  ```
  (`getLayReports` drops its `date` filter argument — there's no longer a natural single-value filter field on lay_report other than `henId`, which `getLayReportsByHen` already covers — it becomes a plain list-all, matching the `getAll`-style vector query used elsewhere in this codebase, e.g. `examples/egg-economy/config/graph/*.graphql`.)
- [ ] **Step 5: Update `hen.graphql`'s local denormalized `LayReport` view.** In `examples/farm/config/graph/hen.graphql`, replace the local `LayReport` type (currently `{id, date, count}`):
  ```graphql
  type LayReport {
      id: ID
      eggs: Int
      timeOfDay: String
  }
  ```
- [ ] **Step 6: Update `main.rs`'s `lay_report_config`.** In `examples/farm/src/main.rs`, replace lines 71-77. Note this also fixes a pre-existing, unrelated bug while we're here: the current `getLayReportsByHen` template is missing the `payload.` prefix Mongo-backed payload-field queries need (same class of bug fixed for `hen_productivity` — see decision 3's cross-reference; `meshql-mongo`'s converters nest payload fields under a `payload` subdocument, so a bare `henId` filter silently matches nothing against real Mongo):
  ```rust
  let lay_report_config = RootConfig::builder()
      .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
      .vector("getLayReports", "{}")
      .vector("getLayReportsByHen", r#"{"payload.henId": "{{id}}"}"#)
      .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
      .build();
  ```
- [ ] **Step 7: Run test to verify it passes and build succeeds.**
  ```bash
  cargo build -p farm
  cargo test -p farm --test lay_report_schema_cert
  ```
  Expected: `test accepts_hen_id_eggs_time_of_day_shape ... ok` (requires Docker running, for the Mongo testcontainer). Also confirm `cargo build -p farm` succeeds cleanly (no stale references to `date`/`count` anywhere in `main.rs`).
- [ ] **Step 8: Commit.**
  ```bash
  git add examples/farm/config/json/lay_report.schema.json \
          examples/farm/config/graph/lay_report.graphql \
          examples/farm/config/graph/hen.graphql \
          examples/farm/src/main.rs \
          examples/farm/tests/lay_report_schema_cert.rs
  git commit -m "$(cat <<'EOF'
  Migrate farm's lay_report schema to {henId, eggs, timeOfDay}

  Breaking change per the farm-event-sourcing-retrofit spec: lay_report's
  old {henId, date, count} shape predates the events/projections pattern.
  The new shape reads as a fact ("a hen laid N eggs at this time") and
  matches the camelCase FK convention (henId) already used elsewhere.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 3: Add `hen_productivity` entity

**Files:**
- Create: `examples/farm/config/json/hen_productivity.schema.json`
- Create: `examples/farm/config/graph/hen_productivity.graphql`
- Modify: `examples/farm/config/graph/hen.graphql` (add `productivity` field)
- Modify: `examples/farm/src/main.rs` (wire the new entity)
- Test: `examples/farm/tests/hen_productivity_cert.rs` (new)

`hen_productivity` is wired exactly like `farm`/`coop`/`hen`/`lay_report` today — an ordinary restlette+graphlette pair, still under the old shared-`NoAuth` `ServerConfig.restlettes` path at the end of this task. Task 5 later moves *all* restlettes (including this one) out of `ServerConfig.restlettes` and gives this one its own Casbin policy. Building it as an ordinary entity first, and proving it works, keeps this task focused.

- [ ] **Step 1: Write the failing test.** Create `examples/farm/tests/hen_productivity_cert.rs`:
  ```rust
  //! Proves hen_productivity is wired as an ordinary restlette+graphlette
  //! pair with the {henId, totalEggs, lastLaidAt} aggregate shape (decision
  //! #1 in the plan — exact fields aren't settled by the spec).

  use meshql_core::NoAuth;
  use meshql_mongo::MongoRepository;
  use meshql_restlette::build_restlette_router;
  use std::sync::Arc;
  use testcontainers::runners::AsyncRunner;
  use testcontainers_modules::mongo::Mongo;

  #[tokio::test]
  async fn accepts_hen_id_total_eggs_last_laid_at_shape() {
      let container = Mongo::default().start().await.unwrap();
      let port = container.get_host_port_ipv4(27017).await.unwrap();
      let mongo_uri = format!("mongodb://127.0.0.1:{port}");
      let db = format!("hen_productivity_{}", uuid::Uuid::new_v4().simple());
      let auth: Arc<dyn meshql_core::Auth> = Arc::new(NoAuth);
      let repo = Arc::new(
          MongoRepository::new(&mongo_uri, &db, "hen_productivities", Arc::clone(&auth))
              .await
              .unwrap(),
      );
      let router = build_restlette_router("/hen_productivity/api", repo, auth);

      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
      tokio::spawn(async move {
          axum::serve(listener, router).await.unwrap();
      });

      let resp = reqwest::Client::new()
          .post(format!("{addr}/hen_productivity/api"))
          .json(&serde_json::json!({
              "henId": "hen-1",
              "totalEggs": 42,
              "lastLaidAt": "2026-07-22T08:00:00Z"
          }))
          .send()
          .await
          .unwrap();
      assert_eq!(resp.status(), 201);
      let body: serde_json::Value = resp.json().await.unwrap();
      assert_eq!(body["henId"], "hen-1");
      assert_eq!(body["totalEggs"], 42);
      assert_eq!(body["lastLaidAt"], "2026-07-22T08:00:00Z");
  }
  ```
- [ ] **Step 2: Run test to verify it fails.**
  ```bash
  cargo test -p farm --test hen_productivity_cert 2>&1 | tail -20
  ```
  Expected: compiles and runs today (nothing about this test depends on `main.rs` wiring — it builds its own router directly), so it will actually pass at this point too, same caveat as Task 2. The real proof-of-work for this task is `main.rs` wiring the entity into the actual server config, checked in Step 5.
- [ ] **Step 3: Create the JSON Schema.** `examples/farm/config/json/hen_productivity.schema.json`:
  ```json
  {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
      "henId": { "type": "string" },
      "totalEggs": { "type": "integer", "minimum": 0 },
      "lastLaidAt": { "type": "string", "format": "date-time" }
    },
    "required": ["henId", "totalEggs"]
  }
  ```
- [ ] **Step 4: Create the GraphQL schema.** `examples/farm/config/graph/hen_productivity.graphql`:
  ```graphql
  type HenProductivity {
      id: ID
      henId: String
      totalEggs: Int
      lastLaidAt: String
      hen: Hen
  }

  type Hen {
      id: ID
      name: String
  }

  type Query {
      getHenProductivity(id: ID, at: Float): HenProductivity
      getHenProductivities(at: Float): [HenProductivity]
      getHenProductivityByHen(id: ID, at: Float): [HenProductivity]
  }
  ```
- [ ] **Step 5: Add the reverse relation to `hen.graphql`.** In `examples/farm/config/graph/hen.graphql`, add a `productivity` field and local `HenProductivity` type (mirroring the existing `layReports` field/local `LayReport` type pattern):
  ```graphql
  type Hen {
      id: ID
      coopId: String
      name: String
      dateOfBirth: String
      coop: Coop
      layReports: [LayReport]
      productivity: [HenProductivity]
  }
  ```
  and add, alongside the existing local `LayReport` type:
  ```graphql
  type HenProductivity {
      id: ID
      totalEggs: Int
      lastLaidAt: String
  }
  ```
- [ ] **Step 6: Wire it into `main.rs`.** Add the const, repo, searcher, root config, and both `ServerConfig` entries — following the exact `adding-an-entity.md` pattern. In `examples/farm/src/main.rs`:
  - Add near the other `const ..._GRAPHQL` lines:
    ```rust
    const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");
    ```
  - Add near the other repo declarations:
    ```rust
    let hen_productivity_repo = Arc::new(
        MongoRepository::new(MONGO_URI, DB_NAME, "hen_productivities", Arc::clone(&auth)).await?,
    );
    ```
  - Add near the other searcher declarations:
    ```rust
    let hen_productivity_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(MONGO_URI, DB_NAME, "hen_productivities", Arc::clone(&auth)).await?,
    );
    ```
  - Add near the other root configs:
    ```rust
    let hen_productivity_config = RootConfig::builder()
        .singleton("getHenProductivity", r#"{"id": "{{id}}"}"#)
        .vector("getHenProductivities", "{}")
        .vector("getHenProductivityByHen", r#"{"payload.henId": "{{id}}"}"#)
        .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
        .build();
    ```
  - Note the `payload.` prefix on `henId`: this is a Mongo-backed restlette, and `meshql-mongo`'s converters nest payload fields under a `payload` subdocument — a bare `{"henId": ...}` filter silently matches nothing against real Mongo, which would make every lookup return empty and (downstream, in the merkql worker pipeline) cause a new `hen_productivity` record to be created per lay_report instead of one accumulating record per hen.
  - Also add `.vector_resolver("productivity", None, "getHenProductivityByHen", "/hen_productivity/graph")` to `hen_config`'s builder chain (alongside the existing `.vector_resolver("layReports", ...)` call).
  - Add to `config.graphlettes`:
    ```rust
    GraphletteConfig {
        path: "/hen_productivity/graph".to_string(),
        schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
        root_config: hen_productivity_config,
        searcher: hen_productivity_searcher,
    },
    ```
  - Add to `config.restlettes`:
    ```rust
    RestletteConfig {
        path: "/hen_productivity/api".to_string(),
        schema_json: serde_json::json!({}),
        repository: hen_productivity_repo,
    },
    ```
- [ ] **Step 7: Run test to verify it passes, and smoke-test the real server wiring.**
  ```bash
  cargo build -p farm
  cargo test -p farm --test hen_productivity_cert
  ```
  Expected: build succeeds, test passes. Also manually sanity-check the schema builder accepts the new `productivity` federation field — this is caught by `cargo build -p farm` since `RootConfig`/schema wiring is checked at construction, and by starting the server once locally against a local Mongo if available (optional, not required for this task's automated check).
- [ ] **Step 8: Commit.**
  ```bash
  git add examples/farm/config/json/hen_productivity.schema.json \
          examples/farm/config/graph/hen_productivity.graphql \
          examples/farm/config/graph/hen.graphql \
          examples/farm/src/main.rs \
          examples/farm/tests/hen_productivity_cert.rs
  git commit -m "$(cat <<'EOF'
  Add hen_productivity entity to examples/farm

  Wired as an ordinary restlette+graphlette pair (repo+searcher+
  RootConfig+Graphlette/RestletteConfig), matching the canonical
  adding-an-entity.md shape. Aggregate fields {henId, totalEggs,
  lastLaidAt} — a deliberately minimal choice (plan decision #1); the
  fold logic that populates it is the companion merkql-worker-pipeline
  spec's job, not this one.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 4: Verb-granular `authorize_action` strings in `meshql-restlette`

**Files:**
- Modify: `meshql-restlette/src/routes.rs:137,217,256`
- Test: `meshql-restlette/tests/verb_granular_auth_cert.rs` (new)

This is the one shared-framework change in this plan (see decision 4). It's additive/behavior-neutral for every other consumer in the workspace (confirmed via `grep` — no other example wires `CasbinAuth`, and `Auth::authorize_action`'s default always returns `true`).

- [ ] **Step 1: Write the failing test.** Create `meshql-restlette/tests/verb_granular_auth_cert.rs`:
  ```rust
  //! Proves create/update/delete each pass their own verb as the
  //! authorize_action string, not one shared "write" — the change the
  //! farm-event-sourcing-retrofit spec's Auth section requires so a
  //! Casbin policy can express "create allowed, update/delete denied"
  //! (lay_report's new create-only contract).

  use meshql_core::{Auth, Envelope, Stash};
  use meshql_restlette::build_restlette_router;
  use meshql_sqlite::SqliteRepository;
  use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
  use std::str::FromStr;
  use std::sync::Arc;

  /// Denies every action except "create" — if create/update/delete all
  /// passed the same "write" string, this would make create ALSO fail
  /// (since "write" != "create"), so a passing create + failing
  /// update/delete proves the three handlers pass distinct strings.
  struct CreateOnlyAuth;
  impl Auth for CreateOnlyAuth {
      fn get_auth_token(&self, _context: &Stash) -> Vec<String> {
          vec![]
      }
      fn is_authorized(&self, _credentials: &[String], _envelope: &Envelope) -> bool {
          true
      }
      fn authorize_action(&self, _credentials: &[String], action: &str) -> bool {
          action == "create"
      }
  }

  async fn spawn_server() -> String {
      let pool = SqlitePoolOptions::new()
          .max_connections(1)
          .connect_with(
              SqliteConnectOptions::from_str("sqlite::memory:")
                  .unwrap()
                  .create_if_missing(true),
          )
          .await
          .unwrap();
      let repo = Arc::new(SqliteRepository::new_with_pool(pool).await.unwrap());
      let router = build_restlette_router("/widgets", repo, Arc::new(CreateOnlyAuth));

      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
      tokio::spawn(async move {
          axum::serve(listener, router).await.unwrap();
      });
      addr
  }

  #[tokio::test]
  async fn create_succeeds_update_and_delete_are_denied() {
      let addr = spawn_server().await;
      let client = reqwest::Client::new();

      let created: serde_json::Value = client
          .post(format!("{addr}/widgets"))
          .json(&serde_json::json!({"name": "sprocket"}))
          .send()
          .await
          .unwrap()
          .json()
          .await
          .unwrap();
      let id = created["id"].as_str().unwrap();

      let update_resp = client
          .put(format!("{addr}/widgets/{id}"))
          .json(&serde_json::json!({"name": "sprocket-v2"}))
          .send()
          .await
          .unwrap();
      assert_eq!(update_resp.status(), 403, "update must pass \"update\", not \"create\"");

      let delete_resp = client.delete(format!("{addr}/widgets/{id}")).send().await.unwrap();
      assert_eq!(delete_resp.status(), 403, "delete must pass \"delete\", not \"create\"");
  }
  ```
- [ ] **Step 2: Run test to verify it fails.**
  ```bash
  cargo test -p meshql-restlette --test verb_granular_auth_cert 2>&1 | tail -30
  ```
  Expected: `create_succeeds_update_and_delete_are_denied ... FAILED` — the create POST itself returns 403 today (current code passes `"write"` for all three verbs, and `CreateOnlyAuth` only allows `"create"`, so `"write" != "create"` denies even the create step), causing the `.json().await.unwrap()` on the create response to panic (no JSON body on a 403) or the first assertion to fail, depending on where it trips. Confirm the failure is in the create/update path, not a compile error.
- [ ] **Step 3: Change the three action strings.** In `meshql-restlette/src/routes.rs`:
  - Line 137 (`create_handler`): change `"write"` → `"create"`.
  - Line 217 (`update_handler`): change `"write"` → `"update"`.
  - Line 256 (`delete_handler`): change `"write"` → `"delete"`.
  ```rust
  // create_handler, was: if !state.auth.authorize_action(&tokens, "write") {
  if !state.auth.authorize_action(&tokens, "create") {
  ```
  ```rust
  // update_handler, was: if !state.auth.authorize_action(&tokens, "write") {
  if !state.auth.authorize_action(&tokens, "update") {
  ```
  ```rust
  // delete_handler, was: if !state.auth.authorize_action(&tokens, "write") {
  if !state.auth.authorize_action(&tokens, "delete") {
  ```
- [ ] **Step 4: Run test to verify it passes, plus the full existing suite for this crate.**
  ```bash
  cargo test -p meshql-restlette
  ```
  Expected: `create_succeeds_update_and_delete_are_denied ... ok`, and the pre-existing `header_cert.rs` tests (which use `NoAuth`, unaffected by this change) still pass.
- [ ] **Step 5: Commit.**
  ```bash
  git add meshql-restlette/src/routes.rs meshql-restlette/tests/verb_granular_auth_cert.rs
  git commit -m "$(cat <<'EOF'
  meshql-restlette: pass verb-granular action strings to authorize_action

  create/update/delete now pass "create"/"update"/"delete" instead of
  one shared "write" string, so a Casbin policy can express "create
  allowed, update/delete denied" (lay_report's new create-only
  contract in the farm-event-sourcing-retrofit spec). Behavior-neutral
  for every other caller in the workspace: Auth::authorize_action's
  default always returns true, and no example currently wires
  CasbinAuth into a restlette.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 5: Per-entity Casbin auth — refactor `main.rs` into `lib.rs` `build()`

**Files:**
- Create: `examples/farm/src/lib.rs`
- Create: `examples/farm/config/casbin/model.conf`
- Create: `examples/farm/config/casbin/actor_policy.csv`
- Create: `examples/farm/config/casbin/lay_report_policy.csv`
- Create: `examples/farm/config/casbin/hen_productivity_policy.csv`
- Modify: `examples/farm/Cargo.toml`
- Modify: `examples/farm/src/main.rs` (becomes thin)
- Test: `examples/farm/tests/auth_policy_cert.rs` (new)

This is the task that implements decisions 3, 5, and 6. It moves the entire wiring body out of `main.rs` into a reusable `pub async fn build(...)` in `lib.rs` (so both `main.rs` and integration tests exercise the *real* wiring, not a re-implementation of it — matching why `examples/egg-economy` factors `manifest`/`projectors`/etc. into `lib.rs`), and changes restlette construction from `ServerConfig.restlettes` (one shared `Auth`) to a hand-built router with three `CasbinAuth` instances merged into `run_ext`'s `extra` argument.

- [ ] **Step 1: Add dependencies.** In `examples/farm/Cargo.toml`, add to `[dependencies]`:
  ```toml
  meshql-casbin = { path = "../../meshql-casbin" }
  axum = { workspace = true }
  ```
  and add a new `[dev-dependencies]` section:
  ```toml
  [dev-dependencies]
  reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
  testcontainers = "0.23"
  testcontainers-modules = { version = "0.11", features = ["mongo"] }
  uuid = { workspace = true }
  ```
- [ ] **Step 2: Write the failing test.** Create `examples/farm/tests/auth_policy_cert.rs` — this is the test that proves the whole auth-dispatch mechanism end-to-end. It will fail to compile until `farm::build` exists:
  ```rust
  //! Proves the per-entity Casbin auth wiring end-to-end over real HTTP:
  //!   - farm/coop/hen: full CRUD for the default ("fe") caller
  //!   - lay_report: create allowed, update/delete denied (403) for "fe"
  //!   - hen_productivity: every verb denied (403) for "fe" — no policy
  //!     row grants it anything
  //! Plus a direct unit-level proof that the "worker" role (which a real
  //! deployment would grant via trusted-header identity injection — see
  //! plan decision #6, out of scope to build here) can create/update
  //! hen_productivity per the embedded policy.

  use meshql_casbin::CasbinAuth;
  use meshql_core::{Auth, NoAuth};
  use testcontainers::runners::AsyncRunner;
  use testcontainers_modules::mongo::Mongo;

  async fn spawn_farm() -> String {
      let container = Mongo::default().start().await.unwrap();
      let port = container.get_host_port_ipv4(27017).await.unwrap();
      let mongo_uri = format!("mongodb://127.0.0.1:{port}");
      let db_name = format!("farm_auth_{}", uuid::Uuid::new_v4().simple());

      let (config, extra) = farm::build(&mongo_uri, &db_name).await.unwrap();
      let app = meshql_server::build_app_ext(config, extra).await.unwrap();
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
      tokio::spawn(async move {
          axum::serve(listener, app).await.unwrap();
      });
      // Container must outlive the spawned server; leak it for test simplicity
      // (matches the Box::leak(tempdir) convention used elsewhere in this repo
      // for keeping test resources alive across a spawned async task).
      Box::leak(Box::new(container));
      addr
  }

  #[tokio::test]
  async fn actors_get_full_crud_by_default() {
      let addr = spawn_farm().await;
      let client = reqwest::Client::new();

      let farm: serde_json::Value = client
          .post(format!("{addr}/farm/api"))
          .json(&serde_json::json!({"name": "Green Acres"}))
          .send().await.unwrap()
          .json().await.unwrap();
      let id = farm["id"].as_str().unwrap();

      let update = client
          .put(format!("{addr}/farm/api/{id}"))
          .json(&serde_json::json!({"name": "Green Acres II"}))
          .send().await.unwrap();
      assert_eq!(update.status(), 200);

      let delete = client.delete(format!("{addr}/farm/api/{id}")).send().await.unwrap();
      assert_eq!(delete.status(), 200);
  }

  #[tokio::test]
  async fn lay_report_is_create_only() {
      let addr = spawn_farm().await;
      let client = reqwest::Client::new();

      let created: serde_json::Value = client
          .post(format!("{addr}/lay_report/api"))
          .json(&serde_json::json!({"henId": "hen-1", "eggs": 2, "timeOfDay": "2026-07-22T08:00:00Z"}))
          .send().await.unwrap()
          .json().await.unwrap();
      let id = created["id"].as_str().unwrap();

      let update = client
          .put(format!("{addr}/lay_report/api/{id}"))
          .json(&serde_json::json!({"eggs": 3}))
          .send().await.unwrap();
      assert_eq!(update.status(), 403, "lay_report update must be denied");

      let delete = client.delete(format!("{addr}/lay_report/api/{id}")).send().await.unwrap();
      assert_eq!(delete.status(), 403, "lay_report delete must be denied");
  }

  #[tokio::test]
  async fn hen_productivity_denies_default_caller_every_verb() {
      let addr = spawn_farm().await;
      let client = reqwest::Client::new();

      let create = client
          .post(format!("{addr}/hen_productivity/api"))
          .json(&serde_json::json!({"henId": "hen-1", "totalEggs": 10}))
          .send().await.unwrap();
      assert_eq!(create.status(), 403, "hen_productivity create must be denied to the default caller");
  }

  #[tokio::test]
  async fn worker_role_can_create_and_update_hen_productivity() {
      let model = include_str!("../config/casbin/model.conf");
      let policy = include_str!("../config/casbin/hen_productivity_policy.csv");
      let auth = CasbinAuth::from_strings(model, policy, NoAuth).await.unwrap();

      assert!(auth.authorize_action(&["worker".to_string()], "create"));
      assert!(auth.authorize_action(&["worker".to_string()], "update"));
      assert!(!auth.authorize_action(&["worker".to_string()], "delete"));
      assert!(!auth.authorize_action(&[], "create"), "default caller has no role in this policy");
  }
  ```
- [ ] **Step 3: Run test to verify it fails to compile.**
  ```bash
  cargo test -p farm --test auth_policy_cert 2>&1 | tail -30
  ```
  Expected: `error[E0433]: failed to resolve: use of undeclared crate or module 'farm'` (no `lib.rs` yet) and missing `config/casbin/*` files.
- [ ] **Step 4: Create the Casbin model.** `examples/farm/config/casbin/model.conf` (identical shape to `meshql-casbin/tests/fixtures/model.conf` — proven-working RBAC-with-glob-object model):
  ```
  [request_definition]
  r = sub, obj, act

  [policy_definition]
  p = sub, obj, act

  [role_definition]
  g = _, _

  [policy_effect]
  e = some(where (p.eft == allow))

  [matchers]
  m = g(r.sub, p.sub) && keyMatch(r.obj, p.obj) && (r.act == p.act || p.act == "*")
  ```
- [ ] **Step 5: Create the three policy files.**

  `examples/farm/config/casbin/actor_policy.csv` (farm/coop/hen — full CRUD, granted to everyone by default):
  ```
  p, fe, /*, create
  p, fe, /*, update
  p, fe, /*, delete

  g, *, fe
  ```

  `examples/farm/config/casbin/lay_report_policy.csv` (create-only):
  ```
  p, fe, /*, create

  g, *, fe
  ```

  `examples/farm/config/casbin/hen_productivity_policy.csv` (worker-only, no default grant):
  ```
  p, worker, /*, create
  p, worker, /*, update
  ```
  (No `g, *, ...` row here — this is what makes the default `"*"` caller (everyone, since `NoAuth::get_auth_token` always returns `["*"]`) resolve to zero roles under this policy, denying every verb.)

- [ ] **Step 6: Create `examples/farm/src/lib.rs`** — the full wiring, extracted from `main.rs` (post Tasks 2-3) and restructured per decision 3 (restlettes hand-built with per-entity auth, not in `ServerConfig.restlettes`):
  ```rust
  //! examples/farm — the minimal, non-event-sourced meshql reference
  //! example, partially retrofitted per
  //! docs/superpowers/specs/2026-07-22-farm-event-sourcing-retrofit-design.md:
  //! lay_report is a create-only domain event, hen_productivity is a new
  //! projection entity, and writes are authorized per-entity via three
  //! separate CasbinAuth instances (see the plan's "Decisions" section for
  //! why this doesn't require any meshql-core/meshql-server changes).

  use meshql_casbin::CasbinAuth;
  use meshql_core::{Auth, GraphletteConfig, NoAuth, RootConfig, ServerConfig};
  use meshql_mongo::{MongoRepository, MongoSearcher};
  use meshql_restlette::build_restlette_router;
  use std::sync::Arc;

  pub mod manifest;

  const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
  const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
  const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
  const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");
  const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");

  const CASBIN_MODEL: &str = include_str!("../config/casbin/model.conf");
  const ACTOR_POLICY: &str = include_str!("../config/casbin/actor_policy.csv");
  const LAY_REPORT_POLICY: &str = include_str!("../config/casbin/lay_report_policy.csv");
  const HEN_PRODUCTIVITY_POLICY: &str = include_str!("../config/casbin/hen_productivity_policy.csv");

  /// Build the farm ServerConfig (graphlettes only — reads stay open to
  /// everyone, per the spec) plus a hand-assembled restlette Router with
  /// per-entity Casbin auth, ready to pass as `run_ext`'s `extra` argument
  /// (after mounting `/manifest` on it — see `main.rs`).
  ///
  /// Shared by `main.rs` and integration tests, so tests exercise the real
  /// wiring rather than a re-implementation of it.
  pub async fn build(mongo_uri: &str, db_name: &str) -> anyhow::Result<(ServerConfig, axum::Router)> {
      // Reads (GraphQL) stay open to everyone — this retrofit is about
      // write authorization, not read restriction (per spec).
      let read_auth: Arc<dyn Auth> = Arc::new(NoAuth);

      // Three separate CasbinAuth instances = the per-entity discrimination
      // mechanism. CasbinAuth::authorize_action's Casbin object is always
      // the literal "/api" (meshql-casbin/src/lib.rs), so discrimination
      // happens by *which instance* handles a restlette's requests —
      // decided here, in wiring code — not by the engine matching a
      // per-entity object string.
      let actor_auth: Arc<dyn Auth> =
          Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, ACTOR_POLICY, NoAuth).await?);
      let lay_report_auth: Arc<dyn Auth> =
          Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, LAY_REPORT_POLICY, NoAuth).await?);
      let hen_productivity_auth: Arc<dyn Auth> =
          Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, HEN_PRODUCTIVITY_POLICY, NoAuth).await?);

      // --- Repositories ---
      let farm_repo =
          Arc::new(MongoRepository::new(mongo_uri, db_name, "farms", Arc::clone(&read_auth)).await?);
      let coop_repo =
          Arc::new(MongoRepository::new(mongo_uri, db_name, "coops", Arc::clone(&read_auth)).await?);
      let hen_repo =
          Arc::new(MongoRepository::new(mongo_uri, db_name, "hens", Arc::clone(&read_auth)).await?);
      let lay_report_repo = Arc::new(
          MongoRepository::new(mongo_uri, db_name, "lay_reports", Arc::clone(&read_auth)).await?,
      );
      let hen_productivity_repo = Arc::new(
          MongoRepository::new(mongo_uri, db_name, "hen_productivities", Arc::clone(&read_auth))
              .await?,
      );

      // --- Searchers ---
      let farm_searcher: Arc<dyn meshql_core::Searcher> =
          Arc::new(MongoSearcher::new(mongo_uri, db_name, "farms", Arc::clone(&read_auth)).await?);
      let coop_searcher: Arc<dyn meshql_core::Searcher> =
          Arc::new(MongoSearcher::new(mongo_uri, db_name, "coops", Arc::clone(&read_auth)).await?);
      let hen_searcher: Arc<dyn meshql_core::Searcher> =
          Arc::new(MongoSearcher::new(mongo_uri, db_name, "hens", Arc::clone(&read_auth)).await?);
      let lay_report_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
          MongoSearcher::new(mongo_uri, db_name, "lay_reports", Arc::clone(&read_auth)).await?,
      );
      let hen_productivity_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
          MongoSearcher::new(mongo_uri, db_name, "hen_productivities", Arc::clone(&read_auth))
              .await?,
      );

      // --- Root Configs ---
      let farm_config = RootConfig::builder()
          .singleton("getFarm", r#"{"id": "{{id}}"}"#)
          .vector("getFarms", r#"{"name": "{{name}}"}"#)
          .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
          .build();

      let coop_config = RootConfig::builder()
          .singleton("getCoop", r#"{"id": "{{id}}"}"#)
          .vector("getCoops", r#"{"name": "{{name}}"}"#)
          .vector("getCoopsByFarm", r#"{"farmId": "{{id}}"}"#)
          .singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
          .vector_resolver("hens", None, "getHensByCoop", "/hen/graph")
          .build();

      let hen_config = RootConfig::builder()
          .singleton("getHen", r#"{"id": "{{id}}"}"#)
          .vector("getHens", r#"{"name": "{{name}}"}"#)
          .vector("getHensByCoop", r#"{"coopId": "{{id}}"}"#)
          .singleton_resolver("coop", Some("coopId"), "getCoop", "/coop/graph")
          .vector_resolver("layReports", None, "getLayReportsByHen", "/lay_report/graph")
          .vector_resolver("productivity", None, "getHenProductivityByHen", "/hen_productivity/graph")
          .build();

      let lay_report_config = RootConfig::builder()
          .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
          .vector("getLayReports", "{}")
          .vector("getLayReportsByHen", r#"{"henId": "{{id}}"}"#)
          .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
          .build();

      let hen_productivity_config = RootConfig::builder()
          .singleton("getHenProductivity", r#"{"id": "{{id}}"}"#)
          .vector("getHenProductivities", "{}")
          .vector("getHenProductivityByHen", r#"{"payload.henId": "{{id}}"}"#)
          .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
          .build();

      // Graphlettes only — no restlettes here. build_app_with_auth (which
      // run_ext calls) applies exactly one shared Auth to every entry in
      // config.restlettes, which can't express three different policies.
      // Restlette routers are hand-built below instead, each with the
      // Auth instance appropriate to its own write policy, and merged into
      // the `extra` Router this function returns — the same mechanism
      // examples/egg-economy already uses for its own extra routes.
      let config = ServerConfig {
          port: 3033,
          graphlettes: vec![
              GraphletteConfig {
                  path: "/farm/graph".to_string(),
                  schema_text: FARM_GRAPHQL.to_string(),
                  root_config: farm_config,
                  searcher: farm_searcher,
              },
              GraphletteConfig {
                  path: "/coop/graph".to_string(),
                  schema_text: COOP_GRAPHQL.to_string(),
                  root_config: coop_config,
                  searcher: coop_searcher,
              },
              GraphletteConfig {
                  path: "/hen/graph".to_string(),
                  schema_text: HEN_GRAPHQL.to_string(),
                  root_config: hen_config,
                  searcher: hen_searcher,
              },
              GraphletteConfig {
                  path: "/lay_report/graph".to_string(),
                  schema_text: LAY_REPORT_GRAPHQL.to_string(),
                  root_config: lay_report_config,
                  searcher: lay_report_searcher,
              },
              GraphletteConfig {
                  path: "/hen_productivity/graph".to_string(),
                  schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
                  root_config: hen_productivity_config,
                  searcher: hen_productivity_searcher,
              },
          ],
          restlettes: vec![],
      };

      // Restlette routers: farm/coop/hen share actor_auth (full CRUD);
      // lay_report gets its own create-only policy; hen_productivity gets
      // its own worker-only policy that denies the default caller entirely.
      let restlette_router = axum::Router::new()
          .merge(build_restlette_router("/farm/api", farm_repo, Arc::clone(&actor_auth)))
          .merge(build_restlette_router("/coop/api", coop_repo, Arc::clone(&actor_auth)))
          .merge(build_restlette_router("/hen/api", hen_repo, Arc::clone(&actor_auth)))
          .merge(build_restlette_router(
              "/lay_report/api",
              lay_report_repo,
              Arc::clone(&lay_report_auth),
          ))
          .merge(build_restlette_router(
              "/hen_productivity/api",
              hen_productivity_repo,
              Arc::clone(&hen_productivity_auth),
          ));

      Ok((config, restlette_router))
  }
  ```
- [ ] **Step 7: Rewrite `main.rs`** to call `farm::build`:
  ```rust
  use meshql_server::run_ext;

  const MONGO_URI: &str = "mongodb://127.0.0.1:27017";
  const DB_NAME: &str = "farm_db";

  #[tokio::main]
  async fn main() -> anyhow::Result<()> {
      let (config, extra) = farm::build(MONGO_URI, DB_NAME).await?;
      run_ext(config, extra).await
  }
  ```
  (The `/manifest` route is added to `extra` in Task 6 — this task's `main.rs` runs without it for now, which is fine since Task 6 lands immediately after.)
- [ ] **Step 8: Run test to verify it passes.**
  ```bash
  cargo build -p farm
  cargo test -p farm --test auth_policy_cert
  ```
  Expected: all four tests in `auth_policy_cert.rs` pass (`actors_get_full_crud_by_default`, `lay_report_is_create_only`, `hen_productivity_denies_default_caller_every_verb`, `worker_role_can_create_and_update_hen_productivity`). Also re-run Tasks 2/3's tests to confirm they're unaffected by the refactor:
  ```bash
  cargo test -p farm --test lay_report_schema_cert --test hen_productivity_cert
  ```
- [ ] **Step 9: Commit.**
  ```bash
  git add examples/farm/Cargo.toml \
          examples/farm/src/lib.rs \
          examples/farm/src/main.rs \
          examples/farm/config/casbin/ \
          examples/farm/tests/auth_policy_cert.rs
  git commit -m "$(cat <<'EOF'
  Wire per-entity Casbin auth into examples/farm

  Three CasbinAuth instances (actor: farm/coop/hen full CRUD;
  lay_report: create-only; hen_productivity: worker-only, denies the
  default caller entirely) replace the single shared NoAuth. Restlette
  routers are hand-built with build_restlette_router (which already
  takes its own Auth per call) and merged into run_ext's `extra`
  argument instead of going through ServerConfig.restlettes, which
  can only apply one shared Auth to every entry — no meshql-core or
  meshql-server changes needed. Wiring moved from main.rs into a
  reusable farm::build() so integration tests exercise the real
  wiring, not a reimplementation of it.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 6: Manifest generator for `examples/farm`

**Files:**
- Create: `examples/farm/src/manifest.rs`
- Create: `examples/farm/src/bin/gen_manifest.rs`
- Create: `examples/farm/config/manifest.json` (generated artifact)
- Modify: `examples/farm/src/main.rs` (mount `/manifest`)
- Modify: `examples/farm/Cargo.toml` (dev-dependency: `jsonschema`)
- Test: `examples/farm/tests/manifest_conformance.rs` (new — the 3-test suite)

Mirrors `examples/egg-economy/src/manifest.rs`'s reference algorithm, but checks `config/json/<entity>.schema.json` file existence instead of `egg-economy`'s `ALL_VERBS` split (farm has no verb/noun split — every entity gets both surfaces, per the spec's "always advertise both surfaces" correction, and per the manifest-parity spec's reference algorithm, which already specifies the file-existence check, not `ALL_VERBS`, as the portable rule).

- [ ] **Step 1: Add the dev-dependency.** In `examples/farm/Cargo.toml`, add to `[dev-dependencies]`:
  ```toml
  jsonschema = { version = "0.26", default-features = false }
  ```
- [ ] **Step 2: Write the failing test.** Create `examples/farm/tests/manifest_conformance.rs`:
  ```rust
  //! Manifest conformance: the committed manifest validates against the
  //! published schema AND matches regeneration from the config files.
  //! Same three-test shape as examples/egg-economy/tests/manifest_conformance.rs.
  //! Unlike egg-economy, every farm entity (including hen_productivity)
  //! must advertise BOTH graph and api surfaces — farm has no verb/noun
  //! split (see the farm-event-sourcing-retrofit spec's manifest-generator
  //! section: "the manifest is honest about what exists").

  use std::path::Path;

  fn crate_dir() -> &'static Path {
      Path::new(env!("CARGO_MANIFEST_DIR"))
  }

  #[test]
  fn manifest_validates_against_published_schema() {
      let schema: serde_json::Value =
          serde_json::from_str(include_str!("../../../schemas/manifest.schema.json"))
              .expect("schema parses");
      let manifest: serde_json::Value =
          serde_json::from_str(include_str!("../config/manifest.json")).expect("manifest parses");

      let validator = jsonschema::validator_for(&schema).expect("schema compiles");
      let errors: Vec<String> = validator
          .iter_errors(&manifest)
          .map(|e| format!("{e} at {}", e.instance_path))
          .collect();
      assert!(errors.is_empty(), "manifest invalid:\n{}", errors.join("\n"));
  }

  #[test]
  fn manifest_matches_regeneration() {
      let committed: serde_json::Value =
          serde_json::from_str(include_str!("../config/manifest.json")).expect("manifest parses");
      let generated =
          farm::manifest::generate(&crate_dir().join("config")).expect("generation succeeds");
      assert_eq!(
          committed, generated,
          "config/manifest.json is stale — regenerate: cargo run -p farm --bin gen_manifest"
      );
  }

  #[test]
  fn every_entity_advertises_both_surfaces() {
      let manifest: serde_json::Value =
          serde_json::from_str(include_str!("../config/manifest.json")).expect("manifest parses");
      let entities = manifest["entities"].as_object().expect("entities object");

      let mut seen = 0;
      for dir_ent in std::fs::read_dir(crate_dir().join("config/graph")).unwrap() {
          let path = dir_ent.unwrap().path();
          if path.extension().and_then(|e| e.to_str()) != Some("graphql") {
              continue;
          }
          let entity = path.file_stem().unwrap().to_str().unwrap().to_string();
          let e = entities
              .get(&entity)
              .unwrap_or_else(|| panic!("entity '{entity}' missing from manifest"));
          assert_eq!(e["surfaces"]["graph"]["kind"], "graphql", "{entity} graph surface");
          // Every farm entity has a matching config/json/<entity>.schema.json,
          // so every entity — hen_productivity included — must advertise an
          // api surface too. A missing api surface here is exactly the
          // "restlette exists but manifest hides it" bug the spec corrects.
          assert_eq!(e["surfaces"]["api"]["kind"], "rest", "{entity} api surface");
          seen += 1;
      }
      assert_eq!(seen, entities.len(), "manifest entity count != graph file count");
  }
  ```
- [ ] **Step 3: Run test to verify it fails.**
  ```bash
  cargo test -p farm --test manifest_conformance 2>&1 | tail -30
  ```
  Expected: `error[E0433]: failed to resolve: could not find 'manifest' in 'farm'` and/or a file-not-found panic on `include_str!("../config/manifest.json")` (doesn't exist yet — compile error, so the whole crate fails to build).
- [ ] **Step 4: Create `examples/farm/src/manifest.rs`:**
  ```rust
  //! Generate the deployment manifest from the config directory. Mirrors
  //! examples/egg-economy/src/manifest.rs's reference algorithm, with one
  //! difference: farm has no verb/noun split (every entity is plain CRUD,
  //! plus lay_report/hen_productivity's write-side restrictions, which are
  //! an authorization concern — see the retrofit spec — not a documentation
  //! concern), so `api` is emitted whenever a matching
  //! config/json/<entity>.schema.json file exists, with no ALL_VERBS-style
  //! filtering. This also implements the spec's "always advertise both
  //! surfaces" correction: hen_productivity's restlette exists (a worker
  //! calls it) even though FE callers can't write to it, so the manifest
  //! advertises it the same as any other entity.

  use anyhow::Context;
  use serde_json::{json, Map, Value};
  use std::path::{Path, PathBuf};

  pub fn generate(config_dir: &Path) -> anyhow::Result<Value> {
      let mut entities = Map::new();

      let graph_dir = config_dir.join("graph");
      let mut paths: Vec<PathBuf> = std::fs::read_dir(&graph_dir)
          .with_context(|| format!("reading {}", graph_dir.display()))?
          .map(|e| e.map(|e| e.path()))
          .collect::<Result<_, _>>()
          .with_context(|| format!("reading {}", graph_dir.display()))?;
      paths.sort();

      for path in paths {
          if path.extension().and_then(|e| e.to_str()) != Some("graphql") {
              continue;
          }
          let entity = path
              .file_stem()
              .and_then(|s| s.to_str())
              .ok_or_else(|| anyhow::anyhow!("bad graphql filename: {path:?}"))?
              .to_string();
          let graphql = std::fs::read_to_string(&path)
              .with_context(|| format!("reading {}", path.display()))?;

          let mut surfaces = Map::new();
          surfaces.insert(
              "graph".to_string(),
              json!({ "kind": "graphql", "path": format!("/{entity}/graph"), "schema": graphql }),
          );

          let json_schema_path = config_dir.join("json").join(format!("{entity}.schema.json"));
          if json_schema_path.exists() {
              let raw = std::fs::read_to_string(&json_schema_path)
                  .with_context(|| format!("reading {}", json_schema_path.display()))?;
              let json_schema: Value = serde_json::from_str(&raw)
                  .with_context(|| format!("parsing {}", json_schema_path.display()))?;
              surfaces.insert(
                  "api".to_string(),
                  json!({ "kind": "rest", "path": format!("/{entity}/api"), "schema": json_schema }),
              );
          }

          entities.insert(entity, json!({ "surfaces": surfaces }));
      }

      Ok(json!({
          "meshql": 1,
          "entities": entities,
          "surfaces": {}
      }))
  }
  ```
- [ ] **Step 5: Create `examples/farm/src/bin/gen_manifest.rs`:**
  ```rust
  //! Regenerate config/manifest.json. Run from anywhere:
  //!   cargo run -p farm --bin gen_manifest

  use std::path::Path;

  fn main() -> anyhow::Result<()> {
      let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
      let manifest = farm::manifest::generate(&config_dir)?;
      let out = config_dir.join("manifest.json");
      std::fs::write(&out, serde_json::to_string_pretty(&manifest)? + "\n")?;
      println!("wrote {}", out.display());
      Ok(())
  }
  ```
- [ ] **Step 6: Generate `config/manifest.json`.**
  ```bash
  cargo run -p farm --bin gen_manifest
  ```
  Expected output: `wrote .../examples/farm/config/manifest.json`. Inspect it — confirm all 5 entities (`farm`, `coop`, `hen`, `lay_report`, `hen_productivity`) are present, each with both a `graph` and an `api` surface:
  ```bash
  jq '.entities | keys' examples/farm/config/manifest.json
  jq '.entities.hen_productivity.surfaces | keys' examples/farm/config/manifest.json
  ```
  Expected: `["coop", "farm", "hen", "hen_productivity", "lay_report"]` and `["api", "graph"]`.
- [ ] **Step 7: Mount `GET /manifest` in `main.rs`.** Replace `examples/farm/src/main.rs` with:
  ```rust
  use meshql_server::run_ext;

  const MONGO_URI: &str = "mongodb://127.0.0.1:27017";
  const DB_NAME: &str = "farm_db";
  const MANIFEST_JSON: &str = include_str!("../config/manifest.json");

  #[tokio::main]
  async fn main() -> anyhow::Result<()> {
      let (config, extra) = farm::build(MONGO_URI, DB_NAME).await?;
      let extra = extra.route(
          "/manifest",
          axum::routing::get(move || async move {
              (
                  [(axum::http::header::CONTENT_TYPE, "application/json")],
                  MANIFEST_JSON,
              )
          }),
      );
      run_ext(config, extra).await
  }
  ```
- [ ] **Step 8: Run test to verify it passes.**
  ```bash
  cargo build -p farm
  cargo test -p farm --test manifest_conformance
  ```
  Expected: all three tests pass. Re-run the other test files to confirm nothing regressed:
  ```bash
  cargo test -p farm
  ```
- [ ] **Step 9: Commit.**
  ```bash
  git add examples/farm/src/manifest.rs \
          examples/farm/src/bin/gen_manifest.rs \
          examples/farm/config/manifest.json \
          examples/farm/src/main.rs \
          examples/farm/Cargo.toml \
          examples/farm/tests/manifest_conformance.rs
  git commit -m "$(cat <<'EOF'
  Add manifest generator to examples/farm, wire GET /manifest

  Closes farm out to full three-language manifest parity. Mirrors
  egg-economy's reference algorithm, but checks config/json/<entity>
  .schema.json file existence instead of egg-economy's ALL_VERBS split
  — farm has no verb/noun surface filtering, so every entity
  (hen_productivity included) advertises both graph and api surfaces,
  per the retrofit spec's "always advertise both surfaces" correction.

  Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 7: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Build the whole workspace.**
  ```bash
  cargo build --workspace
  ```
  Expected: clean build, no warnings introduced by this plan's changes (`cargo build -p farm`, `cargo build -p meshql-restlette`, and everything downstream should already have been checked per-task, but a full workspace build catches anything that depends on `meshql-restlette`'s changed action strings or `RestletteConfig`/`ServerConfig` — which are unchanged — indirectly).
- [ ] **Step 2: Run the full workspace test suite.**
  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass, including the pre-existing per-backend `farm_cert.rs` suites (`meshql-mongo`, `meshql-sqlite`, `meshql-postgres`, `meshql-mysql`, `meshql-merkql`) — these were confirmed out of scope during planning (`meshql-cert/tests/features/farm.feature` has zero `lay_report` references, so they only exercise `farm`/`coop`/`hen`, unaffected by this retrofit) but must still be re-run to prove that claim. Requires Docker running (Mongo/Postgres/MySQL testcontainers).
- [ ] **Step 3: Confirm the manifest is fresh** (belt-and-suspenders — `manifest_matches_regeneration` in Task 6 already checks this, but re-run explicitly after all other changes):
  ```bash
  cargo run -p farm --bin gen_manifest
  git status --short examples/farm/config/manifest.json
  ```
  Expected: no output from `git status` (the regenerated file is byte-identical to what's committed — if it differs, something in `config/graph/*.graphql` or `config/json/*.schema.json` changed after Task 6 without regenerating).
- [ ] **Step 4: Review the full diff against `main`.**
  ```bash
  git diff main --stat
  ```
  Expected: changes confined to `examples/farm/**`, `meshql-restlette/src/routes.rs`, and `meshql-restlette/tests/verb_granular_auth_cert.rs` — nothing in `meshql-core`, `meshql-server`, `meshql-graphlette`, or any other example.
- [ ] **Step 5: Hand off.** This plan's work is done. Use `superpowers:finishing-a-development-branch` to decide how to integrate (merge, PR, or further review) — do not merge to `main` as part of this plan without that step, and do not push `--force` or skip hooks.

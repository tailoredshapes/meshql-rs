# meshql-changes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `meshql-changes` crate (thin change notifications over SSE, fed by a storage-tailing `ChangeSource`) and the deployment manifest spec (`schemas/manifest.schema.json`), per the approved design in `docs/superpowers/specs/2026-07-07-meshql-changes-design.md`.

**Architecture:** A `ChangeSource` trait (promotion of egg-economy's `EventSource`) with a portable poll-based `SearcherTail` impl that diffs `find_all` output by payload hash and recovers Envelope metadata via point `Repository::read`s. A `ChangeHub` (tokio broadcast) fans events to an axum SSE route that filters per-subscriber by auth tokens. The manifest is a published JSON Schema; deployments serve a conforming static document. No changes to any storage adapter or lette; one small additive helper in `meshql-core`.

**Tech Stack:** Rust 2021, axum 0.7 (`axum::response::sse`), tokio broadcast + tokio-stream, serde_json (default BTreeMap maps → deterministic serialization), jsonschema (dev-only, conformance tests), meshql-sqlite in-memory for certification tests.

**Read the spec first:** `docs/superpowers/specs/2026-07-07-meshql-changes-design.md`. It records the reasoning; this plan records the steps. Where they disagree, stop and flag it.

**Conventions for every task:** TDD (@superpowers:test-driven-development) — write the failing test, watch it fail, implement, watch it pass, commit. Run commands from the repo root `/tank/repos/tailoredshapes/meshql-rs`.

**Key existing signatures you will call (verified against source, do not guess):**

```rust
// meshql-core/src/lib.rs
pub type Stash = serde_json::Map<String, serde_json::Value>;
pub struct Envelope { pub id: String, pub payload: Stash, pub created_at: DateTime<Utc>, pub deleted: bool, pub authorized_tokens: Vec<String> }
impl Envelope { pub fn new(id: impl Into<String>, payload: Stash, tokens: Vec<String>) -> Self } // created_at = now, deleted = false

#[async_trait] pub trait Repository: Send + Sync {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope>;
    async fn read(&self, id: &str, tokens: &[String], at: Option<DateTime<Utc>>) -> Result<Option<Envelope>>;
    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool>;
    // ... list, create_many, read_many, remove_many
}
#[async_trait] pub trait Searcher: Send + Sync {
    async fn find_all(&self, template: &str, args: &Stash, creds: &[String], at: i64) -> Result<Vec<Stash>>;
    // find(...)
}
// meshql-core/src/auth.rs
pub trait Auth: Send + Sync { fn get_auth_token(&self, context: &Stash) -> Vec<String>; /* ... */ }
pub struct AuthContext(pub Stash);            // axum request extension
pub fn envelope_visible_to(envelope: &Envelope, tokens: &[String]) -> bool;
pub struct NoAuth;                            // get_auth_token → vec!["*"]
pub struct StashKeyAuth;                      // StashKeyAuth::new("user_id")

// meshql-sqlite
SqliteRepository::new_with_pool(pool.clone()).await?;
SqliteSearcher::new_with_pool(pool).await?;
```

**sqlite in-memory trap:** each pooled connection to `sqlite::memory:` gets its OWN private database — a default pool (10 connections) means the schema lands on one connection and concurrent access sees empty DBs. Always build the pool the way `meshql-sqlite/tests/searcher_cert.rs` does:

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap().create_if_missing(true);
let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap();
```

**Facts to respect:**
- `find_all` rows are **payload + inserted `"id"` only** — no `created_at`, no tokens. Latest non-deleted version per id. Tombstones invisible.
- An "update" through the restlette is `repo.create(Envelope::new(same_id, merged_payload, tokens))` — a new version.
- serde_json without `preserve_order` (this workspace) uses BTreeMap: `serde_json::to_string` of a Stash is deterministic. Safe to hash.
- `run(config)` / `run_ext(config, extra_router)` in meshql-server; extra routes merge with priority.

---

## Task 1: Manifest JSON Schema

**Files:**
- Create: `schemas/manifest.schema.json`
- Create: `schemas/README.md`

No Rust code — validation tests arrive in Task 2 (they need the egg-economy manifest to validate). Schema first so Task 2 can fail against it meaningfully.

- [ ] **Step 1: Write the schema**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://raw.githubusercontent.com/tailoredshapes/meshql-rs/main/schemas/manifest-v1.schema.json",
  "title": "meshql deployment manifest",
  "description": "Describes a meshql deployment: its entities, their surfaces (GraphQL, REST), and any deployment-level surfaces (SSE change feed, MCP, search, ...). The author declares this document; anything can serve it. Consumers pick the surface kinds they understand and ignore the rest.",
  "type": "object",
  "required": ["meshql", "entities"],
  "properties": {
    "meshql": {
      "description": "Manifest spec major version this document conforms to.",
      "const": 1
    },
    "entities": {
      "type": "object",
      "additionalProperties": {
        "type": "object",
        "required": ["surfaces"],
        "properties": {
          "surfaces": {
            "type": "object",
            "additionalProperties": { "$ref": "#/$defs/surface" }
          }
        }
      }
    },
    "surfaces": {
      "description": "Deployment-level surfaces not tied to one entity (change feed, MCP, search index, ...).",
      "type": "object",
      "additionalProperties": { "$ref": "#/$defs/surface" }
    }
  },
  "$defs": {
    "surface": {
      "type": "object",
      "required": ["kind"],
      "properties": {
        "kind": {
          "description": "Open string. 'graphql', 'rest', and 'sse' are understood by the meshql TS client; unknown kinds pass through for other tooling.",
          "type": "string"
        },
        "path": {
          "description": "HTTP path of the surface, relative to the deployment base URL.",
          "type": "string"
        },
        "schema": {
          "description": "For kind 'graphql': the schema text (string). For kind 'rest': the entity's JSON Schema (object).",
          "oneOf": [{ "type": "string" }, { "type": "object" }]
        }
      },
      "additionalProperties": true
    }
  }
}
```

Note `"meshql": 1` (integer const) — the spec's example showed `"0.x"` as a sketch; the schema pins the real convention: integer major version, breaking changes ship a new `manifest-v2.schema.json` file with a new `$id`.

- [ ] **Step 2: Write `schemas/README.md`**

```markdown
# meshql deployment manifest

A meshql deployment is described by a static JSON document conforming to
`manifest.schema.json`. The document is declared by the deployment author
(it can describe surfaces no single process knows about — MCP servers,
sidecars, search indexes) and served however you like: a `run_ext` static
route, nginx, S3, committed next to `config/`.

Clients (e.g. the meshql TS client) are constructed with a manifest URL.
The `kind` field of each surface is an open string; consumers use the kinds
they understand and ignore the rest. Absence of an `sse` changes surface
tells a client to degrade to refetch-on-dispatch.

Versioning: documents declare `"meshql": 1`. Breaking schema changes ship
as `manifest-v2.schema.json` (new `$id`); this file always points at the
latest via its filename `manifest.schema.json`.

See `examples/egg-economy/config/manifest.json` for a complete example
(generated — see `gen_manifest` in that example) and
`docs/superpowers/specs/2026-07-07-meshql-changes-design.md` for the design.
```

- [ ] **Step 3: Sanity-check the schema parses**

Run: `python3 -c "import json; json.load(open('schemas/manifest.schema.json')); print('ok')"`
Expected: `ok`

- [ ] **Step 4: Commit**

```bash
git add schemas/
git commit -m "feat: deployment manifest JSON Schema (manifest-v1)"
```

---

## Task 2: egg-economy manifest — generator, document, conformance & drift tests

**Files:**
- Create: `examples/egg-economy/src/bin/gen_manifest.rs`
- Create: `examples/egg-economy/config/manifest.json` (generated)
- Create: `examples/egg-economy/tests/manifest_conformance.rs`
- Modify: `examples/egg-economy/Cargo.toml` (add `jsonschema` dev-dep; bins are auto-discovered in `src/bin/`)

The manifest is generated from the example's config files, so drift is impossible to miss: the test regenerates and asserts equality with the committed file. This concretely satisfies the spec's "optional convenience" (we deliberately skip a `manifest_json(&ServerConfig)` helper in meshql-core — YAGNI until a second consumer wants it; the spec marks it optional).

- [ ] **Step 1: Write the failing conformance test**

`examples/egg-economy/tests/manifest_conformance.rs`:

```rust
//! Manifest conformance: the committed manifest validates against the
//! published schema AND matches regeneration from the config files.
//! Drift (a schema file edited without regenerating) breaks this test.

use std::path::Path;

fn crate_dir() -> &'static Path {
    // CARGO_MANIFEST_DIR = examples/egg-economy (the example crate, not the repo root)
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn manifest_validates_against_published_schema() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/manifest.schema.json"
    ))
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
    let generated = egg_economy::manifest::generate(&crate_dir().join("config"))
        .expect("generation succeeds");
    assert_eq!(
        committed, generated,
        "config/manifest.json is stale — regenerate: cargo run -p egg-economy --bin gen_manifest"
    );
}

#[test]
fn every_config_schema_appears_in_manifest() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../config/manifest.json")).expect("manifest parses");
    let entities = manifest["entities"].as_object().expect("entities object");

    for dir_ent in std::fs::read_dir(crate_dir().join("config/graph")).unwrap() {
        let path = dir_ent.unwrap().path();
        let entity = path.file_stem().unwrap().to_str().unwrap().to_string();
        let e = entities
            .get(&entity)
            .unwrap_or_else(|| panic!("entity '{entity}' missing from manifest"));
        assert_eq!(e["surfaces"]["graph"]["kind"], "graphql", "{entity} graph surface");
        if egg_economy::ALL_VERBS.contains(&entity.as_str()) {
            // Verbs are writable event meshes: they have restlettes.
            assert_eq!(e["surfaces"]["api"]["kind"], "rest", "{entity} api surface");
        } else {
            // Nouns are read-only projections: advertising a restlette
            // that 404s is exactly the manifest-honesty failure the spec
            // guards against.
            assert!(
                e["surfaces"].get("api").is_none(),
                "{entity} is a noun and must not advertise a rest surface"
            );
        }
    }
}
```

- [ ] **Step 2: Add dev-dep and lib module wiring**

In `examples/egg-economy/Cargo.toml` under `[dev-dependencies]` add:

```toml
jsonschema = { version = "0.26", default-features = false }
```

(If 0.26's API differs from `jsonschema::validator_for` / `iter_errors`, check the crate docs for the installed version and adapt the test — those functions exist from 0.18 onward.)

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p egg-economy --test manifest_conformance`
Expected: FAIL — `config/manifest.json` doesn't exist / `egg_economy::manifest` unresolved.

- [ ] **Step 4: Implement the generator module**

Add `pub mod manifest;` to `examples/egg-economy/src/lib.rs`, then create `examples/egg-economy/src/manifest.rs`:

```rust
//! Generate the deployment manifest from the config directory.
//! The manifest is a static document (see schemas/manifest.schema.json);
//! this generator is the example's convenience for producing it.

use crate::ALL_VERBS;
use serde_json::{json, Map, Value};
use std::path::Path;

/// Build the manifest document from `config/graph/*.graphql` and
/// `config/json/*.schema.json`. Deterministic: entities sorted by name
/// (serde_json maps are BTreeMaps in this workspace, so key order is
/// sorted on serialization anyway).
///
/// Honesty rule: only VERBS (event meshes) have restlettes in this
/// deployment — nouns (projections: farm, hen, ...) are read-only
/// graphlettes. The manifest must not advertise REST surfaces that 404,
/// so `api` is emitted only for entities in ALL_VERBS. This mirrors the
/// deployment's CQRS shape: front ends write events, never domain models.
pub fn generate(config_dir: &Path) -> anyhow::Result<Value> {
    let mut entities = Map::new();

    for dir_ent in std::fs::read_dir(config_dir.join("graph"))? {
        let path = dir_ent?.path();
        let entity = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("bad graphql filename: {path:?}"))?
            .to_string();
        let graphql = std::fs::read_to_string(&path)?;

        let mut surfaces = Map::new();
        surfaces.insert(
            "graph".to_string(),
            json!({ "kind": "graphql", "path": format!("/{entity}/graph"), "schema": graphql }),
        );
        if ALL_VERBS.contains(&entity.as_str()) {
            let json_schema_path = config_dir.join("json").join(format!("{entity}.schema.json"));
            let json_schema: Value =
                serde_json::from_str(&std::fs::read_to_string(&json_schema_path)?)?;
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
        "surfaces": {
            "changes": { "kind": "sse", "path": "/changes" }
        }
    }))
}
```

(`anyhow` — check `examples/egg-economy/Cargo.toml`; the example already depends on it for `main.rs`. If not, add `anyhow = "1"` to `[dependencies]`.)

- [ ] **Step 5: Implement the generator bin**

`examples/egg-economy/src/bin/gen_manifest.rs`:

```rust
//! Regenerate config/manifest.json. Run from anywhere:
//!   cargo run -p egg-economy --bin gen_manifest

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    let manifest = egg_economy::manifest::generate(&config_dir)?;
    let out = config_dir.join("manifest.json");
    std::fs::write(&out, serde_json::to_string_pretty(&manifest)? + "\n")?;
    println!("wrote {}", out.display());
    Ok(())
}
```

- [ ] **Step 6: Generate the manifest**

Run: `cargo run -p egg-economy --bin gen_manifest`
Expected: `wrote .../examples/egg-economy/config/manifest.json`

- [ ] **Step 7: Run the conformance tests to verify they pass**

Run: `cargo test -p egg-economy --test manifest_conformance`
Expected: 3 passed. If schema validation fails, fix the generator (or schema) — the schema is the contract.

- [ ] **Step 8: Commit**

```bash
git add schemas/ examples/egg-economy/
git commit -m "feat: egg-economy deployment manifest with generator and conformance/drift tests"
```

---

## Task 3: `tokens_visible_to` helper in meshql-core

The SSE filter holds a `ChangeEvent` (token list), not an `&Envelope`. Extract the visibility rule into a token-slice helper that `envelope_visible_to` delegates to, so the logic exists once.

**Files:**
- Modify: `meshql-core/src/auth.rs`
- Modify: `meshql-core/src/lib.rs` (re-export)

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `meshql-core/src/auth.rs`:

```rust
    #[test]
    fn tokens_visible_to_empty_envelope_tokens_is_public() {
        assert!(tokens_visible_to(&[], &["anyone".to_string()]));
        assert!(tokens_visible_to(&[], &[]));
    }

    #[test]
    fn tokens_visible_to_wildcard_caller_sees_everything() {
        assert!(tokens_visible_to(&["secret".to_string()], &["*".to_string()]));
    }

    #[test]
    fn tokens_visible_to_wildcard_envelope_visible_to_any_caller() {
        assert!(tokens_visible_to(&["*".to_string()], &["bob".to_string()]));
    }

    #[test]
    fn tokens_visible_to_requires_overlap_otherwise() {
        assert!(tokens_visible_to(
            &["alice".to_string(), "ops".to_string()],
            &["ops".to_string()]
        ));
        assert!(!tokens_visible_to(&["alice".to_string()], &["bob".to_string()]));
    }

    #[test]
    fn envelope_visible_to_delegates_to_tokens_visible_to() {
        let env = Envelope::new("x", Stash::new(), vec!["alice".to_string()]);
        assert!(envelope_visible_to(&env, &["alice".to_string()]));
        assert!(!envelope_visible_to(&env, &["bob".to_string()]));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p meshql-core tokens_visible_to`
Expected: FAIL — `tokens_visible_to` not found.

- [ ] **Step 3: Implement**

In `meshql-core/src/auth.rs`, add above `envelope_visible_to` and rewrite it to delegate (keep `envelope_visible_to`'s existing doc comment where it is):

```rust
/// Token-slice form of the visibility rule — the single place the
/// convention is implemented. `envelope_visible_to` delegates here; use
/// this directly when you have a token list but no `Envelope` (e.g. the
/// meshql-changes SSE filter).
pub fn tokens_visible_to(envelope_tokens: &[String], caller: &[String]) -> bool {
    if envelope_tokens.is_empty() {
        return true;
    }
    if caller.iter().any(|t| t == "*") {
        return true;
    }
    if envelope_tokens.iter().any(|t| t == "*") {
        return true;
    }
    envelope_tokens.iter().any(|t| caller.iter().any(|c| c == t))
}

pub fn envelope_visible_to(envelope: &Envelope, tokens: &[String]) -> bool {
    tokens_visible_to(&envelope.authorized_tokens, tokens)
}
```

In `meshql-core/src/lib.rs` line 6, extend the re-export:

```rust
pub use auth::{envelope_visible_to, tokens_visible_to, Auth, AuthContext, NoAuth, StashKeyAuth};
```

- [ ] **Step 4: Run tests to verify pass (plus no regression)**

Run: `cargo test -p meshql-core`
Expected: all pass, including the five new tests.

- [ ] **Step 5: Commit**

```bash
git add meshql-core/
git commit -m "feat(core): extract tokens_visible_to; envelope_visible_to delegates"
```

---

## Task 4: `meshql-changes` crate scaffold — `ChangeEvent` + `ChangeSource`

**Files:**
- Modify: `Cargo.toml` (workspace members — add `"meshql-changes"` after `"meshql-merksql"`)
- Create: `meshql-changes/Cargo.toml`
- Create: `meshql-changes/src/lib.rs`
- Create: `meshql-changes/src/event.rs`
- Create: `meshql-changes/src/source.rs`

- [ ] **Step 1: Create the crate**

`meshql-changes/Cargo.toml`:

```toml
[package]
name = "meshql-changes"
version = "0.1.0"
edition = "2021"
description = "meshql-changes — thin change notifications over SSE for meshql deployments"
license = "MIT"
repository = "https://github.com/tailoredshapes/meshql-rs"

[dependencies]
meshql-core = { version = "0.1.0", path = "../meshql-core" }
axum = { workspace = true }
tokio = { workspace = true }
tokio-stream = { version = "0.1", features = ["sync"] }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
async-trait = { workspace = true }
anyhow = "1"

[dev-dependencies]
meshql-sqlite = { version = "0.1.0", path = "../meshql-sqlite" }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
futures = "0.3"
```

Add `"meshql-changes",` to the root `Cargo.toml` `[workspace] members` list.

- [ ] **Step 2: Write the failing wire-format test**

`meshql-changes/src/event.rs`:

```rust
use serde::Serialize;

/// A thin change notification: something about `entity`/`id` changed at
/// `created_at` (epoch millis, the store's commit time). `authorized_tokens`
/// ride along for per-subscriber filtering and are NEVER serialized to the
/// wire — see `wire_json`.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
    pub authorized_tokens: Vec<String>,
}

#[derive(Serialize)]
struct WireEvent<'a> {
    entity: &'a str,
    id: &'a str,
    created_at: i64,
    deleted: bool,
}

impl ChangeEvent {
    /// The SSE `data:` payload. Tokens are stripped by construction — the
    /// wire struct has no field for them.
    pub fn wire_json(&self) -> String {
        serde_json::to_string(&WireEvent {
            entity: &self.entity,
            id: &self.id,
            created_at: self.created_at,
            deleted: self.deleted,
        })
        .expect("WireEvent is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: "abc-123".into(),
            created_at: 1751892345123,
            deleted: false,
            authorized_tokens: vec!["secret-team".into()],
        }
    }

    #[test]
    fn wire_json_contains_the_thin_fields() {
        let v: serde_json::Value = serde_json::from_str(&event().wire_json()).unwrap();
        assert_eq!(v["entity"], "hen");
        assert_eq!(v["id"], "abc-123");
        assert_eq!(v["created_at"], 1751892345123i64);
        assert_eq!(v["deleted"], false);
    }

    #[test]
    fn wire_json_never_leaks_tokens() {
        let wire = event().wire_json();
        assert!(!wire.contains("secret-team"));
        assert!(!wire.contains("authorized_tokens"));
    }
}
```

- [ ] **Step 3: Write the trait and lib.rs**

`meshql-changes/src/source.rs`:

```rust
use crate::ChangeEvent;
use async_trait::async_trait;

/// Something that observes committed writes for one entity and yields them
/// as change events. Promotion of egg-economy's `EventSource` (see
/// examples/egg-economy/src/source.rs for the CDC rationale: derive from
/// the committed store, never the request path — no dual write).
///
/// Delivery contract: at-least-once, per-entity ordered by `created_at`.
/// Consumers tolerate duplicates because the client response is an
/// idempotent refetch.
#[async_trait]
pub trait ChangeSource: Send + Sync {
    /// The entity this source tails (e.g. "hen").
    fn entity(&self) -> &str;
    /// Changes committed since the last poll.
    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>>;
}
```

`meshql-changes/src/lib.rs`:

```rust
//! meshql-changes: thin change notifications for meshql deployments.
//!
//! A `ChangeSource` observes committed writes at the storage layer (CDC
//! model). `SearcherTail` is the portable, poll-based source that works
//! against any certified `Searcher`+`Repository` pair; native change-stream
//! sources slot in behind the same trait. A `ChangeHub` broadcasts events
//! to an SSE route (`changes_router`) that filters per subscriber by auth
//! tokens. Clients respond to notifications by refetching through the
//! normal graphlette — reads never bypass GraphQL.
//!
//! Design: docs/superpowers/specs/2026-07-07-meshql-changes-design.md

mod event;
mod source;

pub use event::ChangeEvent;
pub use source::ChangeSource;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p meshql-changes`
Expected: 2 passed (`wire_json_contains_the_thin_fields`, `wire_json_never_leaks_tokens`).

- [ ] **Step 5: Verify the workspace still builds whole**

Run: `cargo check --workspace`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml meshql-changes/
git commit -m "feat(changes): meshql-changes crate with ChangeEvent and ChangeSource trait"
```

---

## Task 5: `ChangeSource` certification suite

The reusable behavior contract (invariant 5): any `ChangeSource` impl must pass these. Library code in `meshql_changes::testing`; exercised by Task 6's tests.

**Files:**
- Create: `meshql-changes/src/testing.rs`
- Modify: `meshql-changes/src/lib.rs` (add `pub mod testing;`)

- [ ] **Step 1: Write the suite**

`meshql-changes/src/testing.rs`:

```rust
//! Certification suite for `ChangeSource` implementations (invariant 5:
//! storage is pluggable, behavior is certified). Drive writes through the
//! provided `Repository`; assert the source under test emits the right
//! events. Every `ChangeSource` impl must pass all of these before merging.

use crate::{ChangeEvent, ChangeSource};
use meshql_core::{Envelope, Repository, Stash};
use serde_json::json;
use std::time::Duration;

fn wildcard() -> Vec<String> {
    vec!["*".to_string()]
}

fn payload(name: &str) -> Stash {
    let mut s = Stash::new();
    s.insert("name".to_string(), json!(name));
    s
}

async fn drain(source: &dyn ChangeSource) -> Vec<ChangeEvent> {
    source.poll().await.expect("poll succeeds")
}

/// A create is emitted with the envelope's commit time and tokens.
pub async fn test_detects_create(source: &dyn ChangeSource, repo: &dyn Repository) {
    drain(source).await; // settle any pre-existing state

    let env = repo
        .create(
            Envelope::new(uuid(), payload("henrietta"), vec!["farm-team".to_string()]),
            &wildcard(),
        )
        .await
        .expect("create");

    let events = drain(source).await;
    let ev = events
        .iter()
        .find(|e| e.id == env.id)
        .expect("create event emitted");
    assert!(!ev.deleted);
    assert_eq!(ev.entity, source.entity());
    assert_eq!(ev.created_at, env.created_at.timestamp_millis());
    assert_eq!(ev.authorized_tokens, vec!["farm-team".to_string()]);
}

/// An update (new version, same id, changed payload) is emitted.
pub async fn test_detects_update(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = uuid();
    repo.create(Envelope::new(id.clone(), payload("v1"), vec![]), &wildcard())
        .await
        .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await; // distinct commit ms
    let v2 = repo
        .create(Envelope::new(id.clone(), payload("v2"), vec![]), &wildcard())
        .await
        .expect("update-as-new-version");

    let events = drain(source).await;
    let ev = events.iter().find(|e| e.id == id).expect("update event emitted");
    assert!(!ev.deleted);
    assert_eq!(ev.created_at, v2.created_at.timestamp_millis());
}

/// A byte-identical rewrite produces no observable change: no event.
pub async fn test_ignores_identical_rewrite(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = uuid();
    repo.create(Envelope::new(id.clone(), payload("same"), vec![]), &wildcard())
        .await
        .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    repo.create(Envelope::new(id.clone(), payload("same"), vec![]), &wildcard())
        .await
        .expect("rewrite");

    let events = drain(source).await;
    assert!(
        events.iter().all(|e| e.id != id),
        "identical payload must not emit"
    );
}

/// A delete is emitted as deleted=true carrying the last-known tokens.
pub async fn test_detects_delete(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = uuid();
    repo.create(
        Envelope::new(id.clone(), payload("doomed"), vec!["farm-team".to_string()]),
        &wildcard(),
    )
    .await
    .expect("create");
    drain(source).await;

    assert!(repo.remove(&id, &wildcard()).await.expect("remove"));

    let events = drain(source).await;
    let ev = events.iter().find(|e| e.id == id).expect("delete event emitted");
    assert!(ev.deleted);
    assert_eq!(ev.authorized_tokens, vec!["farm-team".to_string()]);
}

/// Create+update+delete strictly between polls collapses to a delete.
pub async fn test_update_then_delete_between_polls(
    source: &dyn ChangeSource,
    repo: &dyn Repository,
) {
    let id = uuid();
    repo.create(Envelope::new(id.clone(), payload("v1"), vec![]), &wildcard())
        .await
        .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    repo.create(Envelope::new(id.clone(), payload("v2"), vec![]), &wildcard())
        .await
        .expect("update");
    assert!(repo.remove(&id, &wildcard()).await.expect("remove"));

    let events = drain(source).await;
    let for_id: Vec<_> = events.iter().filter(|e| e.id == id).collect();
    assert!(
        for_id.iter().any(|e| e.deleted),
        "a delete must be emitted; got {for_id:?}"
    );
}

/// A quiet store emits nothing (poll idempotence).
pub async fn test_quiet_store_emits_nothing(source: &dyn ChangeSource, repo: &dyn Repository) {
    repo.create(Envelope::new(uuid(), payload("steady"), vec![]), &wildcard())
        .await
        .expect("create");
    drain(source).await;

    let events = drain(source).await;
    assert!(events.is_empty(), "no writes → no events; got {events:?}");
}

fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("cert-{}", N.fetch_add(1, Ordering::Relaxed))
}
```


- [ ] **Step 2: Wire into lib.rs**

Add to `meshql-changes/src/lib.rs`:

```rust
pub mod testing;
```

- [ ] **Step 3: Verify it compiles (no runners yet — Task 6 exercises it)**

Run: `cargo check -p meshql-changes`
Expected: clean (warnings about unused functions are fine at this stage).

- [ ] **Step 4: Commit**

```bash
git add meshql-changes/
git commit -m "feat(changes): ChangeSource certification suite"
```

---

## Task 6: `SearcherTail` — portable poll-based source, certified against sqlite

**Files:**
- Create: `meshql-changes/src/tail.rs`
- Create: `meshql-changes/tests/searcher_tail_cert.rs`
- Modify: `meshql-changes/src/lib.rs`

- [ ] **Step 1: Write the failing cert runner**

`meshql-changes/tests/searcher_tail_cert.rs`:

```rust
//! SearcherTail certification against in-memory sqlite. The repo and
//! searcher MUST share one pool — separate `sqlite::memory:` connections
//! are separate databases.

use meshql_changes::testing as cert;
use meshql_changes::SearcherTail;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use std::str::FromStr;
use std::sync::Arc;

async fn setup() -> (SearcherTail, Arc<SqliteRepository>) {
    // max_connections(1): each sqlite::memory: connection is its own DB.
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher = Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());
    let tail = SearcherTail::new("hen", searcher, repo.clone());
    (tail, repo)
}

#[tokio::test]
async fn detects_create() {
    let (tail, repo) = setup().await;
    cert::test_detects_create(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn detects_update() {
    let (tail, repo) = setup().await;
    cert::test_detects_update(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn ignores_identical_rewrite() {
    let (tail, repo) = setup().await;
    cert::test_ignores_identical_rewrite(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn detects_delete() {
    let (tail, repo) = setup().await;
    cert::test_detects_delete(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn update_then_delete_between_polls() {
    let (tail, repo) = setup().await;
    cert::test_update_then_delete_between_polls(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn quiet_store_emits_nothing() {
    let (tail, repo) = setup().await;
    cert::test_quiet_store_emits_nothing(&tail, repo.as_ref()).await;
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p meshql-changes --test searcher_tail_cert`
Expected: FAIL to compile — `SearcherTail` not found.

- [ ] **Step 3: Implement `SearcherTail`**

`meshql-changes/src/tail.rs`:

```rust
//! The portable, poll-based ChangeSource: one `find_all` per poll, diffed
//! against kept state by payload hash. Works against any certified
//! Searcher+Repository pair.
//!
//! Why payload hash: `find_all` rows are payload + `"id"` only — no
//! Envelope metadata. Commit time and tokens are recovered by a point
//! `Repository::read` per *changed* envelope (a handful per poll, not
//! N+1 over the table).
//!
//! Backend caveat (see spec): the `["*"]` poll relies on searchers letting
//! a wildcard caller see everything. All backends except Mongo currently
//! do; on Mongo this tail is correct only under NoAuth until the adapter
//! aligns with the meshql-core convention.

use crate::{ChangeEvent, ChangeSource};
use async_trait::async_trait;
use meshql_core::{Repository, Searcher, Stash};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

struct Known {
    payload_hash: u64,
    tokens: Vec<String>,
}

pub struct SearcherTail {
    entity: String,
    searcher: Arc<dyn Searcher>,
    repository: Arc<dyn Repository>,
    state: tokio::sync::Mutex<HashMap<String, Known>>,
}

impl SearcherTail {
    pub fn new(
        entity: impl Into<String>,
        searcher: Arc<dyn Searcher>,
        repository: Arc<dyn Repository>,
    ) -> Self {
        Self {
            entity: entity.into(),
            searcher,
            repository,
            state: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Deterministic: serde_json Stash is a BTreeMap in this workspace, so
    /// serialization order is sorted. In-process hash only — never persisted.
    fn hash_row(row: &Stash) -> u64 {
        let mut h = DefaultHasher::new();
        serde_json::to_string(row)
            .expect("Stash is always serializable")
            .hash(&mut h);
        h.finish()
    }

    /// Point-read the envelope to recover commit time + tokens, and emit.
    /// If the envelope vanished between find_all and this read (delete
    /// race), emit a delete with last-known tokens instead.
    async fn emit_changed(
        &self,
        id: &str,
        last_known_tokens: Option<Vec<String>>,
        now_ms: i64,
        state: &mut HashMap<String, Known>,
        row_hash: u64,
        out: &mut Vec<ChangeEvent>,
    ) -> anyhow::Result<()> {
        let wildcard = ["*".to_string()];
        match self
            .repository
            .read(id, &wildcard, None)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            Some(env) => {
                state.insert(
                    id.to_string(),
                    Known {
                        payload_hash: row_hash,
                        tokens: env.authorized_tokens.clone(),
                    },
                );
                out.push(ChangeEvent {
                    entity: self.entity.clone(),
                    id: id.to_string(),
                    created_at: env.created_at.timestamp_millis(),
                    deleted: false,
                    authorized_tokens: env.authorized_tokens,
                });
            }
            None => {
                // Deleted between the list and the read.
                state.remove(id);
                out.push(ChangeEvent {
                    entity: self.entity.clone(),
                    id: id.to_string(),
                    created_at: now_ms,
                    deleted: true,
                    authorized_tokens: last_known_tokens.unwrap_or_default(),
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ChangeSource for SearcherTail {
    fn entity(&self) -> &str {
        &self.entity
    }

    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let rows: Vec<Stash> = self
            .searcher
            .find_all("{}", &Stash::new(), &["*".to_string()], now_ms)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut state = self.state.lock().await;
        let mut out = Vec::new();
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();

        for row in &rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()).map(String::from) else {
                continue;
            };
            present.insert(id.clone());
            let row_hash = Self::hash_row(row);
            match state.get(&id) {
                None => {
                    self.emit_changed(&id, None, now_ms, &mut state, row_hash, &mut out)
                        .await?;
                }
                Some(known) if known.payload_hash != row_hash => {
                    let last = known.tokens.clone();
                    self.emit_changed(&id, Some(last), now_ms, &mut state, row_hash, &mut out)
                        .await?;
                }
                Some(_) => {} // unchanged
            }
        }

        // Disappearances are deletes (tombstones are invisible to find_all).
        let gone: Vec<String> = state
            .keys()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            let known = state.remove(&id).expect("key just listed");
            out.push(ChangeEvent {
                entity: self.entity.clone(),
                id,
                created_at: now_ms,
                deleted: true,
                authorized_tokens: known.tokens,
            });
        }

        Ok(out)
    }
}
```

Add to `meshql-changes/src/lib.rs`:

```rust
mod tail;
pub use tail::SearcherTail;
```

- [ ] **Step 4: Run the cert to verify pass**

Run: `cargo test -p meshql-changes --test searcher_tail_cert`
Expected: 6 passed.

**Note on a likely failure:** `test_detects_update` asserts the event's `created_at` equals the *new* version's commit millis. If sqlite's `find_all` (latest-version windowing) or `Repository::read` behaves unexpectedly here, debug with @superpowers:systematic-debugging — do not weaken the assertion; it is the spec's metadata-recovery contract.

- [ ] **Step 5: Commit**

```bash
git add meshql-changes/
git commit -m "feat(changes): SearcherTail passes ChangeSource certification on sqlite"
```

---

## Task 7: `ChangeHub` + `run_tails`

**Files:**
- Create: `meshql-changes/src/hub.rs`
- Modify: `meshql-changes/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

In `meshql-changes/src/hub.rs` (tests inline at bottom, shown with the implementation in Step 2 — write the test module FIRST, watch it fail to compile, then add the implementation above it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChangeEvent, ChangeSource};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn ev(id: &str) -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: id.into(),
            created_at: 1,
            deleted: false,
            authorized_tokens: vec![],
        }
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        hub.publish(ev("a"));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.id, "a");
    }

    /// A scripted source: emits one event on its Nth poll, then nothing.
    struct OneShot {
        polls: AtomicUsize,
    }

    #[async_trait]
    impl ChangeSource for OneShot {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            match self.polls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(vec![ev("from-tail")]),
                1 => Err(anyhow::anyhow!("transient poll failure")),
                _ => Ok(vec![]),
            }
        }
    }

    #[tokio::test]
    async fn run_tails_pumps_sources_into_hub_and_survives_errors() {
        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = std::sync::Arc::new(OneShot {
            polls: AtomicUsize::new(0),
        });

        let handle = tokio::spawn(run_tails(
            hub.clone(),
            vec![source.clone()],
            Duration::from_millis(10),
        ));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event within 2s")
            .unwrap();
        assert_eq!(got.id, "from-tail");

        // Give the loop time to hit the error poll and keep going.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(source.polls.load(Ordering::SeqCst) >= 3, "loop survived the error");
        handle.abort();
    }
}
```

- [ ] **Step 2: Run to verify failure, then implement above the test module**

Run: `cargo test -p meshql-changes hub` → FAIL to compile (`ChangeHub` not found).

Implementation (top of `meshql-changes/src/hub.rs`):

```rust
//! ChangeHub: fan change events out to SSE subscribers. A thin wrapper
//! over tokio::sync::broadcast. `run_tails` is the pump: poll every
//! source round-robin and publish — the shape of egg-economy's
//! `run_connector`. Poll errors are logged and retried next interval,
//! never fatal.

use crate::{ChangeEvent, ChangeSource};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct ChangeHub {
    tx: broadcast::Sender<ChangeEvent>,
}

impl ChangeHub {
    /// `capacity` is the per-subscriber buffer; a subscriber that falls
    /// more than `capacity` events behind is lagged and gets its stream
    /// closed by the SSE layer (correctness over continuity).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: ChangeEvent) {
        // Err means no subscribers — not an error for a notification hub.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.tx.subscribe()
    }
}

/// Poll every source and publish new events, forever. Spawn this.
pub async fn run_tails(hub: ChangeHub, sources: Vec<Arc<dyn ChangeSource>>, interval: Duration) {
    loop {
        for source in &sources {
            match source.poll().await {
                Ok(events) => {
                    for event in events {
                        hub.publish(event);
                    }
                }
                Err(e) => eprintln!("[changes {}] poll: {e}", source.entity()),
            }
        }
        tokio::time::sleep(interval).await;
    }
}
```

Add to `meshql-changes/src/lib.rs`:

```rust
mod hub;
pub use hub::{run_tails, ChangeHub};
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p meshql-changes`
Expected: all pass (event, cert, hub tests).

- [ ] **Step 4: Commit**

```bash
git add meshql-changes/
git commit -m "feat(changes): ChangeHub broadcast and run_tails pump"
```

---

## Task 8: SSE route — `change_stream` + `changes_router`

The stream logic is a pure(ish) function over a broadcast receiver so lag-closure and filtering are unit-testable without HTTP; the router is a thin axum shell over it.

**Files:**
- Create: `meshql-changes/src/sse.rs`
- Create: `meshql-changes/tests/sse_integration.rs`
- Modify: `meshql-changes/src/lib.rs`

- [ ] **Step 1: Write failing unit tests for `change_stream`**

Test module at the bottom of `meshql-changes/src/sse.rs` (write first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeEvent;
    use tokio_stream::StreamExt;

    fn ev(entity: &str, id: &str, tokens: &[&str]) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(),
            id: id.into(),
            created_at: 42,
            deleted: false,
            authorized_tokens: tokens.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn delivers_visible_events_and_filters_invisible() {
        let hub = ChangeHub::new(16);
        let stream = change_stream(hub.subscribe(), vec!["farm-team".into()], None);
        tokio::pin!(stream);

        hub.publish(ev("hen", "visible", &["farm-team"]));
        hub.publish(ev("hen", "hidden", &["other-team"]));
        hub.publish(ev("hen", "public", &[]));

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        let texts = format!("{first:?}{second:?}");
        assert!(texts.contains("visible"));
        assert!(texts.contains("public"));
        assert!(!texts.contains("hidden"));
    }

    #[tokio::test]
    async fn entity_filter_drops_other_entities() {
        let hub = ChangeHub::new(16);
        let wanted: std::collections::HashSet<String> = ["hen".to_string()].into();
        let stream = change_stream(hub.subscribe(), vec!["*".into()], Some(wanted));
        tokio::pin!(stream);

        hub.publish(ev("farm", "nope", &[]));
        hub.publish(ev("hen", "yep", &[]));

        let first = stream.next().await.unwrap().unwrap();
        assert!(format!("{first:?}").contains("yep"));
    }

    #[tokio::test]
    async fn lagged_subscriber_stream_closes() {
        let hub = ChangeHub::new(2); // tiny buffer
        let rx = hub.subscribe();
        for i in 0..10 {
            hub.publish(ev("hen", &format!("e{i}"), &[]));
        }
        let stream = change_stream(rx, vec!["*".into()], None);
        tokio::pin!(stream);

        // Drain whatever survives; the stream must END (None), not hang.
        let mut n = 0;
        while let Some(item) = stream.next().await {
            assert!(item.is_ok());
            n += 1;
            assert!(n < 10, "expected lag closure before all 10");
        }
        // reaching here = stream closed. Buffer is 2, so at most 2 delivered.
        assert!(n <= 2);
    }
}
```

- [ ] **Step 2: Run to verify compile failure**

Run: `cargo test -p meshql-changes sse`
Expected: FAIL — `change_stream` not found.

- [ ] **Step 3: Implement `sse.rs`**

```rust
//! The SSE surface: GET /changes streams thin change notifications.
//!
//! - `event: change`, `id:` = the notification's created_at millis,
//!   `data:` = ChangeEvent::wire_json() (tokens stripped by construction).
//! - Per-subscriber filtering with the same token rule as the lettes
//!   (meshql_core::tokens_visible_to); tokens are captured once at connect.
//! - Reconnect contract: no replay. The hub is in-memory; on (re)connect a
//!   client must treat all cached state as stale. Last-Event-ID is ignored
//!   in v1 (a log-backed source may honor it later).
//! - Lag: a subscriber that overruns the broadcast buffer gets its stream
//!   CLOSED (never silent drops), forcing the reconnect-refetch path.

use crate::{ChangeEvent, ChangeHub};
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Extension, Router};
use meshql_core::{tokens_visible_to, Auth, AuthContext};
use serde::Deserialize;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// The filtered notification stream for one subscriber. Ends (closes the
/// SSE connection) on broadcast lag.
pub fn change_stream(
    rx: tokio::sync::broadcast::Receiver<ChangeEvent>,
    subscriber_tokens: Vec<String>,
    entities: Option<HashSet<String>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx)
        .take_while(|item| !matches!(item, Err(BroadcastStreamRecvError::Lagged(_))))
        .filter_map(move |item| {
            let ev = item.expect("non-lag items are Ok");
            if let Some(wanted) = &entities {
                if !wanted.contains(&ev.entity) {
                    return None;
                }
            }
            if !tokens_visible_to(&ev.authorized_tokens, &subscriber_tokens) {
                return None;
            }
            Some(Ok(Event::default()
                .event("change")
                .id(ev.created_at.to_string())
                .data(ev.wire_json())))
        })
}

#[derive(Clone)]
struct SseState {
    hub: ChangeHub,
    auth: Arc<dyn Auth>,
}

#[derive(Deserialize)]
struct ChangesParams {
    entities: Option<String>,
}

/// Build the /changes router. Merge into a deployment via `run_ext`
/// (in-process form) or serve from a standalone sidecar binary attached to
/// the same storage — same code, two deployment weights.
///
/// Pass the SAME `Arc<dyn Auth>` you pass to `build_app_with_auth` so the
/// stream and the lettes agree on caller identity.
pub fn changes_router(path: &str, hub: ChangeHub, auth: Arc<dyn Auth>) -> Router {
    Router::new()
        .route(path, get(changes_handler))
        .with_state(SseState { hub, auth })
}

async fn changes_handler(
    State(state): State<SseState>,
    auth_ctx: Option<Extension<AuthContext>>,
    Query(params): Query<ChangesParams>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stash = auth_ctx.map(|e| e.0 .0).unwrap_or_default();
    let tokens = state.auth.get_auth_token(&stash);
    let entities = params.entities.map(|s| {
        s.split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect::<HashSet<_>>()
    });

    Sse::new(change_stream(state.hub.subscribe(), tokens, entities)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}
```

Add to `meshql-changes/src/lib.rs`:

```rust
mod sse;
pub use sse::{change_stream, changes_router};
```

- [ ] **Step 4: Run unit tests to verify pass**

Run: `cargo test -p meshql-changes sse`
Expected: 3 passed.

- [ ] **Step 5: Write the failing end-to-end integration test**

`meshql-changes/tests/sse_integration.rs`:

```rust
//! End-to-end: real sqlite storage, real tail, real axum server, real SSE
//! over HTTP. Auth path mirrors production: edge middleware populates
//! AuthContext from a trusted header; StashKeyAuth extracts identity.

use axum::middleware::{self, Next};
use axum::Router;
use meshql_changes::{changes_router, run_tails, ChangeHub, SearcherTail};
use meshql_core::{Auth, AuthContext, Envelope, Repository, Stash, StashKeyAuth};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

async fn edge_identity(mut req: axum::extract::Request, next: Next) -> axum::response::Response {
    // Trusted-header identity, as production edge middleware would inject.
    let mut stash = Stash::new();
    if let Some(user) = req.headers().get("x-user").and_then(|v| v.to_str().ok()) {
        stash.insert("user".to_string(), json!(user));
    }
    req.extensions_mut().insert(AuthContext::new(stash));
    next.run(req).await
}

struct Server {
    base: String,
    repo: Arc<SqliteRepository>,
}

async fn start(auth: Arc<dyn Auth>) -> Server {
    // max_connections(1): each sqlite::memory: connection is its own DB,
    // and the spawned tail polls concurrently with test-task writes.
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher = Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());

    let hub = ChangeHub::new(64);
    let tail = Arc::new(SearcherTail::new("hen", searcher, repo.clone()));
    tokio::spawn(run_tails(hub.clone(), vec![tail], Duration::from_millis(20)));

    let app: Router = changes_router("/changes", hub, auth).layer(middleware::from_fn(edge_identity));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Server { base: format!("http://{addr}"), repo }
}

fn payload(name: &str) -> Stash {
    let mut s = Stash::new();
    s.insert("name".to_string(), json!(name));
    s
}

/// Read from the SSE body until a COMPLETE `data:` line satisfying `pred`
/// arrives, or time out. Only scans newline-terminated lines — a chunk
/// boundary can split a line mid-JSON, so the unterminated remainder stays
/// in `buf` until its newline arrives.
async fn await_data_line(
    resp: reqwest::Response,
    pred: impl Fn(&str) -> bool,
) -> Result<String, String> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| format!("timeout; buffer so far: {buf}"))?
            .ok_or_else(|| format!("stream ended; buffer: {buf}"))?
            .map_err(|e| e.to_string())?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            if let Some(data) = line.trim_end().strip_prefix("data: ") {
                if pred(data) {
                    return Ok(data.to_string());
                }
            }
        }
    }
}

#[tokio::test]
async fn create_update_delete_notifications_arrive_over_http() {
    let server = start(Arc::new(meshql_core::NoAuth)).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/changes", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    // Create
    let env = server
        .repo
        .create(Envelope::new("hen-1", payload("henrietta"), vec![]), &["*".to_string()])
        .await
        .unwrap();
    let data = await_data_line(resp, |d| d.contains("hen-1")).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["entity"], "hen");
    assert_eq!(v["deleted"], false);
    assert_eq!(v["created_at"], env.created_at.timestamp_millis());

    // Delete → deleted:true notification on a fresh connection
    let resp2 = client
        .get(format!("{}/changes", server.base))
        .send()
        .await
        .unwrap();
    server.repo.remove("hen-1", &["*".to_string()]).await.unwrap();
    let data = await_data_line(resp2, |d| d.contains("hen-1") && d.contains("true"))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["deleted"], true);
}

#[tokio::test]
async fn subscribers_only_see_envelopes_their_tokens_allow() {
    let server = start(Arc::new(StashKeyAuth::new("user"))).await;
    let client = reqwest::Client::new();

    let alice = client
        .get(format!("{}/changes", server.base))
        .header("x-user", "alice")
        .send()
        .await
        .unwrap();
    let bob = client
        .get(format!("{}/changes", server.base))
        .header("x-user", "bob")
        .send()
        .await
        .unwrap();

    // alice-only envelope, then a public marker envelope.
    server
        .repo
        .create(
            Envelope::new("secret-hen", payload("classified"), vec!["alice".to_string()]),
            &["*".to_string()],
        )
        .await
        .unwrap();
    server
        .repo
        .create(Envelope::new("public-hen", payload("open"), vec![]), &["*".to_string()])
        .await
        .unwrap();

    // Alice sees the secret envelope.
    await_data_line(alice, |d| d.contains("secret-hen")).await.unwrap();

    // Bob's FIRST hen event must be the public one — the secret was filtered.
    let bob_first = await_data_line(bob, |d| d.contains("-hen")).await.unwrap();
    assert!(
        bob_first.contains("public-hen"),
        "bob's first event should be public-hen, got: {bob_first}"
    );
}

#[tokio::test]
async fn entities_param_filters_the_stream() {
    let server = start(Arc::new(meshql_core::NoAuth)).await;
    let client = reqwest::Client::new();
    // Subscribe to a different entity than the one the tail feeds.
    let resp = client
        .get(format!("{}/changes?entities=farm", server.base))
        .send()
        .await
        .unwrap();

    server
        .repo
        .create(Envelope::new("hen-x", payload("x"), vec![]), &["*".to_string()])
        .await
        .unwrap();

    // No hen event should arrive; expect timeout.
    let res = await_data_line(resp, |d| d.contains("hen-x")).await;
    assert!(res.is_err(), "hen event leaked through entities=farm filter");
}
```

- [ ] **Step 6: Run integration tests**

Run: `cargo test -p meshql-changes --test sse_integration`
Expected: 3 passed. The `entities_param` test takes ~5s (deliberate timeout). If `create_update_delete...` is flaky on the deleted assertion, the second connection may also receive the create event first — the predicate `contains("true")` guards this; verify before "fixing".

Deliberate omission (do not add): the spec's test list mentions heartbeats. The 15s keep-alive is impractical to await in a test and `KeepAlive` is axum's own machinery — configuring it (Task 8 Step 3) is the extent of our responsibility. Noted here so nobody stalls hunting for the missing case.

- [ ] **Step 7: Full crate + workspace check**

Run: `cargo test -p meshql-changes && cargo check --workspace`
Expected: everything passes.

- [ ] **Step 8: Commit**

```bash
git add meshql-changes/
git commit -m "feat(changes): SSE route with per-subscriber token filtering and lag closure"
```

---

## Task 9: Wire into egg-economy (in-process form) + finish

**Files:**
- Modify: `examples/egg-economy/src/main.rs`
- Modify: `examples/egg-economy/Cargo.toml` (add `meshql-changes` dependency)
- Create: `meshql-changes/README.md`

- [ ] **Step 1: Read `examples/egg-economy/src/main.rs` fully**

Understand how entities/repos/searchers are constructed and where `run(config)` is called (currently line ~300). Note the entity list — every entity gets a tail.

- [ ] **Step 2: Add the dependency**

In `examples/egg-economy/Cargo.toml` `[dependencies]`:

```toml
meshql-changes = { version = "0.1.0", path = "../../meshql-changes" }
```

- [ ] **Step 3: Wire tails + SSE + manifest route**

In `main.rs`, where repos/searchers are already in scope (adapt names to the actual code — the repos/searchers for each entity already exist as variables; reuse them, do not construct new ones):

```rust
use meshql_changes::{changes_router, run_tails, ChangeHub, SearcherTail};

// after all repos/searchers are built, before run():
// NB: tails poll Mongo with ["*"] creds. This works in this example only
// because every write carries the "*" token (NoAuth restlettes + workers
// hardcode vec!["*"]) — MongoSearcher has no wildcard-caller special-case.
// See the spec's backend caveat: SearcherTail on Mongo is NoAuth-only for now.
let hub = ChangeHub::new(256);
let sources: Vec<std::sync::Arc<dyn meshql_changes::ChangeSource>> = vec![
    std::sync::Arc::new(SearcherTail::new("farm", farm_searcher.clone(), farm_repo.clone())),
    // ... one per entity, matching the entity list in this file ...
];
tokio::spawn(run_tails(hub.clone(), sources, std::time::Duration::from_millis(500)));

let manifest = include_str!("../config/manifest.json");
let extra = changes_router("/changes", hub, std::sync::Arc::new(meshql_core::NoAuth))
    .route(
        "/manifest",
        axum::routing::get(move || async move {
            ([(axum::http::header::CONTENT_TYPE, "application/json")], manifest)
        }),
    );

// replace `run(config).await` with:
run_ext(config, extra).await
```

Check the actual imports in main.rs (`meshql_server::run` → also import `run_ext`). If repo/searcher variables were moved into config structs by value, restructure minimally: clone the `Arc`s into locals before building the configs (they are `Arc`s — cloning is cheap and idiomatic here).

**Specifically:** the VERB repos/searchers are consumed inside the `event_defs` loop (moved into `RestletteConfig`/`RepositoryTail`) — clone the `Arc`s inside that loop to collect verb `SearcherTail`s too. Every entity gets a tail: 12 verbs + 8 nouns. The smoke test in Step 5 expects a `build_farm` (verb) event, so verb tails are not optional.

- [ ] **Step 4: Verify it builds and existing example tests still pass**

Run: `cargo check -p egg-economy && cargo test -p egg-economy`
Expected: clean; manifest conformance still green.

- [ ] **Step 5: Manual smoke test**

Facts to respect: the example binds port **5088** (`PORT` env var), requires MongoDB (`MONGO_URI`), and its ONLY write surfaces are the verb restlettes (`/build_farm/api`, `/eggs_laid/api`, ...) — nouns like `farm` are read-only projections materialized by workers. A `farm` change reaches the tail via POST → Mongo → CDC connector → worker fold → projection write, so allow a few seconds. Check `build_farm.schema.json` for the required body fields before POSTing.

```bash
# Mongo must be running (see the example's README / docker usage)
cargo run -p egg-economy &
sleep 3
curl -s http://localhost:5088/manifest | head -c 300; echo
curl -sN http://localhost:5088/changes & CURL_PID=$!
sleep 1
curl -s -X POST http://localhost:5088/build_farm/api -H 'Content-Type: application/json' \
  -d '{...valid per config/json/build_farm.schema.json...}'
sleep 4   # POST → CDC (500ms) → worker (500ms) → tail poll (500ms)
kill $CURL_PID; kill %1
```

Expected: manifest JSON prints; the SSE curl prints an `event: change` block for `"entity":"build_farm"` (the event write) and then `"entity":"farm"` (the worker's projection write) within a few seconds. If Mongo isn't available locally, skip this step and note it — the integration tests in Task 8 already cover the mechanism end-to-end.

- [ ] **Step 6: Write `meshql-changes/README.md`**

Short: what it is (thin notifications, SSE, CDC-model tailing), the 10-line wiring snippet from Step 3, the reconnect contract ("on (re)connect, treat cached state as stale"), the Mongo caveat, link to the spec and to `schemas/README.md`.

- [ ] **Step 7: Full workspace verification**

Run: `cargo test --workspace`
Expected: all green (Docker-backed DB certs skip locally as usual — note any failures that predate this work rather than "fixing" them here).

- [ ] **Step 8: Commit**

```bash
git add meshql-changes/ examples/egg-economy/
git commit -m "feat: egg-economy serves /changes SSE and /manifest; meshql-changes README"
```

---

## Out of scope (do not build)

- GraphQL subscriptions; server-side replay; native change-stream sources (merkql/Mongo/Postgres) — future work behind `ChangeSource`.
- The searcher auth-convention fixes (four backends deviate) — separate tracked task; do not touch adapter crates here.
- The TypeScript client — separate project.
- A `manifest_json(&ServerConfig)` helper in meshql-core — deliberately skipped (YAGNI); the egg-economy generator is the convenience.

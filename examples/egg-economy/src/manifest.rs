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

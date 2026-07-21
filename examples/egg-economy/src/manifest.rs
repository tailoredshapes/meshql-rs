//! Generate the deployment manifest from the config directory.
//! The manifest is a static document (see schemas/manifest.schema.json);
//! this generator is the example's convenience for producing it.

use crate::ALL_VERBS;
use anyhow::Context;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Build the manifest document from `config/graph/*.graphql` and
/// `config/json/*.schema.json`. Deterministic: directory entries are
/// sorted explicitly before iteration (serde_json resolves with
/// `preserve_order` in this workspace, so map key order is insertion
/// order — raw `read_dir` order would vary across filesystems).
///
/// Honesty rule: only VERBS (event meshes) have restlettes in this
/// deployment — nouns (projections: farm, hen, ...) are read-only
/// graphlettes. The manifest must not advertise REST surfaces that 404,
/// so `api` is emitted only for entities in ALL_VERBS. This mirrors the
/// deployment's CQRS shape: front ends write events, never domain models.
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
        if ALL_VERBS.contains(&entity.as_str()) {
            let json_schema_path = config_dir
                .join("json")
                .join(format!("{entity}.schema.json"));
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
        "surfaces": {
            "changes": { "kind": "sse", "path": "/changes" }
        }
    }))
}

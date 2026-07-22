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

        let json_schema_path = config_dir
            .join("json")
            .join(format!("{entity}.schema.json"));
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

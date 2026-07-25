//! Generate the deployment manifest from the config directory.
//! The manifest is a static document (see schemas/manifest.schema.json);
//! this generator is the example's convenience for producing it.

use crate::ALL_VERBS;
use anyhow::Context;
use meshql_changes::StreamletteConfig;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
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
///
/// The same honesty rule governs `streamlettes`. Each one contributes a
/// `stream` surface to its entity:
///
/// ```json
/// { "kind": "sse", "path": "/eggs_laid/stream", "resume": true }
/// ```
///
/// `resume` is read straight off `StreamSource::supports_resume()` — it is
/// never a literal and never a parameter. That matters because `resume` is
/// what a client consults INSTEAD of assuming resume works: only a `Seekable`
/// source can honour a `Last-Event-ID`, so advertising `resume: true` over a
/// `Tail` source would send clients a cursor that silently never resumes.
/// Taking the deployment's own `StreamletteConfig`s (the very values handed to
/// `build_app_with_streams`) rather than a hand-written description is what
/// makes that disagreement unrepresentable.
///
/// Additive, not a replacement: the deployment-level `/changes` feed is a
/// different pump model and keeps its own surface entry.
pub fn generate(config_dir: &Path, streamlettes: &[StreamletteConfig]) -> anyhow::Result<Value> {
    // Indexed by entity so the per-entity loop below can attach a stream
    // surface as it builds each entity — keeping surface insertion order
    // (graph, api, stream) stable for the byte-comparing regeneration test.
    let mut streams: BTreeMap<&str, &StreamletteConfig> = BTreeMap::new();
    for sl in streamlettes {
        if let Some(prev) = streams.insert(sl.entity.as_str(), sl) {
            anyhow::bail!(
                "two streamlettes claim entity '{}': {} and {}",
                sl.entity,
                prev.path,
                sl.path
            );
        }
    }

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
        if let Some(sl) = streams.remove(entity.as_str()) {
            surfaces.insert("stream".to_string(), stream_surface(sl));
        }

        entities.insert(entity, json!({ "surfaces": surfaces }));
    }

    // A streamlette over an entity with no `config/graph` file still gets
    // advertised — the stream genuinely exists, and silently dropping it is
    // the same honesty failure in the other direction. BTreeMap order keeps
    // this deterministic.
    for (entity, sl) in streams {
        let mut surfaces = Map::new();
        surfaces.insert("stream".to_string(), stream_surface(sl));
        entities.insert(entity.to_string(), json!({ "surfaces": surfaces }));
    }

    Ok(json!({
        "meshql": 1,
        "entities": entities,
        "surfaces": {
            "changes": { "kind": "sse", "path": "/changes" }
        }
    }))
}

/// One streamlette's manifest surface.
///
/// `resume` is derived, not declared: `supports_resume()` is the server's own
/// answer to "will a `Last-Event-ID` be honoured here", so routing the manifest
/// through it means the two cannot disagree. Guarded by
/// `tests/manifest_conformance.rs::streamlette_surfaces`.
fn stream_surface(sl: &StreamletteConfig) -> Value {
    json!({
        "kind": "sse",
        "path": sl.path,
        "resume": sl.source.supports_resume(),
    })
}

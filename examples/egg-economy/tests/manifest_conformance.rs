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
fn every_graph_entity_appears_in_manifest() {
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
        seen += 1;
    }
    // Guard against a vacuous pass (empty config dir) and against manifest
    // entities that have no corresponding config/graph file.
    assert_eq!(seen, entities.len(), "manifest entity count != graph file count");
}

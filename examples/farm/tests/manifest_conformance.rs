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
    assert!(
        errors.is_empty(),
        "manifest invalid:\n{}",
        errors.join("\n")
    );
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
        assert_eq!(
            e["surfaces"]["graph"]["kind"], "graphql",
            "{entity} graph surface"
        );
        // Every farm entity has a matching config/json/<entity>.schema.json,
        // so every entity — hen_productivity included — must advertise an
        // api surface too. A missing api surface here is exactly the
        // "restlette exists but manifest hides it" bug the spec corrects.
        assert_eq!(e["surfaces"]["api"]["kind"], "rest", "{entity} api surface");
        seen += 1;
    }
    assert_eq!(
        seen,
        entities.len(),
        "manifest entity count != graph file count"
    );
}

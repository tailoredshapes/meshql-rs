//! Manifest conformance: the committed manifest validates against the
//! published schema AND matches regeneration from the config files.
//! Drift (a schema file edited without regenerating) breaks this test.

use async_trait::async_trait;
use meshql_changes::{ChangeEvent, ChangeSource, SeekableSource, StreamSource, StreamletteConfig};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn crate_dir() -> &'static Path {
    // CARGO_MANIFEST_DIR = examples/egg-economy (the example crate, not the repo root)
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
    // `&[]`: this deployment is Mongo-backed and configures no streamlettes,
    // so the committed manifest carries no per-entity `sse` surface. The
    // streamlette half of the generator is exercised against fixtures in
    // `streamlette_surfaces` below.
    let generated = egg_economy::manifest::generate(&crate_dir().join("config"), &[])
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
        assert_eq!(
            e["surfaces"]["graph"]["kind"], "graphql",
            "{entity} graph surface"
        );
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
    assert_eq!(
        seen,
        entities.len(),
        "manifest entity count != graph file count"
    );
}

/// Streamlette advertisement conformance.
///
/// The manifest's `resume` flag is load-bearing: a client reads it to decide
/// whether to send `Last-Event-ID` at all, rather than assuming resume works.
/// A `Tail` source cannot honour resume, so a manifest that advertises
/// `resume: true` over one turns the whole discovery mechanism into a lie.
///
/// These tests therefore never hand-write the expected manifest — they build
/// `StreamletteConfig`s and cross-check what the generator emitted against
/// `StreamSource::supports_resume()`, which is the authority the manifest has
/// to agree with. A hand-written expectation would only prove the same thing
/// was typed twice.
mod streamlette_surfaces {
    use super::*;

    struct Stub(&'static str);

    #[async_trait]
    impl ChangeSource for Stub {
        fn entity(&self) -> &str {
            self.0
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl SeekableSource for Stub {
        async fn backfill(&self, _cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
        fn cursor_is_valid(&self, _cursor: &str) -> bool {
            true
        }
    }

    fn tail(entity: &'static str) -> StreamletteConfig {
        StreamletteConfig {
            path: format!("/{entity}/stream"),
            entity: entity.to_string(),
            source: StreamSource::Tail {
                source: Arc::new(Stub(entity)),
                poll_interval: Duration::from_millis(500),
            },
            hub_capacity: 256,
        }
    }

    fn seekable(entity: &'static str) -> StreamletteConfig {
        StreamletteConfig {
            path: format!("/{entity}/stream"),
            entity: entity.to_string(),
            source: StreamSource::Seekable {
                source: Arc::new(Stub(entity)),
                poll_interval: Duration::from_millis(50),
            },
            hub_capacity: 1024,
        }
    }

    /// Both variants, over entities that really exist in `config/graph` — so
    /// the fixtures land on real entity entries rather than synthesizing ones.
    fn fixtures() -> Vec<StreamletteConfig> {
        vec![seekable("eggs_laid"), tail("farm")]
    }

    /// Every `sse` surface the generator emitted, as (path, resume).
    fn advertised(manifest: &serde_json::Value) -> HashMap<String, bool> {
        let mut out = HashMap::new();
        let entities = manifest["entities"].as_object().expect("entities object");
        for (entity, body) in entities {
            let surfaces = body["surfaces"].as_object().expect("surfaces object");
            for surface in surfaces.values() {
                if surface["kind"] != "sse" {
                    continue;
                }
                let path = surface["path"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{entity} sse surface has no path"))
                    .to_string();
                let resume = surface["resume"].as_bool().unwrap_or_else(|| {
                    panic!("{entity} sse surface has no boolean `resume` at {path}")
                });
                assert!(
                    out.insert(path.clone(), resume).is_none(),
                    "two sse surfaces advertise {path}"
                );
            }
        }
        out
    }

    fn generate(streamlettes: &[StreamletteConfig]) -> serde_json::Value {
        egg_economy::manifest::generate(&crate_dir().join("config"), streamlettes)
            .expect("generation succeeds")
    }

    /// The load-bearing assertion: nothing may advertise `resume: true` unless
    /// its source really supports resume. Derived from the same configs the
    /// generator was handed, so a generator that hardcoded `true` fails here.
    #[test]
    fn advertised_resume_never_outruns_the_source() {
        let streamlettes = fixtures();
        let manifest = generate(&streamlettes);
        let advertised = advertised(&manifest);

        let mut saw_resumable = false;
        let mut saw_non_resumable = false;
        for cfg in &streamlettes {
            let resume = *advertised
                .get(&cfg.path)
                .unwrap_or_else(|| panic!("{} is configured but not advertised", cfg.path));
            let supported = cfg.source.supports_resume();
            assert_eq!(
                resume, supported,
                "{} advertises resume: {resume} but its source supports_resume() == {supported}",
                cfg.path
            );
            if supported {
                saw_resumable = true;
            } else {
                saw_non_resumable = true;
            }
        }

        // Guard against a vacuous pass: both branches must actually be covered,
        // otherwise "resume: true implies Seekable" is never tested.
        assert!(
            saw_resumable,
            "fixtures must include a Seekable streamlette"
        );
        assert!(
            saw_non_resumable,
            "fixtures must include a Tail streamlette"
        );
        assert_eq!(
            advertised.len(),
            streamlettes.len(),
            "manifest advertises sse surfaces that no streamlette backs"
        );
    }

    /// The per-entity surfaces are additive: the deployment-level `/changes`
    /// feed is a different pump model and is not replaced by them.
    #[test]
    fn deployment_level_changes_surface_survives() {
        let manifest = generate(&fixtures());
        assert_eq!(manifest["surfaces"]["changes"]["kind"], "sse");
        assert_eq!(manifest["surfaces"]["changes"]["path"], "/changes");
    }

    /// The streamlette surfaces attach to the entity they name, alongside the
    /// entity's existing graph/api surfaces rather than displacing them.
    #[test]
    fn streamlette_surfaces_attach_to_their_entity() {
        let manifest = generate(&fixtures());
        let laid = &manifest["entities"]["eggs_laid"]["surfaces"];
        assert_eq!(laid["stream"]["path"], "/eggs_laid/stream");
        assert_eq!(laid["stream"]["resume"], true);
        assert_eq!(laid["graph"]["kind"], "graphql");
        assert_eq!(laid["api"]["kind"], "rest");

        let farm = &manifest["entities"]["farm"]["surfaces"];
        assert_eq!(farm["stream"]["path"], "/farm/stream");
        assert_eq!(farm["stream"]["resume"], false);
        assert_eq!(farm["graph"]["kind"], "graphql");
        assert!(
            farm.get("api").is_none(),
            "farm is a noun and must not gain a rest surface"
        );
    }

    /// `resume` rides on `$defs/surface`'s `additionalProperties: true` — a
    /// manifest carrying streamlette surfaces needs no schema change.
    #[test]
    fn streamlette_manifest_validates_against_published_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/manifest.schema.json"))
                .expect("schema parses");
        let manifest = generate(&fixtures());
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let errors: Vec<String> = validator
            .iter_errors(&manifest)
            .map(|e| format!("{e} at {}", e.instance_path))
            .collect();
        assert!(
            errors.is_empty(),
            "manifest with streamlette surfaces invalid:\n{}",
            errors.join("\n")
        );
    }
}

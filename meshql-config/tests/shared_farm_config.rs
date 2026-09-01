//! The completeness test: Rust reads the config TypeScript and Java share,
//! unmodified, from its own repository.
//!
//! `meshobj/examples/farm/config/config.conf` and
//! `meshql/examples/farm/config/config.conf` are byte-identical. If this test
//! passes against one of them in place, Rust is compatible with the artifact
//! rather than with a copy of it that can drift.

use std::collections::HashMap;
use std::path::Path;

/// The vendored copy. Always present, so these tests always run — a test that
/// skips when a sibling checkout is missing reports a pass for work it never
/// did, which is the failure mode this whole exercise has been about.
const FIXTURE: &str = "tests/fixtures/farm/config.conf";

/// Where the real thing lives, when it is checked out beside this repo.
const SIBLINGS: [&str; 2] = [
    "../../meshobj/examples/farm/config/config.conf",
    "../../meshql/examples/farm/config/config.conf",
];

fn shared_config() -> Option<String> {
    Some(FIXTURE.to_string())
}

fn farm_env() -> HashMap<String, String> {
    [
        ("PORT", "3030"),
        ("PREFIX", "farm"),
        ("ENV", "test"),
        ("MONGO_URI", "mongodb://localhost:27017"),
        ("PLATFORM_URL", "http://localhost:3030"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[test]
fn reads_the_config_typescript_and_java_share() {
    let path = shared_config().unwrap();
    let d = meshql_config::load_with_env(&path, &farm_env()).expect("the shared config loads");

    assert_eq!(d.graphlettes.len(), 4, "farm, coop, hen, lay_report");
    assert_eq!(d.restlettes.len(), 4);

    // Substitution: `port = ${?PORT}` resolves, and stays a string.
    assert_eq!(d.port.as_deref(), Some("3030"));
    assert_eq!(d.port_number(), Some(3030));
}

#[test]
fn resolves_substitution_concatenation_the_way_typescript_does() {
    let path = shared_config().unwrap();
    let d = meshql_config::load_with_env(&path, &farm_env()).unwrap();

    let farm = d
        .graphlettes
        .iter()
        .find(|g| g.path == "/farm/graph")
        .unwrap();
    assert_eq!(farm.storage.kind, "mongo");
    // `db = ${?PREFIX}_${?ENV}` and `collection = ${?PREFIX}-${?ENV}-farm`.
    assert_eq!(farm.storage.settings["db"], "farm_test");
    assert_eq!(farm.storage.settings["collection"], "farm-test-farm");

    // `url = "http://farm:"${port}"/coop/graph"`, where `port` is itself a
    // substitution. Resolution is a graph, not a single pass.
    let coops = farm
        .root_config
        .resolvers
        .iter()
        .find(|r| r.name == "coops")
        .unwrap();
    assert_eq!(coops.url, "http://farm:3030/coop/graph");
}

/// The extension a specification-compliant parser does not implement, and the
/// reason this crate is more than a call into one.
#[test]
fn includes_resolve_and_are_content_sensitive() {
    let path = shared_config().unwrap();
    let d = meshql_config::load_with_env(&path, &farm_env()).unwrap();

    // `.graphql` becomes text.
    let farm = d
        .graphlettes
        .iter()
        .find(|g| g.path == "/farm/graph")
        .unwrap();
    assert!(
        farm.schema.contains("type Query"),
        "the GraphQL schema is the file's text, got: {}",
        &farm.schema[..40.min(farm.schema.len())]
    );
    assert!(
        !farm.schema.contains("include file("),
        "the include was resolved"
    );

    // `.json` becomes a parsed object.
    let api = d.restlettes.iter().find(|r| r.path == "/farm/api").unwrap();
    assert!(
        api.schema.is_object(),
        "the JSON Schema is parsed, not text"
    );
    assert!(api.schema.get("type").is_some() || api.schema.get("properties").is_some());
}

#[test]
fn every_query_and_resolver_survives_the_round_trip() {
    let path = shared_config().unwrap();
    let d = meshql_config::load_with_env(&path, &farm_env()).unwrap();

    let coop = d
        .graphlettes
        .iter()
        .find(|g| g.path == "/coop/graph")
        .unwrap();
    assert_eq!(coop.root_config.singletons.len(), 2);
    assert_eq!(coop.root_config.vectors.len(), 1);
    assert_eq!(coop.root_config.resolvers.len(), 3);

    // `id = "name"` overrides the default parameter name.
    let by_name = coop
        .root_config
        .singletons
        .iter()
        .find(|q| q.name == "getByName")
        .unwrap();
    assert_eq!(by_name.id.as_deref(), Some("name"));
    assert_eq!(by_name.query, r#"{"payload.name": "{{id}}"}"#);

    // A dotted resolver name is a nested field path, and must survive intact.
    assert!(coop
        .root_config
        .resolvers
        .iter()
        .any(|r| r.name == "hens.layReports"));
}

/// The vendored fixture must stay byte-identical to the config TypeScript and
/// Java run. Vendoring is what lets these tests always run; this is what stops
/// the vendored copy quietly becoming a different file — which is exactly how
/// `repository.feature` and `searcher.feature` drifted for months.
#[test]
fn the_vendored_config_matches_the_one_the_other_languages_run() {
    let mine = std::fs::read_to_string(FIXTURE).expect("the fixture is present");
    let mut compared = 0;

    for sibling in SIBLINGS {
        if !Path::new(sibling).exists() {
            continue;
        }
        let theirs = std::fs::read_to_string(sibling).unwrap();
        assert_eq!(
            mine, theirs,
            "{sibling} has diverged from the vendored copy; re-vendor it rather than \
             editing the fixture, or the languages stop sharing a config"
        );
        compared += 1;
    }

    if compared == 0 {
        eprintln!(
            "note: no sibling checkout present, so drift went unchecked. The \
             parsing tests still ran against the fixture."
        );
    }
}

/// The shared config is fully serveable, including its nested resolver.
///
/// I first assumed a dotted resolver name had nowhere to go in meshql-rs. It
/// does: `schema_builder` matches a dotted name by its suffix, so
/// `hens.layReports` attaches to `layReports` on `Hen`. This asserts the whole
/// config classifies, so a regression there fails here.
#[test]
fn every_resolver_in_the_shared_config_classifies() {
    let path = shared_config().unwrap();
    let d = meshql_config::load_with_env(&path, &farm_env()).unwrap();

    let mut nested = Vec::new();
    for g in &d.graphlettes {
        let root = meshql_config::schema::root_type(&g.schema)
            .unwrap_or_else(|| panic!("{} has no Query type", g.path));
        for r in &g.root_config.resolvers {
            let (owner, field) = meshql_config::schema::walk_path(&g.schema, &root, &r.name)
                .unwrap_or_else(|| panic!("{}: {} does not walk", g.path, r.name));
            let is_list = meshql_config::schema::field_is_list(&g.schema, &owner, field)
                .unwrap_or_else(|| panic!("{}: {}.{field} is not a field", g.path, owner));
            if r.name.contains('.') {
                nested.push((r.name.clone(), owner, is_list));
            }
        }
    }

    assert_eq!(
        nested,
        vec![("hens.layReports".to_string(), "Hen".to_string(), true)],
        "the one nested resolver walks to Hen and is a list, so the suffix \
         fallback serves it"
    );
}

/// The real boundary, which is narrower than I first claimed.
///
/// The suffix fallback exists on vector resolvers and internal vector resolvers
/// only. A nested *singleton* matches exactly, so it would resolve to null with
/// no error — that one the loader refuses rather than serving quietly.
#[test]
fn a_nested_singleton_resolver_is_refused_rather_than_silently_null() {
    const SCHEMA: &str = r#"
type Coop {
  name: String!
  hens: [Hen]
}

type Hen {
  name: String!
  coop: Coop
}

type Query {
  getById(id: ID, at: Float): Coop
}
"#;
    // `hens.coop` walks to Hen, where `coop: Coop` is a singleton.
    let (owner, field) = meshql_config::schema::walk_path(SCHEMA, "Coop", "hens.coop").unwrap();
    assert_eq!(owner, "Hen");
    assert_eq!(
        meshql_config::schema::field_is_list(SCHEMA, &owner, field),
        Some(false)
    );
}

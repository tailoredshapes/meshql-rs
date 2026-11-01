use super::*;

const DEPLOYABLE_SCHEMA: &str = r#"
type Deployable {
    id: ID
    name: String!
    description: String
    repo_url: String
    team_id: String
    team: Team
    deployment_status: String
}
type Team {
    id: ID
    name: String
    kind: String
    description: String
}
type Query {
    getById(id: ID, at: Float): Deployable
    getAll(at: Float): [Deployable]
    getByName(name: String, at: Float): [Deployable]
}
"#;

const DEPENDENCY_SCHEMA: &str = r#"
type Dependency {
    id: ID
    deployable_id: String!
    service_id: String!
    protocol: String
    auth_method: String
    criticality: String
}
type Query {
    getById(id: ID, at: Float): Dependency
    getAll(at: Float): [Dependency]
    getByDeployableId(deployable_id: String, at: Float): [Dependency]
    getByServiceId(service_id: String, at: Float): [Dependency]
}
"#;

#[test]
fn parses_deployable_schema_with_federation() {
    let parsed = parse_meshql_schema(DEPLOYABLE_SCHEMA).expect("parse ok");
    assert_eq!(parsed.entity_name, "Deployable");
    assert_eq!(parsed.entity_fields.len(), 7);

    let names: Vec<&str> = parsed
        .entity_fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "id",
            "name",
            "description",
            "repo_url",
            "team_id",
            "team",
            "deployment_status",
        ]
    );

    let team_field = parsed
        .entity_fields
        .iter()
        .find(|f| f.name == "team")
        .expect("team field present");
    assert!(team_field.is_federated, "team should be federated");
    assert_eq!(
        team_field.federation_subselection.as_deref(),
        Some("{ id name kind description }")
    );

    // Scalar fields are not federated.
    let team_id_field = parsed
        .entity_fields
        .iter()
        .find(|f| f.name == "team_id")
        .expect("team_id field present");
    assert!(!team_id_field.is_federated);

    // Three query ops.
    let op_names: Vec<&str> = parsed.query_ops.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(op_names, vec!["getById", "getAll", "getByName"]);

    let get_by_id = parsed
        .query_ops
        .iter()
        .find(|o| o.name == "getById")
        .unwrap();
    assert!(!get_by_id.returns_list);
    assert_eq!(
        get_by_id.args,
        vec![
            ("id".to_string(), "ID".to_string()),
            ("at".to_string(), "Float".to_string()),
        ]
    );

    let get_all = parsed
        .query_ops
        .iter()
        .find(|o| o.name == "getAll")
        .unwrap();
    assert!(get_all.returns_list);
}

#[test]
fn parses_dependency_schema_with_getbyfk() {
    let parsed = parse_meshql_schema(DEPENDENCY_SCHEMA).expect("parse ok");
    assert_eq!(parsed.entity_name, "Dependency");
    // No federation (no other type blocks besides Query).
    assert!(parsed.entity_fields.iter().all(|f| !f.is_federated));

    let by_deployable = parsed
        .query_ops
        .iter()
        .find(|o| o.name == "getByDeployableId")
        .expect("getByDeployableId present");
    assert!(by_deployable.returns_list);
    assert_eq!(by_deployable.args[0].0, "deployable_id");
    assert_eq!(by_deployable.args[0].1, "String");

    let by_service = parsed
        .query_ops
        .iter()
        .find(|o| o.name == "getByServiceId")
        .expect("getByServiceId present");
    assert!(by_service.returns_list);
    assert_eq!(by_service.args[0].0, "service_id");
}

#[test]
fn bails_on_malformed_schema() {
    let garbage = "this is not graphql at all { ;; ";
    assert!(parse_meshql_schema(garbage).is_err());

    let no_brace = "type Deployable id: ID }";
    assert!(parse_meshql_schema(no_brace).is_err());

    let no_close = "type Deployable { id: ID";
    assert!(parse_meshql_schema(no_close).is_err());
}

#[test]
fn renders_field_selection_one_level_federation() {
    let parsed = parse_meshql_schema(DEPLOYABLE_SCHEMA).expect("parse ok");
    let rendered = render_entity_field_selection(&parsed);
    assert_eq!(
        rendered,
        "id name description repo_url team_id team { id name kind description } deployment_status"
    );
}

#[test]
fn strips_line_comments() {
    let schema = r#"
# this is a header comment
type Service {
    id: ID            # the uuid
    name: String!
}
type Query {
    getAll(at: Float): [Service]   # list everything
}
"#;
    let parsed = parse_meshql_schema(schema).expect("parse ok");
    assert_eq!(parsed.entity_name, "Service");
    assert_eq!(parsed.entity_fields.len(), 2);
    assert_eq!(parsed.query_ops.len(), 1);
}

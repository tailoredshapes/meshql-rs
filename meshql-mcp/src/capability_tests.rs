use super::*;
use serde_json::json;
const SDL: &str = "scalar Date\ntype Other { id: ID }\ntype Widget { id: ID name: String! count: Int other: Other }\ntype Query { getAll(at: Float): [Widget] getById(id: ID, at: Float): Widget getByName(name: String): [Widget] getByGroupId(group_id: ID): [Widget] }";
fn schema() -> Value {
    json!({"type":"object","properties":{"name":{"type":"string"},"count":{"type":"integer"}},"required":["name"],"additionalProperties":false})
}
#[test]
fn reads_are_variable_queries_with_optional_time() {
    let caps = CapabilitiesBuilder::new()
        .auto_from_schemas(&[("widget", "/widget/graph", SDL)])
        .build();
    assert_eq!(
        caps.iter().map(|c| c.name).collect::<Vec<_>>(),
        [
            "list_widgets",
            "get_widget_by_id",
            "find_widgets_by_name",
            "widgets_for_group"
        ]
    );
    assert!(caps[0].input_schema["properties"].get("at").is_some());
    assert!(
        matches!(&caps[1].handler, CapabilityHandler::GraphQueryVariables {query,..} if query.contains("id: $id"))
    );
}
#[test]
fn actual_rest_schema_is_required_for_writes() {
    let caps = CapabilitiesBuilder::new()
        .with_rest_schema("widget", "/widget/api", schema())
        .build();
    assert_eq!(caps.len(), 3);
    assert_eq!(caps[0].input_schema["properties"]["payload"], schema());
    assert!(caps[1].input_schema["properties"]["patch"]
        .get("required")
        .is_none());
    assert_eq!(
        caps[1].input_schema["properties"]["patch"]["properties"]["count"]["type"],
        "integer"
    );
}
#[test]
fn compounds_and_unquoted_strings_cannot_inject_legacy_graphql() {
    assert!(substitute(
        "{ getById(id: \"{id}\") { id } }",
        &json!({"id":{" ) { id } stolen: getAll { secret } } #":null}})
    )
    .is_err());
    assert!(substitute(
        "{ getById(id: {id}) { id } }",
        &json!({"id":"x) { secret }"})
    )
    .is_err());
    assert!(substitute(
        r#"{ q(id: "\{id}") { id } }"#,
        &json!({"id":"\") { secret } #"})
    )
    .is_err());
    let query = substitute(
        "{ q(name: \"{name}\") { id } }",
        &json!({"name":"café \"\n"}),
    )
    .unwrap();
    assert!(query.contains("café \\\"\\n"));
}
#[test]
fn path_and_json_templates_preserve_data() {
    let path = substitute_path("/widget/api/{id}", &json!({"id":"../other?q#x"})).unwrap();
    assert!(!path.contains("/../") && !path.contains('?') && !path.contains('#'));
    assert!(substitute_path("/widget/api/{id}", &json!({"id":".."})).is_err());
    assert_eq!(
        substitute_value(&json!({"n":"{n}","s":"pré {s}"}), &json!({"n":3,"s":"é"})).unwrap(),
        json!({"n":3,"s":"pré é"})
    );
}
#[test]
fn invalid_sdl_fails_closed_and_overrides_still_work() {
    assert!(CapabilitiesBuilder::new()
        .auto_from_schemas(&[("bad", "/bad/graph", "bad")])
        .try_build()
        .is_err());
    let caps = CapabilitiesBuilder::new()
        .auto_from_schemas(&[("widget", "/widget/graph", SDL)])
        .describe("list_widgets", "custom")
        .build();
    assert_eq!(caps[0].description, "custom");
}

#[tokio::test]
async fn custom_dispatch_and_existing_builder_overrides_are_preserved() {
    let replacement = Capability {
        name: "list_widgets",
        description: "custom override",
        input_schema: json!({"type":"object"}),
        handler: CapabilityHandler::Custom(Arc::new(|client, args| {
            Box::pin(async move { Ok(json!({"echo":args,"backend":client.base_url()})) })
        })),
    };
    let caps = CapabilitiesBuilder::new()
        .auto_from_schemas(&[("widget", "/widget/graph", SDL)])
        .add(replacement.with_description("custom override final"))
        .describe("missing", "ignored")
        .build();
    assert_eq!(caps.len(), 4);
    let cap = caps.into_iter().find(|c| c.name == "list_widgets").unwrap();
    assert_eq!(cap.description, "custom override final");
    let client = Arc::new(MeshqlClient::new("http://localhost:1"));
    let tool = cap.into_tool(client.clone());
    assert_eq!(
        (tool.handler)(client, json!({"value":7})).await.unwrap(),
        json!({"echo":{"value":7},"backend":"http://localhost:1"})
    );
}

use crate::{
    capability::{Capability, CapabilityHandler},
    schema::{parse_meshql_schema, query_arguments, selection_for_query},
};
use serde_json::{json, Map, Value};
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
fn name(entity: &str, operation: &str, args: &[(String, String)]) -> String {
    match operation {
        "getAll" => format!("list_{entity}s"),
        "getById" => format!("get_{entity}_by_id"),
        "getByName" => format!("find_{entity}s_by_name"),
        op if op.starts_with("getBy") && op.ends_with("Id") => {
            let key = args
                .iter()
                .find(|(n, _)| n != "at")
                .map(|(n, _)| n.trim_end_matches("_id"))
                .unwrap_or("id");
            format!("{entity}s_for_{key}")
        }
        _ => format!("{entity}_{operation}"),
    }
}
pub(crate) fn reads(entity: &str, path: &str, text: &str) -> anyhow::Result<Vec<Capability>> {
    let parsed = parse_meshql_schema(text)?;
    let mut out = Vec::new();
    for op in parsed.query_ops {
        let arguments = query_arguments(text, &op.name)?;
        let (props, required, declarations) = crate::input_schema::arguments(&arguments)?;
        let bindings = op
            .args
            .iter()
            .map(|(n, _)| format!("{n}: ${n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let vars = if declarations.is_empty() {
            String::new()
        } else {
            format!("({declarations})")
        };
        let args = if bindings.is_empty() {
            String::new()
        } else {
            format!("({bindings})")
        };
        let selection = selection_for_query(text, &op.name)?
            .map(|body| format!("{{ {body} }}"))
            .unwrap_or_default();
        let query = format!("query Mcp{vars} {{ {}{args} {selection} }}", op.name);
        out.push(Capability {name:leak(name(entity, &op.name, &op.args)), description:leak(format!("Run {} for {entity}", op.name)),
            input_schema:json!({"type":"object", "properties":props, "required":required, "additionalProperties":false}),
            handler:CapabilityHandler::GraphQueryVariables {path:path.into(), query}});
    }
    Ok(out)
}
fn outer(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}
pub(crate) fn writes(entity: &str, path: &str, schema: Value) -> anyhow::Result<Vec<Capability>> {
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|e| anyhow::anyhow!("invalid REST schema: {e}"))?;
    let mut patch = Map::new();
    for key in [
        "type",
        "properties",
        "additionalProperties",
        "patternProperties",
        "$defs",
        "definitions",
    ] {
        if let Some(v) = schema.get(key) {
            patch.insert(key.into(), v.clone());
        }
    }
    let definitions = [
        (
            format!("create_{entity}"),
            outer(json!({"payload":schema}), &["payload"]),
            CapabilityHandler::EntityCreate {
                api_path: path.into(),
            },
        ),
        (
            format!("update_{entity}"),
            outer(
                json!({"id":{"type":"string"}, "patch":Value::Object(patch)}),
                &["id", "patch"],
            ),
            CapabilityHandler::EntityUpdate {
                api_path: path.into(),
            },
        ),
        (
            format!("delete_{entity}"),
            outer(json!({"id":{"type":"string"}}), &["id"]),
            CapabilityHandler::EntityDelete {
                api_path: path.into(),
            },
        ),
    ];
    Ok(definitions
        .into_iter()
        .map(|(n, input_schema, handler)| Capability {
            name: leak(n),
            description: "Write through the entity REST surface",
            input_schema,
            handler,
        })
        .collect())
}

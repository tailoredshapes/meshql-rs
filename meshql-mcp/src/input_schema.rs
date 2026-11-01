//! GraphQL argument types and defaults retain their SDL semantics.
use anyhow::{bail, Result};
use async_graphql_parser::types as p;
use serde_json::{json, Map, Value};

fn argument_type(ty: &p::Type) -> Result<Value> {
    let value = match &ty.base {
        p::BaseType::List(item) => json!({"type":"array", "items":argument_type(item)?}),
        p::BaseType::Named(name) => match name.as_str() {
            "Int" => json!({"type":"integer", "minimum":i32::MIN, "maximum":i32::MAX}),
            "Float" => json!({"type":"number"}),
            "Boolean" => json!({"type":"boolean"}),
            "String" => json!({"type":"string"}),
            "ID" => json!({"anyOf":[{"type":"string"},{"type":"integer"}]}),
            other => bail!("unsupported GraphQL input type {other}; custom scalar and input object arguments require an explicit capability"),
        },
    };
    Ok(if ty.nullable {
        json!({"anyOf":[value,{"type":"null"}]})
    } else {
        value
    })
}
pub(crate) fn arguments(
    args: &[async_graphql_parser::Positioned<p::InputValueDefinition>],
) -> Result<(Map<String, Value>, Vec<String>, String)> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut declarations = Vec::new();
    for arg in args {
        let arg = &arg.node;
        let name = arg.name.node.to_string();
        properties.insert(name.clone(), argument_type(&arg.ty.node)?);
        if !arg.ty.node.nullable && arg.default_value.is_none() {
            required.push(name.clone());
        }
        let default = arg
            .default_value
            .as_ref()
            .map(|v| format!(" = {}", v.node))
            .unwrap_or_default();
        declarations.push(format!("${name}: {}{default}", arg.ty.node));
    }
    Ok((properties, required, declarations.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nullable_defaults_lists_and_id_preserve_graphql_semantics() {
        let sdl = "type Widget { id: ID! } type Query { search(name: String = \"guest\", limit: Int! = 5, tags: [String], strict: [String!]!, id: ID!): Widget }";
        let args = crate::schema::query_arguments(sdl, "search").unwrap();
        let (properties, required, declarations) = arguments(&args).unwrap();
        let schema = json!({"type":"object","properties":properties,"required":required});
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert_eq!(required, vec!["strict", "id"]);
        assert!(declarations.contains("$name: String = \"guest\""));
        assert!(declarations.contains("$limit: Int! = 5"));
        assert!(validator.is_valid(&json!({"strict":[],"id":42})));
        assert!(validator.is_valid(&json!({"strict":["x"],"id":"a","name":null,"tags":[null,"x"]})));
        assert!(validator.is_valid(&json!({"strict":[],"id":"a","tags":null})));
        assert!(!validator.is_valid(&json!({"strict":[null],"id":"a"})));
        assert!(!validator.is_valid(&json!({"strict":null,"id":"a"})));
        assert!(!validator.is_valid(&json!({"strict":[],"id":"a","limit":null})));
        assert!(!validator.is_valid(&json!({"strict":[],"id":"a","limit":2147483648i64})));
        let tools = crate::catalogue::reads("widget", "/widget/graph", sdl).unwrap();
        let crate::capability::CapabilityHandler::GraphQueryVariables { query, .. } =
            &tools[0].handler
        else {
            panic!("variable handler required")
        };
        assert!(query.contains("$limit: Int! = 5"));
    }
}

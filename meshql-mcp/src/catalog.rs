//! Generic `catalog.list / catalog.get / catalog.search` tools, parameterized
//! over a slice of entity names so each deployment can configure its own
//! catalogue.
//!
//! These are thin wrappers over the meshql REST API so an LLM can interrogate
//! a deployment's catalogue without constructing HTTP calls itself.

use crate::client::MeshqlClient;
use crate::tool::{Tool, ToolFuture};
use serde_json::{json, Value};
use std::sync::Arc;

fn entity_arg(args: &Value, entities: &[&'static str]) -> anyhow::Result<String> {
    let entity = args
        .get("entity")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'entity' argument"))?;
    if !entities.contains(&entity) {
        anyhow::bail!("entity must be one of {:?}, got {:?}", entities, entity);
    }
    Ok(entity.to_string())
}

fn name_of(env: &Value) -> String {
    if let Some(p) = env.get("payload").and_then(|p| p.as_object()) {
        if let Some(s) = p.get("name").and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    env.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Build the three catalogue tools (`catalog.list`, `catalog.get`,
/// `catalog.search`), with each handler validating its `entity` argument
/// against the supplied list.
pub fn tools(entities: &[&'static str]) -> Vec<Tool> {
    let entity_enum: Vec<Value> = entities.iter().map(|e| json!(e)).collect();

    let entities_for_list = entities.to_vec();
    let list_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_list.clone();
            Box::pin(async move {
                let entity = entity_arg(&args, &entities)?;
                client.list(&entity).await
            })
        },
    );

    let entities_for_get = entities.to_vec();
    let get_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_get.clone();
            Box::pin(async move {
                let entity = entity_arg(&args, &entities)?;
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing 'id' argument"))?
                    .to_string();
                match client.get(&entity, &id).await? {
                    Some(v) => Ok(v),
                    None => Ok(Value::Null),
                }
            })
        },
    );

    let entities_for_search = entities.to_vec();
    let search_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_search.clone();
            Box::pin(async move {
                let entity = entity_arg(&args, &entities)?;
                let needle = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let all = client.list(&entity).await?;
                let arr = all.as_array().cloned().unwrap_or_default();
                if let Some(needle) = needle {
                    let lower = needle.to_lowercase();
                    let filtered: Vec<Value> = arr
                        .into_iter()
                        .filter(|env| {
                            let n = name_of(env);
                            n.to_lowercase().contains(&lower)
                        })
                        .collect();
                    return Ok(Value::Array(filtered));
                }
                Ok(Value::Array(arr))
            })
        },
    );

    vec![
        Tool {
            name: "catalog.list",
            description: "List every record of a configured entity type. \
                          Use this for a high-level inventory.",
            input_schema: json!({
                "type": "object",
                "required": ["entity"],
                "properties": {
                    "entity": {
                        "type": "string",
                        "enum": entity_enum.clone(),
                        "description": "Which entity to list."
                    }
                }
            }),
            handler: list_handler,
        },
        Tool {
            name: "catalog.get",
            description: "Fetch a single record by id. Returns null if not found.",
            input_schema: json!({
                "type": "object",
                "required": ["entity", "id"],
                "properties": {
                    "entity": { "type": "string", "enum": entity_enum.clone() },
                    "id":     { "type": "string" }
                }
            }),
            handler: get_handler,
        },
        Tool {
            name: "catalog.search",
            description: "Find records whose name contains a substring (case-insensitive). \
                          Returns an array of envelopes.",
            input_schema: json!({
                "type": "object",
                "required": ["entity"],
                "properties": {
                    "entity": { "type": "string", "enum": entity_enum },
                    "name":   { "type": "string", "description": "Substring to match against the record's name." }
                }
            }),
            handler: search_handler,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_returns_three_named_catalog_tools_with_entity_enum() {
        let tools = tools(&["deployable", "service"]);
        assert_eq!(tools.len(), 3);

        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["catalog.list", "catalog.get", "catalog.search"]);

        let expected_enum = json!(["deployable", "service"]);
        for tool in &tools {
            let entity_schema = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.get("entity"))
                .expect("entity property present");
            assert_eq!(
                entity_schema.get("enum").expect("enum on entity"),
                &expected_enum,
                "tool {} should have entity enum {:?}",
                tool.name,
                expected_enum
            );
        }
    }
}

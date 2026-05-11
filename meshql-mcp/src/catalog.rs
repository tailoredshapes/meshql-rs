//! Generic `catalog.list / catalog.get / catalog.search` tools, parameterized
//! over a slice of [`EntityConfig`] so each deployment can configure its own
//! catalogue.
//!
//! These tools post GraphQL queries to each entity's `/graph` endpoint —
//! reads go through the federated graph so the LLM can see foreign-key
//! relations resolved (e.g. an `exposes` row carries its `service { id name }`
//! alongside `service_id`). REST is reserved for writes.

use crate::client::MeshqlClient;
use crate::tool::{Tool, ToolFuture};
use serde_json::{json, Value};
use std::sync::Arc;

/// Legacy entity descriptor used by [`tools`]. Slated for removal alongside
/// this module once all consumers migrate to `Capability`.
#[derive(Clone)]
pub struct EntityConfig {
    pub name: &'static str,
    pub graph_path: String,
    pub fields: &'static str,
}

fn find_entity<'a>(entities: &'a [EntityConfig], name: &str) -> anyhow::Result<&'a EntityConfig> {
    entities.iter().find(|c| c.name == name).ok_or_else(|| {
        let known: Vec<&str> = entities.iter().map(|c| c.name).collect();
        anyhow::anyhow!("entity must be one of {known:?}, got {name:?}")
    })
}

fn entity_arg<'a>(args: &Value, entities: &'a [EntityConfig]) -> anyhow::Result<&'a EntityConfig> {
    let entity = args
        .get("entity")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'entity' argument"))?;
    find_entity(entities, entity)
}

/// Reject ids that would break out of a GraphQL string literal. Until we
/// switch the wire format to use GraphQL `variables`, an id flows directly
/// into the query string — so anything containing `"`, `\`, `\n`, or `\r`
/// is rejected at the handler boundary.
///
/// TODO: replace this with a GraphQL `variables` map and let the server
/// parser handle escaping.
fn sanitize_id(id: &str) -> anyhow::Result<&str> {
    if id
        .chars()
        .any(|c| matches!(c, '"' | '\\' | '\n' | '\r') || (c.is_control()))
    {
        anyhow::bail!("id contains disallowed characters (\", \\, or control chars)");
    }
    Ok(id)
}

/// Build the GraphQL query string for `catalog.list`.
///
/// Pulled out as a private helper so a unit test can pin the exact wire
/// format without spinning up an HTTP mock.
fn build_list_query(fields: &str) -> String {
    format!("{{ getAll {{ {fields} }} }}")
}

/// Build the GraphQL query string for `catalog.get`. `id` must already be
/// sanitized — call [`sanitize_id`] first.
fn build_get_query(fields: &str, id: &str) -> String {
    format!("{{ getById(id: \"{id}\") {{ {fields} }} }}")
}

fn name_of(record: &Value) -> &str {
    record.get("name").and_then(|v| v.as_str()).unwrap_or("")
}

/// Build the three catalogue tools (`catalog.list`, `catalog.get`,
/// `catalog.search`), with each handler validating its `entity` argument
/// against the supplied list and dispatching to the right `/graph` endpoint.
pub fn tools(entities: &[EntityConfig]) -> Vec<Tool> {
    let entity_enum: Vec<Value> = entities.iter().map(|c| json!(c.name)).collect();

    let entities_for_list = entities.to_vec();
    let list_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_list.clone();
            Box::pin(async move {
                let cfg = entity_arg(&args, &entities)?;
                let query = build_list_query(cfg.fields);
                let mut data = client.gql(&cfg.graph_path, &query).await?;
                Ok(data
                    .get_mut("getAll")
                    .map(std::mem::take)
                    .unwrap_or(Value::Array(Vec::new())))
            })
        },
    );

    let entities_for_get = entities.to_vec();
    let get_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_get.clone();
            Box::pin(async move {
                let cfg = entity_arg(&args, &entities)?;
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing 'id' argument"))?;
                let id = sanitize_id(id)?;
                let query = build_get_query(cfg.fields, id);
                let mut data = client.gql(&cfg.graph_path, &query).await?;
                Ok(data
                    .get_mut("getById")
                    .map(std::mem::take)
                    .unwrap_or(Value::Null))
            })
        },
    );

    let entities_for_search = entities.to_vec();
    let search_handler = Arc::new(
        move |client: Arc<MeshqlClient>, args: Value| -> ToolFuture {
            let entities = entities_for_search.clone();
            Box::pin(async move {
                let cfg = entity_arg(&args, &entities)?;
                let needle = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let query = build_list_query(cfg.fields);
                let mut data = client.gql(&cfg.graph_path, &query).await?;
                let arr = data
                    .get_mut("getAll")
                    .map(std::mem::take)
                    .and_then(|v| match v {
                        Value::Array(a) => Some(a),
                        _ => None,
                    })
                    .unwrap_or_default();
                if let Some(needle) = needle {
                    let lower = needle.to_lowercase();
                    let filtered: Vec<Value> = arr
                        .into_iter()
                        .filter(|rec| name_of(rec).to_lowercase().contains(&lower))
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
            description: "List every record of a configured entity type via /graph. \
                          Returns flat GraphQL records (federated fields included).",
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
            description: "Fetch a single record by id via /graph. Returns null if not found.",
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
                          Fetches via /graph and filters client-side; returns an array of records.",
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

    fn fixture_entities() -> Vec<EntityConfig> {
        vec![
            EntityConfig {
                name: "deployable",
                graph_path: "/deployable/graph".to_string(),
                fields: "id name team { id name }",
            },
            EntityConfig {
                name: "service",
                graph_path: "/service/graph".to_string(),
                fields: "id name",
            },
        ]
    }

    #[test]
    fn tools_returns_three_named_catalog_tools_with_entity_enum() {
        let entities = fixture_entities();
        let tools = tools(&entities);
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

    #[test]
    fn catalog_list_builds_getall_query_with_configured_fields() {
        let q = build_list_query("id name team { id name }");
        assert_eq!(q, "{ getAll { id name team { id name } } }");
    }

    #[test]
    fn catalog_get_builds_getbyid_query_with_id_literal() {
        let q = build_get_query("id name", "abc-123");
        assert_eq!(q, "{ getById(id: \"abc-123\") { id name } }");
    }

    #[test]
    fn sanitize_id_rejects_quote_backslash_and_control_chars() {
        assert!(sanitize_id("ok-id").is_ok());
        assert!(sanitize_id("123e4567-e89b-12d3-a456-426614174000").is_ok());
        assert!(sanitize_id("\"; DROP --").is_err());
        assert!(sanitize_id("back\\slash").is_err());
        assert!(sanitize_id("newline\nhere").is_err());
        assert!(sanitize_id("carriage\rreturn").is_err());
        // arbitrary control char
        assert!(sanitize_id("tab\there").is_err());
    }
}

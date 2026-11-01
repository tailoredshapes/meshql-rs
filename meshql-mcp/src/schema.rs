//! SDL discovery uses the same complete parser as graphlettes.
use anyhow::{anyhow, Result};
use async_graphql_parser::{parse_schema, types as p};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSchema {
    pub entity_name: String,
    pub entity_fields: Vec<EntityField>,
    pub query_ops: Vec<QueryOp>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityField {
    pub name: String,
    pub type_text: String,
    pub is_federated: bool,
    pub federation_subselection: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOp {
    pub name: String,
    pub args: Vec<(String, String)>,
    pub returns_list: bool,
}
fn base(ty: &p::Type) -> String {
    match &ty.base {
        p::BaseType::Named(n) => n.to_string(),
        p::BaseType::List(t) => base(t),
    }
}
fn objects(text: &str) -> Result<BTreeMap<String, Vec<p::FieldDefinition>>> {
    let mut out = BTreeMap::new();
    for definition in parse_schema(text)?.definitions {
        if let p::TypeSystemDefinition::Type(t) = definition {
            if let p::TypeKind::Object(object) = t.node.kind {
                out.entry(t.node.name.node.to_string())
                    .or_insert_with(Vec::new)
                    .extend(object.fields.into_iter().map(|f| f.node));
            }
        }
    }
    Ok(out)
}
pub fn parse_meshql_schema(text: &str) -> Result<ParsedSchema> {
    let types = objects(text)?;
    let query = types
        .get("Query")
        .ok_or_else(|| anyhow!("schema has no Query type"))?;
    let name = query
        .iter()
        .find_map(|f| {
            let n = base(&f.ty.node);
            types.contains_key(&n).then_some(n)
        })
        .ok_or_else(|| anyhow!("Query has no object result"))?;
    let fields = &types[&name];
    let entity_fields = fields
        .iter()
        .map(|f| {
            let projected = types.get(&base(&f.ty.node));
            let selection = projected.map(|fs| {
                fs.iter()
                    .filter(|sub| {
                        !types.contains_key(&base(&sub.ty.node)) && sub.arguments.is_empty()
                    })
                    .map(|sub| sub.name.node.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            EntityField {
                name: f.name.node.to_string(),
                type_text: f.ty.node.to_string(),
                is_federated: projected.is_some(),
                federation_subselection: selection
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("{{ {s} }}")),
            }
        })
        .collect();
    let query_ops = query
        .iter()
        .map(|f| QueryOp {
            name: f.name.node.to_string(),
            args: f
                .arguments
                .iter()
                .map(|a| (a.node.name.node.to_string(), a.node.ty.node.to_string()))
                .collect(),
            returns_list: matches!(f.ty.node.base, p::BaseType::List(_)),
        })
        .collect();
    Ok(ParsedSchema {
        entity_name: name,
        entity_fields,
        query_ops,
    })
}
pub(crate) fn query_arguments(
    text: &str,
    operation: &str,
) -> Result<Vec<async_graphql_parser::Positioned<p::InputValueDefinition>>> {
    objects(text)?
        .remove("Query")
        .and_then(|fields| {
            fields
                .into_iter()
                .find(|field| field.name.node.as_str() == operation)
        })
        .map(|field| field.arguments)
        .ok_or_else(|| anyhow!("missing query operation {operation}"))
}
pub(crate) fn selection_for_query(text: &str, operation: &str) -> Result<Option<String>> {
    let types = objects(text)?;
    let field = types
        .get("Query")
        .and_then(|fs| fs.iter().find(|f| f.name.node.as_str() == operation))
        .ok_or_else(|| anyhow!("missing query operation"))?;
    let Some(fields) = types.get(&base(&field.ty.node)) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for field in fields {
        if !field.arguments.is_empty() {
            continue;
        }
        let name = field.name.node.to_string();
        if let Some(projected) = types.get(&base(&field.ty.node)) {
            let leaves = projected
                .iter()
                .filter(|f| !types.contains_key(&base(&f.ty.node)) && f.arguments.is_empty())
                .map(|f| f.name.node.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            if !leaves.is_empty() {
                out.push(format!("{name} {{ {leaves} }}"));
            }
        } else {
            out.push(name);
        }
    }
    Ok(Some(out.join(" ")))
}
pub fn render_entity_field_selection(schema: &ParsedSchema) -> String {
    schema
        .entity_fields
        .iter()
        .filter_map(|f| {
            if f.is_federated {
                f.federation_subselection
                    .as_ref()
                    .map(|s| format!("{} {s}", f.name))
            } else {
                Some(f.name.clone())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;

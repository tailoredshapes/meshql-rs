//! Hand-rolled parser for the meshql GraphQL subset.
//!
//! meshql schema files declare:
//!
//! - One principal `type X { ... }` block (the entity).
//! - Zero or more federated projection types (e.g. `type Team { ... }`) that
//!   the entity references — embedded one level deep.
//! - A `type Query { ... }` block listing operations
//!   (`getById`, `getAll`, `getByName`, `getByXId`, …).
//!
//! This module pulls out:
//!
//! - The principal entity's name and its fields (each annotated with whether
//!   it is a federated reference and, if so, a one-level subselection of the
//!   referenced type).
//! - Every Query operation, its argument list, and whether it returns a list.
//!
//! The parser tolerates whitespace, comments (`#`), and the trailing `!`
//! non-null marker. It does **not** try to be a full GraphQL parser: it bails
//! with a clear error on constructs the meshql dialect doesn't use (input
//! types, unions, interfaces, directives, etc.).

use anyhow::{anyhow, bail, Context, Result};

/// Parsed view of one meshql schema file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSchema {
    /// The principal entity type, e.g. `"Deployable"`.
    pub entity_name: String,
    /// Fields on the entity type, in declaration order.
    pub entity_fields: Vec<EntityField>,
    /// Operations from the `Query` type, in declaration order.
    pub query_ops: Vec<QueryOp>,
}

/// One field on the principal entity type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityField {
    /// Field name, e.g. `"team_id"` or `"team"`.
    pub name: String,
    /// Original type text, e.g. `"String"`, `"String!"`, `"[Deployable]"`,
    /// `"Team"`. The trailing `!` is preserved when present.
    pub type_text: String,
    /// True when the field's type is another type declared in the same
    /// schema file (a federated projection).
    pub is_federated: bool,
    /// When federated, a `{ subfield subfield }` snippet covering the
    /// referenced type's scalar fields (no nested projections).
    pub federation_subselection: Option<String>,
}

/// One operation on the schema's `Query` type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOp {
    /// Operation name, e.g. `"getById"`, `"getByDeployableId"`.
    pub name: String,
    /// `(arg_name, arg_type)` pairs in declaration order. Includes `at: Float`
    /// — callers filter it out when building tool input schemas.
    pub args: Vec<(String, String)>,
    /// True when the operation's return type is `[Entity]`, false for
    /// `Entity` (or `Entity!`).
    pub returns_list: bool,
}

/// Parse a meshql schema string.
pub fn parse_meshql_schema(text: &str) -> Result<ParsedSchema> {
    let blocks = collect_type_blocks(text)?;
    if blocks.is_empty() {
        bail!("schema contains no `type` blocks");
    }

    // The principal entity is the first non-Query type block.
    let principal = blocks
        .iter()
        .find(|b| b.name != "Query")
        .ok_or_else(|| anyhow!("schema has no entity type (only Query)"))?;

    // Other non-Query, non-principal type blocks are federated projections.
    let projection_types: Vec<&TypeBlock> = blocks
        .iter()
        .filter(|b| b.name != "Query" && b.name != principal.name)
        .collect();

    let entity_fields = parse_entity_fields(&principal.body, &projection_types)
        .with_context(|| format!("parsing fields of type {}", principal.name))?;

    let query_block = blocks.iter().find(|b| b.name == "Query");
    let query_ops = match query_block {
        Some(qb) => parse_query_ops(&qb.body).context("parsing Query operations")?,
        None => Vec::new(),
    };

    Ok(ParsedSchema {
        entity_name: principal.name.clone(),
        entity_fields,
        query_ops,
    })
}

/// Render `schema`'s entity-field selection as a GraphQL query body.
///
/// Scalars appear as bare names; federated fields appear with their one-level
/// subselection. Suitable to drop into `{ getAll { <body> } }` or similar.
pub fn render_entity_field_selection(schema: &ParsedSchema) -> String {
    let mut parts = Vec::with_capacity(schema.entity_fields.len());
    for f in &schema.entity_fields {
        match (&f.is_federated, &f.federation_subselection) {
            (true, Some(sub)) => parts.push(format!("{} {}", f.name, sub)),
            // Federated reference with no usable subselection — emit the name
            // alone; the graphlette will reject if needed.
            _ => parts.push(f.name.clone()),
        }
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

struct TypeBlock {
    name: String,
    body: String,
}

/// Strip line comments (`#...`) from a schema string. Comments may appear at
/// the start of a line or after content; we strip everything from `#` to the
/// next newline.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        if let Some(idx) = line.find('#') {
            out.push_str(&line[..idx]);
            // Preserve the newline so line-counting in errors stays sane.
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Split the schema into `type X { ... }` blocks. Each block's name and the
/// raw text between `{` and the matching `}` are captured.
fn collect_type_blocks(text: &str) -> Result<Vec<TypeBlock>> {
    let text = strip_comments(text);
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut blocks = Vec::new();

    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Expect "type".
        let rest = &text[i..];
        if let Some(after_type) = rest.strip_prefix("type") {
            // Ensure word boundary.
            let after_idx = i + 4;
            if after_idx > bytes.len()
                || (after_idx < bytes.len() && is_ident_char(bytes[after_idx]))
            {
                bail!(
                    "expected `type` keyword, but found something else at byte offset {i}: {:?}",
                    &text[i..i.saturating_add(20).min(text.len())]
                );
            }
            let _ = after_type;
            i = after_idx;
            // Whitespace.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Identifier.
            let name_start = i;
            while i < bytes.len() && is_ident_char(bytes[i]) {
                i += 1;
            }
            if name_start == i {
                bail!("expected type name after `type` at offset {name_start}");
            }
            let name = text[name_start..i].to_string();
            // Whitespace.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Expect `{`.
            if i >= bytes.len() || bytes[i] != b'{' {
                bail!(
                    "expected `{{` after type name `{name}` at offset {i}, got {:?}",
                    bytes.get(i).map(|b| *b as char)
                );
            }
            i += 1;
            // Capture until matching `}` (no nesting in meshql).
            let body_start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            if i >= bytes.len() {
                bail!("unterminated body for type `{name}` (missing `}}`)");
            }
            let body = text[body_start..i].to_string();
            i += 1; // consume `}`
            blocks.push(TypeBlock { name, body });
        } else {
            bail!(
                "unexpected text at offset {i}: {:?}",
                &text[i..i.saturating_add(20).min(text.len())]
            );
        }
    }

    Ok(blocks)
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parse the lines inside an entity type body into `EntityField` records.
fn parse_entity_fields(body: &str, projections: &[&TypeBlock]) -> Result<Vec<EntityField>> {
    let mut fields = Vec::new();
    for raw in body.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Trim a trailing comma if any.
        let line = line.trim_end_matches(',');
        // Format: `name: TypeText` (possibly with `!`, brackets).
        let (name, type_text) = split_once_trim(line, ':')
            .ok_or_else(|| anyhow!("malformed field declaration: {line:?}"))?;
        if name.is_empty() || type_text.is_empty() {
            bail!("malformed field declaration: {line:?}");
        }
        let base_type = base_type_name(&type_text);
        let is_federated = projections.iter().any(|p| p.name == base_type);
        let federation_subselection = if is_federated {
            // Build a one-level subselection over the referenced type's fields
            // — including any non-projected references (we won't recurse into
            // those, just emit the bare name).
            let projection = projections
                .iter()
                .find(|p| p.name == base_type)
                .expect("federated field has projection");
            Some(render_projection_subselection(&projection.body))
        } else {
            None
        };
        fields.push(EntityField {
            name,
            type_text,
            is_federated,
            federation_subselection,
        });
    }
    if fields.is_empty() {
        bail!("type body has no fields");
    }
    Ok(fields)
}

/// Render `{ a b c }` for a projection type's body — scalars only. We don't
/// recurse into nested projection references; the bare name is emitted.
fn render_projection_subselection(body: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    for raw in body.split('\n') {
        let line = raw.trim().trim_end_matches(',');
        if line.is_empty() {
            continue;
        }
        if let Some((name, _ty)) = split_once_trim(line, ':') {
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    format!("{{ {} }}", names.join(" "))
}

/// Return the base named type from a GraphQL type text (`String!` → `String`,
/// `[Deployable]` → `Deployable`, `[Deployable!]!` → `Deployable`).
fn base_type_name(type_text: &str) -> String {
    let mut s = type_text.trim();
    s = s.trim_end_matches('!');
    s = s.trim_end_matches(']').trim_start_matches('[');
    s = s.trim_end_matches('!');
    s.trim().to_string()
}

fn split_once_trim(s: &str, delim: char) -> Option<(String, String)> {
    let idx = s.find(delim)?;
    let (l, r) = s.split_at(idx);
    Some((
        l.trim().to_string(),
        r[delim.len_utf8()..].trim().to_string(),
    ))
}

/// Parse the lines inside the `Query` body into `QueryOp` records.
fn parse_query_ops(body: &str) -> Result<Vec<QueryOp>> {
    let mut ops = Vec::new();
    for raw in body.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let line = line.trim_end_matches(',');
        // Format: `name(arg: Type, arg: Type, ...): ReturnType`
        let open = line
            .find('(')
            .ok_or_else(|| anyhow!("malformed Query op (no `(`): {line:?}"))?;
        let close = line
            .find(')')
            .ok_or_else(|| anyhow!("malformed Query op (no `)`): {line:?}"))?;
        if close < open {
            bail!("malformed Query op (parens out of order): {line:?}");
        }
        let name = line[..open].trim().to_string();
        if name.is_empty() {
            bail!("Query op missing name: {line:?}");
        }
        let args_text = line[open + 1..close].trim();
        let args = if args_text.is_empty() {
            Vec::new()
        } else {
            parse_arg_list(args_text)?
        };
        let after_close = line[close + 1..].trim();
        let return_text = after_close
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("Query op missing return type: {line:?}"))?
            .trim();
        let returns_list = return_text.starts_with('[');
        ops.push(QueryOp {
            name,
            args,
            returns_list,
        });
    }
    Ok(ops)
}

fn parse_arg_list(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for piece in text.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (name, ty) = split_once_trim(piece, ':')
            .ok_or_else(|| anyhow!("malformed arg `{piece}` — expected `name: Type`"))?;
        if name.is_empty() || ty.is_empty() {
            bail!("malformed arg `{piece}` — expected `name: Type`");
        }
        out.push((name, ty));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
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
}

//! High-level [`Capability`] abstraction over [`Tool`].
//!
//! A `Capability` is a named, described, schema-typed operation that knows
//! *how* to dispatch itself to a meshql deployment via [`CapabilityHandler`].
//! The four handler variants cover the common shapes:
//!
//! - `GraphQuery` — POST a templated GraphQL query to a graphlette endpoint.
//! - `RestGet` — GET a templated REST path (computed/aggregated endpoints).
//! - `RestPost` — POST a templated REST path with an optional body
//!   (writes/commands).
//! - `Custom` — escape hatch for handlers that don't fit a template.
//!
//! Each capability is converted to a low-level [`Tool`] at server-construction
//! time via [`Capability::into_tool`]; the rest of the wire-handling code
//! (transport, `tools/list`, `tools/call`) is unchanged.
//!
//! Templates support `{arg_name}` placeholders that are substituted from the
//! tool's input JSON. Strings are GraphQL-escaped to avoid injection;
//! numbers, booleans, and nulls insert as their JSON form. A missing
//! placeholder returns an error to the caller.

use crate::client::MeshqlClient;
use crate::schema::{parse_meshql_schema, render_entity_field_selection, ParsedSchema, QueryOp};
use crate::tool::{Tool, ToolFuture, ToolHandler};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Strip out tool-meta fields that the LLM might pass alongside the payload
/// (e.g. `id` on creates, future `_meta` markers). Returns a fresh JSON
/// object suitable for use as the REST body.
fn strip_meta(args: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "id" {
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// A single MCP operation: a named/described/schema-typed wrapper around a
/// [`CapabilityHandler`] that knows how to dispatch itself.
#[derive(Clone)]
pub struct Capability {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub handler: CapabilityHandler,
}

/// What kind of work a [`Capability`] does when invoked. The first three
/// variants are read/RPC dispatchers (no identity required); the `Entity*`
/// variants are mutating writes that **require** the MCP server's client to
/// have been built with an identity (see [`crate::client::MeshqlClient::with_identity_from_env`]).
/// `Custom` is an escape hatch.
#[derive(Clone)]
pub enum CapabilityHandler {
    /// POST a templated GraphQL query to a graphlette endpoint and return the
    /// `data` payload.
    GraphQuery {
        /// Graph route, e.g. `"/deployable/graph"`.
        path: String,
        /// GraphQL query with `{arg_name}` placeholders to substitute from
        /// the tool's input JSON.
        query_template: String,
    },
    /// GET a templated REST path. Placeholders in the path resolve from the
    /// tool's input JSON.
    RestGet {
        /// REST path with placeholders, e.g.
        /// `"/test_environment/{id}/history"`.
        path_template: String,
    },
    /// POST a templated REST path with an optional body — used for RPC-shaped
    /// reads that don't fit GraphQL (e.g. `compute_plan_for_change_request`).
    /// No identity is required; this is for computed/aggregated endpoints,
    /// not domain writes. Use the `EntityCreate`/`EntityUpdate`/`EntityDelete`
    /// variants for writes that mutate envelopes.
    RestPost {
        path_template: String,
        body_template: Option<Value>,
    },
    /// `POST /<entity>/api` — create a new envelope. The whole input JSON
    /// (minus any fields named in `meta_keys`) becomes the payload. Requires
    /// identity (returns a clear error to the caller otherwise).
    EntityCreate { api_path: String },
    /// `PUT /<entity>/api/{id}` — update an existing envelope. Reads the `id`
    /// field out of the input, sends the rest as the payload. Requires identity.
    EntityUpdate { api_path: String },
    /// `DELETE /<entity>/api/{id}` — soft-delete an envelope. Reads the `id`
    /// field out of the input. Requires identity.
    EntityDelete { api_path: String },
    /// Run a [`ToolHandler`] directly. Used for domain logic that doesn't fit
    /// a template (snapshot-based traversals etc.).
    Custom(ToolHandler),
}

impl Capability {
    /// Replace this capability's description and return it. Used by
    /// `CapabilitiesBuilder::describe` to swap auto-generated defaults.
    pub fn with_description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// Convert this capability into a low-level [`Tool`] whose handler knows
    /// how to dispatch the underlying [`CapabilityHandler`]. Called by
    /// `MeshqlMcpServer::new` for every configured capability.
    pub(crate) fn into_tool(self, _client_unused: Arc<MeshqlClient>) -> Tool {
        let Capability {
            name,
            description,
            input_schema,
            handler,
        } = self;

        // The tool's handler closure receives the same `Arc<MeshqlClient>`
        // that the server holds, so we don't capture one here — we just
        // capture the templates and dispatch off the live client.
        let dispatcher: ToolHandler = match handler {
            CapabilityHandler::GraphQuery {
                path,
                query_template,
            } => {
                let path = Arc::new(path);
                let query_template = Arc::new(query_template);
                Arc::new(move |client, args| -> ToolFuture {
                    let path = path.clone();
                    let query_template = query_template.clone();
                    Box::pin(async move {
                        let query = substitute(&query_template, &args)?;
                        client.gql(&path, &query).await
                    })
                })
            }
            CapabilityHandler::RestGet { path_template } => {
                let path_template = Arc::new(path_template);
                Arc::new(move |client, args| -> ToolFuture {
                    let path_template = path_template.clone();
                    Box::pin(async move {
                        let path = substitute(&path_template, &args)?;
                        client.get_path(&path).await
                    })
                })
            }
            CapabilityHandler::RestPost {
                path_template,
                body_template,
            } => {
                let path_template = Arc::new(path_template);
                let body_template = Arc::new(body_template);
                Arc::new(move |client, args| -> ToolFuture {
                    let path_template = path_template.clone();
                    let body_template = body_template.clone();
                    Box::pin(async move {
                        let path = substitute(&path_template, &args)?;
                        let body = match body_template.as_ref() {
                            Some(t) => substitute_value(t, &args)?,
                            None => Value::Object(serde_json::Map::new()),
                        };
                        client.post_path(&path, &body).await
                    })
                })
            }
            CapabilityHandler::EntityCreate { api_path } => {
                let api_path = Arc::new(api_path);
                Arc::new(move |client, args| -> ToolFuture {
                    let api_path = api_path.clone();
                    Box::pin(async move {
                        client.require_identity()?;
                        let payload = strip_meta(&args);
                        client.post_path(&api_path, &payload).await
                    })
                })
            }
            CapabilityHandler::EntityUpdate { api_path } => {
                let api_path = Arc::new(api_path);
                Arc::new(move |client, args| -> ToolFuture {
                    let api_path = api_path.clone();
                    Box::pin(async move {
                        client.require_identity()?;
                        let id = args
                            .get("id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                anyhow::anyhow!("update requires a string `id` argument")
                            })?
                            .to_string();
                        let payload = strip_meta(&args);
                        let url = format!("{}/{}", api_path, id);
                        client.put_path(&url, &payload).await
                    })
                })
            }
            CapabilityHandler::EntityDelete { api_path } => {
                let api_path = Arc::new(api_path);
                Arc::new(move |client, args| -> ToolFuture {
                    let api_path = api_path.clone();
                    Box::pin(async move {
                        client.require_identity()?;
                        let id = args
                            .get("id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                anyhow::anyhow!("delete requires a string `id` argument")
                            })?
                            .to_string();
                        let url = format!("{}/{}", api_path, id);
                        client.delete_path(&url).await
                    })
                })
            }
            CapabilityHandler::Custom(handler) => handler,
        };

        Tool {
            name,
            description,
            input_schema,
            handler: dispatcher,
        }
    }
}

/// Substitute `{arg_name}` placeholders in `template` with values from `args`.
///
/// A placeholder is `{` immediately followed by a non-empty identifier
/// (`[A-Za-z_][A-Za-z0-9_]*`) and `}` with no intervening whitespace. Any `{`
/// that isn't followed by such a token (e.g. the GraphQL selection-set
/// braces `{ getAll { id } }`) is treated as a literal character.
///
/// String values are GraphQL-escaped (`"` → `\"`, `\` → `\\`, newlines → `\n`,
/// CRs → `\r`, tabs → `\t`). Numbers, booleans, and nulls insert as their
/// JSON form (without surrounding quotes — the template is expected to handle
/// quoting context). Missing placeholders (no matching `args` key) return an
/// error.
///
/// Used for `GraphQuery::query_template`, `RestGet::path_template`, and
/// `RestPost::path_template`. Body-template substitution walks the JSON tree
/// via [`substitute_value`] instead.
pub(crate) fn substitute(template: &str, args: &Value) -> anyhow::Result<String> {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'{' {
            if let Some((key, end_idx)) = try_parse_placeholder(template, i) {
                let value = args.get(key).ok_or_else(|| {
                    anyhow::anyhow!("missing argument `{key}` for placeholder `{{{key}}}`")
                })?;
                out.push_str(&render_substituted_value(value));
                i = end_idx;
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    Ok(out)
}

/// If `template[start..]` begins with `{ident}`, return `(ident, end_idx)` —
/// the identifier and the byte index just past the closing `}`. Otherwise
/// `None`, signaling that the `{` at `start` is literal GraphQL syntax.
fn try_parse_placeholder(template: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = template.as_bytes();
    debug_assert_eq!(bytes[start], b'{');
    let mut i = start + 1;
    let id_start = i;
    while i < bytes.len() && is_ident_char(bytes[i]) {
        i += 1;
    }
    if i == id_start {
        return None;
    }
    if i >= bytes.len() || bytes[i] != b'}' {
        return None;
    }
    Some((&template[id_start..i], i + 1))
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Recursively walk a JSON value, substituting `{arg_name}` placeholders in
/// every string leaf. Object keys are left as-is; only string *values* are
/// rewritten. Used for `RestPost::body_template`.
pub(crate) fn substitute_value(template: &Value, args: &Value) -> anyhow::Result<Value> {
    match template {
        Value::String(s) => Ok(Value::String(substitute(s, args)?)),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(substitute_value(v, args)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(obj) => {
            let mut out = serde_json::Map::with_capacity(obj.len());
            for (k, v) in obj {
                out.insert(k.clone(), substitute_value(v, args)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

/// Render `value` for insertion into a substitution result. Strings are
/// escaped for GraphQL string literals; numbers/booleans/nulls render as
/// their JSON form so they can be dropped into a query without quotes.
fn render_substituted_value(value: &Value) -> String {
    match value {
        Value::String(s) => escape_for_graphql(s),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        // Compound values get inserted as their JSON form so they roundtrip
        // through path-template substitutions sensibly. GraphQL callers
        // shouldn't be plumbing arrays/objects through `{x}` placeholders
        // in v1 (they should use `variables`); this is just safe-default
        // behaviour.
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// CapabilitiesBuilder
// ---------------------------------------------------------------------------

/// Schema-driven builder for the capability list passed to
/// [`McpServerConfig`](crate::transport::McpServerConfig).
///
/// Each app's `<app>-mcp` bin calls [`Self::auto_from_schemas`] with its
/// `include_str!`'d GraphQL schema files to seed baseline list/get/find/
/// by-FK capabilities, then layers `.describe` overrides for tools that
/// earn richer wording and `.add` for custom (graph-traversal,
/// computed-REST) capabilities.
#[derive(Default)]
pub struct CapabilitiesBuilder {
    capabilities: Vec<Capability>,
}

impl CapabilitiesBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Auto-generate baseline capabilities from a slice of
    /// `(entity_singular, graph_path, schema_text)` triples. For each schema,
    /// every `Query` operation becomes one capability following the naming
    /// convention table in the crate-level docs. Schemas that fail to parse
    /// log a warning to stderr and are skipped.
    pub fn auto_from_schemas(
        mut self,
        schemas: &[(&'static str, &'static str, &'static str)],
    ) -> Self {
        for (entity, graph_path, schema_text) in schemas {
            let parsed = match parse_meshql_schema(schema_text) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "[meshql-mcp] auto_from_schemas: failed to parse schema for {entity}: {e}"
                    );
                    continue;
                }
            };
            if parsed.query_ops.is_empty() {
                eprintln!(
                    "[meshql-mcp] auto_from_schemas: schema for {entity} has no Query operations; skipping"
                );
                continue;
            }
            let body = render_entity_field_selection(&parsed);
            for op in &parsed.query_ops {
                if let Some(cap) = derive_capability(entity, graph_path, &parsed, op, &body) {
                    self = self.add(cap);
                }
            }
            // Write capabilities — auto-derived from the graph path:
            //   /<entity>/graph  →  /<entity>/api  for create / update / delete
            let api_path = graph_path.trim_end_matches("/graph").to_string() + "/api";
            for cap in derive_write_capabilities(entity, api_path, &parsed) {
                self = self.add(cap);
            }
        }
        self
    }

    /// Replace the description on the capability named `capability_name`. If
    /// no capability matches, logs to stderr (so typos surface) and returns
    /// the builder unchanged.
    pub fn describe(mut self, capability_name: &'static str, description: &'static str) -> Self {
        let found = self
            .capabilities
            .iter_mut()
            .find(|c| c.name == capability_name);
        match found {
            Some(cap) => cap.description = description,
            None => eprintln!(
                "[meshql-mcp] describe: no capability named {capability_name}; ignoring override"
            ),
        }
        self
    }

    /// Append a [`Capability`]. If another capability with the same name
    /// already exists, the previous one is replaced and a warning is logged.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, capability: Capability) -> Self {
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|c| c.name == capability.name)
        {
            eprintln!(
                "[meshql-mcp] add: replacing existing capability {}",
                capability.name
            );
            *existing = capability;
        } else {
            self.capabilities.push(capability);
        }
        self
    }

    /// Finalize and return the capability list.
    pub fn build(self) -> Vec<Capability> {
        self.capabilities
    }
}

/// Build the three baseline write capabilities for an entity:
/// `create_X`, `update_X`, `delete_X`. They all dispatch to
/// `/<entity>/api` and require a configured identity at call time.
fn derive_write_capabilities(
    entity: &'static str,
    api_path: String,
    parsed: &ParsedSchema,
) -> Vec<Capability> {
    let create_schema = build_payload_schema(parsed);
    let update_schema = build_update_schema(parsed);
    vec![
        make_capability(
            leak_string(format!("create_{entity}")),
            leak_string(default_description_create(entity, &parsed.entity_name)),
            create_schema,
            CapabilityHandler::EntityCreate {
                api_path: api_path.clone(),
            },
        ),
        make_capability(
            leak_string(format!("update_{entity}")),
            leak_string(default_description_update(entity, &parsed.entity_name)),
            update_schema,
            CapabilityHandler::EntityUpdate {
                api_path: api_path.clone(),
            },
        ),
        make_capability(
            leak_string(format!("delete_{entity}")),
            leak_string(default_description_delete(entity, &parsed.entity_name)),
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
            CapabilityHandler::EntityDelete { api_path },
        ),
    ]
}

/// JSON schema for the create input — every payload field on the entity,
/// minus federated projections (which are resolved at read time) and `id`
/// (the server generates it). Required fields are those whose `type_text`
/// has a trailing `!`.
fn build_payload_schema(parsed: &ParsedSchema) -> Value {
    let mut props = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for f in &parsed.entity_fields {
        if f.is_federated || f.name == "id" {
            continue;
        }
        let json_type = graphql_type_to_json_type(&f.type_text);
        props.insert(f.name.clone(), json!({ "type": json_type }));
        if f.type_text.trim_end().ends_with('!') {
            required.push(Value::String(f.name.clone()));
        }
    }
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": Value::Array(required)
    })
}

/// JSON schema for `update_X`: same as the create payload but with `id` added
/// as a required field, and no required payload fields (callers can update
/// just the ones they care about).
fn build_update_schema(parsed: &ParsedSchema) -> Value {
    let mut props = Map::new();
    props.insert("id".to_string(), json!({ "type": "string" }));
    for f in &parsed.entity_fields {
        if f.is_federated || f.name == "id" {
            continue;
        }
        let json_type = graphql_type_to_json_type(&f.type_text);
        props.insert(f.name.clone(), json!({ "type": json_type }));
    }
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": ["id"]
    })
}

fn graphql_type_to_json_type(type_text: &str) -> &'static str {
    let trimmed = type_text.trim_end_matches('!').trim();
    match trimmed {
        "Int" | "Float" => "number",
        "Boolean" => "boolean",
        _ if trimmed.starts_with('[') => "array",
        _ => "string",
    }
}

fn default_description_create(entity: &str, type_name: &str) -> String {
    format!(
        "Create a new {entity} ({type_name}). Requires MANIFOLD_USER_ID configured on this MCP server. The created envelope's `authorized_tokens` will be the caller's roles."
    )
}

fn default_description_update(entity: &str, type_name: &str) -> String {
    format!(
        "Update an existing {entity} ({type_name}). Supply `id` plus any fields to change. Requires MANIFOLD_USER_ID configured on this MCP server."
    )
}

fn default_description_delete(entity: &str, type_name: &str) -> String {
    format!(
        "Soft-delete a {entity} ({type_name}) by id. Requires MANIFOLD_USER_ID configured on this MCP server."
    )
}

/// Build one capability for a `Query` operation, using the naming-convention
/// table in the crate-level docs. Returns `None` for operations we don't
/// recognize (apps add custom capabilities for those).
fn derive_capability(
    entity: &'static str,
    graph_path: &'static str,
    parsed: &ParsedSchema,
    op: &QueryOp,
    body: &str,
) -> Option<Capability> {
    let plural = pluralize(entity);
    // Surface arguments come from the op's args, minus the federation-time
    // cursor `at: Float` which apps don't expose to LLMs.
    let surface_args: Vec<&(String, String)> =
        op.args.iter().filter(|(name, _)| name != "at").collect();

    match op.name.as_str() {
        "getAll" => Some(make_capability(
            leak_string(format!("list_{plural}")),
            leak_string(default_description_list(entity, &parsed.entity_name)),
            json!({ "type": "object", "properties": {} }),
            CapabilityHandler::GraphQuery {
                path: graph_path.to_string(),
                query_template: format!("{{ getAll {{ {body} }} }}"),
            },
        )),
        "getById" => Some(make_capability(
            leak_string(format!("get_{entity}_by_id")),
            leak_string(default_description_get_by_id(entity, &parsed.entity_name)),
            schema_for_single_required(&surface_args),
            CapabilityHandler::GraphQuery {
                path: graph_path.to_string(),
                query_template: format!("{{ getById(id: \"{{id}}\") {{ {body} }} }}"),
            },
        )),
        "getByName" => Some(make_capability(
            leak_string(format!("find_{plural}_by_name")),
            leak_string(default_description_find_by_name(
                entity,
                &parsed.entity_name,
            )),
            schema_for_single_required(&surface_args),
            CapabilityHandler::GraphQuery {
                path: graph_path.to_string(),
                query_template: format!("{{ getByName(name: \"{{name}}\") {{ {body} }} }}"),
            },
        )),
        other if other.starts_with("getBy") && other.ends_with("Id") => {
            // getByDeployableId(deployable_id: ...) → "deployables_for_deployable"
            // We use the first surface arg's name as the FK; strip the `_id`
            // suffix to get the related entity's bare name.
            let (arg_name, _) = surface_args.first()?;
            let related = arg_name.strip_suffix("_id").unwrap_or(arg_name);
            let cap_name = leak_string(format!("{plural}_for_{related}"));
            let desc = leak_string(default_description_by_fk(
                entity,
                &parsed.entity_name,
                related,
            ));
            // Build the inline query for the operation by name.
            let op_name = op.name.clone();
            let arg_lit = arg_name.clone();
            let query_template = format!(
                "{{ {op_name}({arg_lit}: \"{{{arg_lit}}}\") {{ {body} }} }}",
                op_name = op_name,
                arg_lit = arg_lit,
                body = body,
            );
            Some(make_capability(
                cap_name,
                desc,
                schema_for_single_required(&surface_args),
                CapabilityHandler::GraphQuery {
                    path: graph_path.to_string(),
                    query_template,
                },
            ))
        }
        // Operations we don't auto-derive (e.g. `count`, `getByMultiple`).
        _ => None,
    }
}

fn make_capability(
    name: &'static str,
    description: &'static str,
    input_schema: Value,
    handler: CapabilityHandler,
) -> Capability {
    Capability {
        name,
        description,
        input_schema,
        handler,
    }
}

/// `Box::leak` the String into a `&'static str` so it can live in the
/// `Capability`'s static-str fields. Capabilities live for the duration of
/// the process, so the leak is bounded.
fn leak_string(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Naive pluralization: append `s`. Apps override via `.describe()` when the
/// default reads wrong.
fn pluralize(singular: &str) -> String {
    format!("{singular}s")
}

fn default_description_list(entity: &str, type_name: &str) -> String {
    format!(
        "List every {entity} in the catalogue (returns flat GraphQL {type_name} records, federated fields included)."
    )
}

fn default_description_get_by_id(entity: &str, type_name: &str) -> String {
    format!("Fetch one {entity} ({type_name}) by its UUID. Returns null if not found.")
}

fn default_description_find_by_name(entity: &str, type_name: &str) -> String {
    format!("Find {entity} ({type_name}) records whose name matches the supplied value.")
}

fn default_description_by_fk(entity: &str, type_name: &str, related: &str) -> String {
    format!("List {entity} ({type_name}) records related to the given {related}.")
}

/// Build an input schema with one required string property whose name comes
/// from `args[0]`. Used by `getById`, `getByName`, and `getByXId` derivations.
fn schema_for_single_required(args: &[&(String, String)]) -> Value {
    if let Some((name, ty)) = args.first() {
        let json_ty = graphql_to_json_type(ty);
        json!({
            "type": "object",
            "required": [name],
            "properties": {
                name: { "type": json_ty }
            }
        })
    } else {
        json!({ "type": "object", "properties": {} })
    }
}

/// Map a GraphQL scalar type (`ID`, `String`, `Int`, `Float`, `Boolean`) to
/// the JSON Schema type used in the tool's `inputSchema`. Unknown types map
/// to `"string"` (the safe default — LLMs treat ids as strings).
fn graphql_to_json_type(ty: &str) -> &'static str {
    let base = ty.trim().trim_end_matches('!');
    match base {
        "Int" => "integer",
        "Float" => "number",
        "Boolean" => "boolean",
        _ => "string",
    }
}

/// Escape `s` for safe insertion inside a GraphQL string literal.
/// Caller is responsible for providing the surrounding quotes.
fn escape_for_graphql(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_named_placeholders_in_template() {
        let args = json!({ "id": "abc-123", "name": "thing" });
        let out = substitute("{ getById(id: \"{id}\") { name } }", &args).unwrap();
        assert_eq!(out, "{ getById(id: \"abc-123\") { name } }");
    }

    #[test]
    fn escapes_graphql_special_chars_in_string_values() {
        let args = json!({ "name": "she said \"hi\"\nand \\bye" });
        let out = substitute("{ find(name: \"{name}\") }", &args).unwrap();
        assert_eq!(out, r#"{ find(name: "she said \"hi\"\nand \\bye") }"#);
    }

    #[test]
    fn inserts_numbers_and_booleans_as_json() {
        let args = json!({ "limit": 5, "active": true });
        let out = substitute("limit={limit} active={active}", &args).unwrap();
        assert_eq!(out, "limit=5 active=true");
    }

    #[test]
    fn errors_on_missing_placeholder_value() {
        let args = json!({ "other": "x" });
        let err = substitute("hello {missing}", &args).unwrap_err();
        assert!(format!("{err}").contains("missing"));
    }

    #[test]
    fn passes_literal_graphql_braces_through() {
        // A `{` that isn't `{ident}` (e.g. GraphQL selection-set syntax) is
        // passed through verbatim, so `{ getAll { id } }` works without
        // escaping.
        let args = json!({});
        let out = substitute("{ getAll { id name } }", &args).unwrap();
        assert_eq!(out, "{ getAll { id name } }");
    }

    #[test]
    fn unterminated_placeholder_passes_through_literally() {
        let args = json!({});
        // `{oops` is not a complete `{ident}` — treated as literal text.
        let out = substitute("hello {oops", &args).unwrap();
        assert_eq!(out, "hello {oops");
    }

    #[test]
    fn empty_braces_pass_through_literally() {
        let args = json!({});
        let out = substitute("hello {}", &args).unwrap();
        assert_eq!(out, "hello {}");
    }

    #[test]
    fn substitute_value_rewrites_string_leaves_only() {
        let template = json!({
            "id": "{id}",
            "count": 42,
            "nested": { "name": "static {name}", "flag": true },
            "list": ["item-{id}", 7]
        });
        let args = json!({ "id": "abc", "name": "Q" });
        let out = substitute_value(&template, &args).unwrap();
        assert_eq!(
            out,
            json!({
                "id": "abc",
                "count": 42,
                "nested": { "name": "static Q", "flag": true },
                "list": ["item-abc", 7]
            })
        );
    }

    #[test]
    fn with_description_replaces_text() {
        let cap = Capability {
            name: "x",
            description: "old",
            input_schema: json!({}),
            handler: CapabilityHandler::GraphQuery {
                path: "/x/graph".into(),
                query_template: "{ getAll { id } }".into(),
            },
        };
        let cap = cap.with_description("new");
        assert_eq!(cap.description, "new");
    }

    // ----- builder tests -----

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

    fn find_by_name<'a>(caps: &'a [Capability], name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.name == name)
    }

    #[test]
    fn auto_derives_list_get_search_from_deployable_schema() {
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .build();
        let names: Vec<&str> = caps.iter().map(|c| c.name).collect();
        assert!(names.contains(&"list_deployables"), "names = {names:?}");
        assert!(names.contains(&"get_deployable_by_id"), "names = {names:?}");
        assert!(
            names.contains(&"find_deployables_by_name"),
            "names = {names:?}"
        );

        let list = find_by_name(&caps, "list_deployables").unwrap();
        assert!(list.description.contains("deployable"));
        if let CapabilityHandler::GraphQuery {
            path,
            query_template,
        } = &list.handler
        {
            assert_eq!(path, "/deployable/graph");
            // Federated team subselection appears in the field body.
            assert!(
                query_template.contains("team { id name kind description }"),
                "query_template = {query_template:?}"
            );
            assert!(query_template.starts_with("{ getAll {"));
        } else {
            panic!("expected GraphQuery handler");
        }

        let get = find_by_name(&caps, "get_deployable_by_id").unwrap();
        // input_schema requires "id".
        assert_eq!(get.input_schema["required"][0], "id");
        assert_eq!(get.input_schema["properties"]["id"]["type"], "string");

        let find = find_by_name(&caps, "find_deployables_by_name").unwrap();
        assert_eq!(find.input_schema["required"][0], "name");
    }

    #[test]
    fn auto_derives_for_by_fk_from_dependency_schema() {
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("dependency", "/dependency/graph", DEPENDENCY_SCHEMA)])
            .build();
        let names: Vec<&str> = caps.iter().map(|c| c.name).collect();
        assert!(
            names.contains(&"dependencys_for_deployable"),
            "names = {names:?}"
        );
        assert!(
            names.contains(&"dependencys_for_service"),
            "names = {names:?}"
        );

        // Argument names mirror the GraphQL arg names.
        let by_dep = find_by_name(&caps, "dependencys_for_deployable").unwrap();
        assert_eq!(by_dep.input_schema["required"][0], "deployable_id");
        if let CapabilityHandler::GraphQuery { query_template, .. } = &by_dep.handler {
            assert!(query_template.contains("getByDeployableId(deployable_id:"));
        } else {
            panic!("expected GraphQuery handler");
        }
    }

    #[test]
    fn describe_overrides_default() {
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .describe("list_deployables", "OVERRIDDEN")
            .build();
        let list = find_by_name(&caps, "list_deployables").unwrap();
        assert_eq!(list.description, "OVERRIDDEN");
    }

    #[test]
    fn describe_on_typo_warns() {
        // Capture stderr is hard to do cleanly here; just confirm no
        // capability got rewritten and the chain still finishes.
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .describe("list_deployabls", "WRONG NAME")
            .build();
        // Original description still in place.
        let list = find_by_name(&caps, "list_deployables").unwrap();
        assert_ne!(list.description, "WRONG NAME");
        assert!(list.description.contains("deployable"));
    }

    #[test]
    fn add_custom_appears_in_build() {
        let custom = Capability {
            name: "blast_radius_for_service",
            description: "Find every deployable that depends on a service.",
            input_schema: json!({
                "type": "object",
                "required": ["service_id"],
                "properties": { "service_id": { "type": "string" } }
            }),
            handler: CapabilityHandler::Custom(Arc::new(|_client, _args| -> ToolFuture {
                Box::pin(async move { Ok(json!({ "ok": true })) })
            })),
        };
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .add(custom)
            .build();
        let cap = find_by_name(&caps, "blast_radius_for_service");
        assert!(cap.is_some(), "custom capability should be present");
    }

    #[test]
    fn auto_derives_create_update_delete_for_each_entity() {
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .build();
        let names: Vec<&str> = caps.iter().map(|c| c.name).collect();
        assert!(names.contains(&"create_deployable"), "names = {names:?}");
        assert!(names.contains(&"update_deployable"), "names = {names:?}");
        assert!(names.contains(&"delete_deployable"), "names = {names:?}");

        let create = find_by_name(&caps, "create_deployable").unwrap();
        // Required fields are the schema's non-null fields, federated `team` is
        // excluded, and `id` is excluded (server generates it).
        assert_eq!(create.input_schema["required"][0], "name");
        assert!(create.input_schema["properties"]["name"]["type"] == "string");
        assert!(
            create.input_schema["properties"].get("team").is_none(),
            "federated `team` projection should not be in the create schema"
        );
        assert!(
            create.input_schema["properties"].get("id").is_none(),
            "`id` should not be in the create schema"
        );
        assert!(matches!(
            &create.handler,
            CapabilityHandler::EntityCreate { api_path } if api_path == "/deployable/api"
        ));

        let update = find_by_name(&caps, "update_deployable").unwrap();
        assert_eq!(update.input_schema["required"][0], "id");
        assert!(update.input_schema["properties"]["id"]["type"] == "string");
        // Update should include all payload fields as optional
        assert!(update.input_schema["properties"].get("name").is_some());

        let delete = find_by_name(&caps, "delete_deployable").unwrap();
        assert_eq!(delete.input_schema["required"][0], "id");
        assert!(matches!(
            &delete.handler,
            CapabilityHandler::EntityDelete { api_path } if api_path == "/deployable/api"
        ));
    }

    #[test]
    fn write_capability_descriptions_mention_identity_requirement() {
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .build();
        for name in [
            "create_deployable",
            "update_deployable",
            "delete_deployable",
        ] {
            let cap = find_by_name(&caps, name).unwrap();
            assert!(
                cap.description.contains("MANIFOLD_USER_ID"),
                "{name} description should mention identity requirement, got: {}",
                cap.description
            );
        }
    }

    #[test]
    fn add_replaces_existing_capability_with_same_name() {
        let replacement = Capability {
            name: "list_deployables",
            description: "BESPOKE",
            input_schema: json!({ "type": "object" }),
            handler: CapabilityHandler::Custom(Arc::new(|_client, _args| -> ToolFuture {
                Box::pin(async move { Ok(json!({ "ok": true })) })
            })),
        };
        let caps = CapabilitiesBuilder::new()
            .auto_from_schemas(&[("deployable", "/deployable/graph", DEPLOYABLE_SCHEMA)])
            .add(replacement)
            .build();
        let list = find_by_name(&caps, "list_deployables").unwrap();
        assert_eq!(list.description, "BESPOKE");
        // And only one capability has that name.
        let count = caps.iter().filter(|c| c.name == "list_deployables").count();
        assert_eq!(count, 1);
    }
}

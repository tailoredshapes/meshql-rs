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
use crate::tool::{Tool, ToolFuture, ToolHandler};
use serde_json::Value;
use std::sync::Arc;

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
/// variants are templated dispatchers; `Custom` is an escape hatch for
/// handlers that don't fit a template.
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
    /// POST a templated REST path with an optional body. Both path
    /// placeholders and body string-leaf placeholders resolve from the tool's
    /// input JSON.
    RestPost {
        path_template: String,
        body_template: Option<Value>,
    },
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
    use serde_json::json;

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
}

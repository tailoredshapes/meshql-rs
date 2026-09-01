//! Resolving `include file(...)` where it appears as a *value*.
//!
//! meshql's shared configs are not quite HOCON. In the specification `include`
//! is a statement at the start of an object body; it is never the right-hand
//! side of an assignment. The TypeScript implementation uses
//! `@pushcorn/hocon-parser`, which extends the specification to allow
//! `key = include file(...)`, and every shared config depends on that:
//!
//! ```text
//! schema = include file(graph/farm.graphql)
//! schema = include file(json/farm.schema.json)
//! ```
//!
//! A specification-compliant parser leaves those as literal strings, so this
//! pass finds them and resolves them.
//!
//! **The extension is content-sensitive, and that is the part worth stating.**
//! A `.graphql` file becomes a string; a `.json` file becomes a parsed object.
//! Standard `include` would parse both as HOCON and fail on the GraphQL. The
//! behaviour here matches what `@pushcorn/hocon-parser` actually produces,
//! verified against the farm config rather than inferred from its docs.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum IncludeError {
    #[error("include file({0}): {1}")]
    Unreadable(String, std::io::Error),
    #[error("include file({0}) is not valid JSON: {1}")]
    BadJson(String, serde_json::Error),
}

/// Recognise `include file(...)`, tolerating the quoting styles the shared
/// configs use: bare, single-quoted, and double-quoted paths.
pub fn included_path(value: &str) -> Option<&str> {
    let t = value.trim();
    let inner = t.strip_prefix("include file(")?.strip_suffix(')')?;
    Some(inner.trim().trim_matches(|c| c == '"' || c == '\''))
}

/// Resolve one include against the directory holding the config file.
///
/// The extension decides the shape. Anything that is not `.json` comes back as
/// text, because the only non-JSON includes in practice are GraphQL schemas and
/// a schema is a string to everything downstream.
pub fn resolve(base_dir: &Path, rel: &str) -> Result<serde_json::Value, IncludeError> {
    let path: PathBuf = base_dir.join(rel.trim_start_matches("./"));
    let text =
        std::fs::read_to_string(&path).map_err(|e| IncludeError::Unreadable(rel.to_string(), e))?;

    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        serde_json::from_str(&text).map_err(|e| IncludeError::BadJson(rel.to_string(), e))
    } else {
        Ok(serde_json::Value::String(text))
    }
}

/// Walk a JSON tree and replace every `include file(...)` string.
pub fn resolve_all(value: &mut serde_json::Value, base_dir: &Path) -> Result<(), IncludeError> {
    match value {
        serde_json::Value::String(s) => {
            if let Some(rel) = included_path(s) {
                *value = resolve(base_dir, rel)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                resolve_all(item, base_dir)?;
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                resolve_all(v, base_dir)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_quoting_styles_the_shared_configs_use() {
        assert_eq!(
            included_path("include file(graph/farm.graphql)"),
            Some("graph/farm.graphql")
        );
        assert_eq!(
            included_path("include file(./graph/coop.graphql)"),
            Some("./graph/coop.graphql")
        );
        assert_eq!(
            included_path(r#"include file("json/farm.schema.json")"#),
            Some("json/farm.schema.json")
        );
        assert_eq!(included_path("  include file(x.json)  "), Some("x.json"));
    }

    #[test]
    fn an_ordinary_string_is_not_an_include() {
        assert_eq!(included_path("scalar Date"), None);
        assert_eq!(included_path("http://farm:3030/coop/graph"), None);
    }

    /// The content-sensitive half. GraphQL becomes a string, JSON becomes an
    /// object — standard `include` would try to parse both as HOCON.
    #[test]
    fn extension_decides_text_or_parsed_json() {
        let dir = std::env::temp_dir().join("meshql-include-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.graphql"), "type Query { x: Int }").unwrap();
        std::fs::write(dir.join("b.json"), r#"{"type":"object"}"#).unwrap();

        assert_eq!(
            resolve(&dir, "a.graphql").unwrap(),
            serde_json::Value::String("type Query { x: Int }".into())
        );
        assert!(resolve(&dir, "b.json").unwrap().is_object());
        // A leading `./` is the same path.
        assert!(resolve(&dir, "./b.json").unwrap().is_object());
    }

    #[test]
    fn resolution_reaches_into_arrays_and_nested_objects() {
        let dir = std::env::temp_dir().join("meshql-include-nested");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.graphql"), "type Q { a: Int }").unwrap();

        let mut v = serde_json::json!({
            "graphlettes": [{ "schema": "include file(s.graphql)" }],
            "untouched": "plain"
        });
        resolve_all(&mut v, &dir).unwrap();
        assert_eq!(v["graphlettes"][0]["schema"], "type Q { a: Int }");
        assert_eq!(v["untouched"], "plain");
    }
}

//! A restlette's JSON Schema, enforced.
//!
//! Every restlette is configured with a JSON Schema for its documents, and
//! meshql-java (networknt) and meshobj (ajv) both refuse a write that does not
//! conform: 400, nothing stored. meshql-rs carried the schema on
//! `RestletteConfig` and never looked at it, so any document was accepted. This
//! turns the schema into the restlette's `ValidatorFn`, compiled once.
//!
//! The draft is the one the schema names in `$schema`, else draft 7 (what
//! meshql-java's `SpecVersion.V7` assumes). Formats are checked, as networknt
//! does by default and meshobj does with ajv-formats. An empty schema (`{}`) or
//! `null` constrains nothing, and yields no validator at all.

use crate::routes::{ValidatorContext, ValidatorFn};
use meshql_core::Stash;
use std::sync::Arc;

/// At most this many violations are named in one refusal.
const MAX_REPORTED: usize = 5;

/// The validator a restlette's JSON Schema describes, or `None` when the schema
/// constrains nothing. A schema that does not compile is an error, so a
/// misconfigured restlette fails at start-up rather than accepting everything.
pub fn schema_validator(schema: &serde_json::Value) -> Result<Option<ValidatorFn>, String> {
    let unconstrained = schema.is_null() || schema.as_object().is_some_and(|o| o.is_empty());
    if unconstrained {
        return Ok(None);
    }

    let mut options = jsonschema::options();
    if schema.get("$schema").is_none() {
        options.with_draft(jsonschema::Draft::Draft7);
    }
    options.should_validate_formats(true);
    let compiled = options
        .build(schema)
        .map_err(|e| format!("the JSON Schema does not compile: {e}"))?;
    let compiled = Arc::new(compiled);

    Ok(Some(Arc::new(
        move |payload: &Stash, _ctx: &ValidatorContext| {
            let document = serde_json::Value::Object(payload.clone());
            let problems: Vec<String> = compiled
                .iter_errors(&document)
                .take(MAX_REPORTED)
                .map(|e| {
                    let at = e.instance_path.to_string();
                    if at.is_empty() {
                        e.to_string()
                    } else {
                        format!("{at}: {e}")
                    }
                })
                .collect();
            if problems.is_empty() {
                Ok(())
            } else {
                Err(format!("Invalid payload: {}", problems.join("; ")))
            }
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(schema: serde_json::Value, doc: serde_json::Value) -> Result<(), String> {
        let v = schema_validator(&schema)
            .unwrap()
            .expect("a constraining schema");
        v(doc.as_object().unwrap(), &ValidatorContext::default())
    }

    #[test]
    fn an_empty_schema_constrains_nothing() {
        assert!(schema_validator(&json!({})).unwrap().is_none());
        assert!(schema_validator(&serde_json::Value::Null)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_schema_that_does_not_compile_is_an_error() {
        let err = match schema_validator(&json!({"type": 5})) {
            Err(e) => e,
            Ok(_) => panic!("a schema with `type: 5` must not compile"),
        };
        assert!(err.contains("does not compile"), "{err}");
    }

    #[test]
    fn violations_are_named_with_where_they_are() {
        let schema = json!({"type": "object", "required": ["name"],
                            "properties": {"eggs": {"type": "integer"}}});
        assert!(check(schema.clone(), json!({"name": "a", "eggs": 2})).is_ok());
        let err = check(schema.clone(), json!({"eggs": "three"})).unwrap_err();
        assert!(err.starts_with("Invalid payload: "), "{err}");
        assert!(err.contains("name"), "the missing property is named: {err}");
        assert!(
            err.contains("/eggs"),
            "the wrong-typed property is located: {err}"
        );
    }

    #[test]
    fn formats_are_checked_and_draft_7_is_the_default() {
        let schema = json!({"properties": {"laid": {"type": "string", "format": "date"}}});
        assert!(check(schema.clone(), json!({"laid": "2026-10-07"})).is_ok());
        assert!(check(schema, json!({"laid": "yesterday"})).is_err());
        // Draft 7's `dependencies` (split in 2019-09) still applies without a `$schema`.
        let d7 = json!({"dependencies": {"a": ["b"]}});
        assert!(check(d7, json!({"a": 1})).is_err());
    }
}

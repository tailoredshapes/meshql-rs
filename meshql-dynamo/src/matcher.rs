//! Predicate matching, replicating `meshql-merkql/src/matcher.rs` exactly.
//!
//! A rendered query template is matched against the record JSON
//! `{"id": <envelope id>, "payload": {…}}`, which is what gives the two — and
//! only two — recognised key shapes:
//!
//! - `"id"` → the envelope id,
//! - `"payload.<path…>"` → a dot path into the payload, nesting allowed.
//!
//! **An unrecognised key matches nothing.** The dot path resolves to `None` and
//! the whole record is rejected, so a query carrying a bare payload field (e.g.
//! `{"kind": "tool"}` instead of `{"payload.kind": "tool"}`) returns an empty
//! result set. This is deliberate and is the *opposite* of how the SQL adapters
//! fail: they skip an unrecognised condition, so a lone bad condition returns
//! **every** record. `sociallymeshy/docs/architecture.md` §2.3 requires this
//! adapter to fail like merkql — empty — because failing wide on a bad template
//! is an authorization-shaped bug wearing a typo's clothes.

use serde_json::Value;

/// Match a record's JSON representation against a dot-notation query.
///
/// - Keys may use dot notation for nested fields (e.g. `"payload.name"`).
/// - Values must equal the record's value at that path.
/// - An empty query `{}` matches everything.
/// - All query keys must match (AND semantics).
/// - A key whose path does not resolve rejects the record.
pub fn matches(record_json: &Value, query: &Value) -> bool {
    let query_obj = match query.as_object() {
        Some(o) => o,
        None => return false,
    };

    for (key, expected) in query_obj {
        let path: Vec<&str> = key.split('.').collect();
        match get_path(record_json, &path) {
            Some(actual) => {
                if actual != expected {
                    return false;
                }
            }
            // Fail *empty*, not wide. See the module docs.
            None => return false,
        }
    }
    true
}

fn get_path<'a>(val: &'a Value, path: &[&str]) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(val);
    }
    match val {
        Value::Object(obj) => {
            let head = path[0];
            let rest = &path[1..];
            obj.get(head).and_then(|child| get_path(child, rest))
        }
        _ => None,
    }
}

/// The record JSON the matcher walks. Built here rather than inline so the
/// shape — `id` top-level, everything else under `payload` — has exactly one
/// definition.
pub fn record_json(env: &meshql_core::Envelope) -> Value {
    serde_json::json!({
        "id": env.id,
        "payload": env.payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record() -> Value {
        json!({
            "id": "s-id-1",
            "payload": {"name": "alpha", "count": 10, "type": "typeA",
                        "nested": {"deep": {"leaf": "found"}}}
        })
    }

    #[test]
    fn empty_query_matches_all() {
        assert!(matches(&record(), &json!({})));
    }

    #[test]
    fn matches_the_envelope_id() {
        assert!(matches(&record(), &json!({"id": "s-id-1"})));
        assert!(!matches(&record(), &json!({"id": "s-id-2"})));
    }

    #[test]
    fn matches_a_payload_dot_path() {
        assert!(matches(&record(), &json!({"payload.name": "alpha"})));
        assert!(!matches(&record(), &json!({"payload.name": "beta"})));
    }

    #[test]
    fn matches_a_deeply_nested_payload_path() {
        assert!(matches(
            &record(),
            &json!({"payload.nested.deep.leaf": "found"})
        ));
        assert!(!matches(
            &record(),
            &json!({"payload.nested.deep.leaf": "lost"})
        ));
        assert!(
            !matches(&record(), &json!({"payload.nested.missing.leaf": "found"})),
            "a path that dead-ends must reject, not match"
        );
    }

    #[test]
    fn conditions_are_anded() {
        assert!(matches(
            &record(),
            &json!({"payload.name": "alpha", "payload.type": "typeA"})
        ));
        assert!(!matches(
            &record(),
            &json!({"payload.name": "alpha", "payload.type": "typeB"})
        ));
    }

    #[test]
    fn a_bare_payload_field_matches_nothing() {
        // The whole point: `{"kind": "tool"}` must not behave like
        // `{"payload.kind": "tool"}`, and must not behave like `{}` either.
        let rec = json!({"id": "x", "payload": {"kind": "tool"}});
        assert!(
            !matches(&rec, &json!({"kind": "tool"})),
            "a payload field written without the `payload.` prefix must match nothing"
        );
        assert!(matches(&rec, &json!({"payload.kind": "tool"})));
    }

    #[test]
    fn an_unknown_top_level_key_matches_nothing() {
        assert!(!matches(&record(), &json!({"deleted": false})));
        assert!(!matches(&record(), &json!({"authorized_tokens": []})));
        assert!(
            !matches(&record(), &json!({"createdAt": "2024-01-01T00:00:00Z"})),
            "`createdAt` is a *result* field, not a queryable one"
        );
    }

    #[test]
    fn record_json_puts_id_on_top_and_payload_underneath() {
        let mut payload = meshql_core::Stash::new();
        payload.insert("name".to_string(), json!("alpha"));
        let env = meshql_core::Envelope::new("id-1", payload, vec![]);
        assert_eq!(
            record_json(&env),
            json!({"id": "id-1", "payload": {"name": "alpha"}})
        );
    }
}

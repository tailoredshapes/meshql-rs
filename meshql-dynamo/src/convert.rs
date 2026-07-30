//! JSON ↔ DynamoDB `AttributeValue` conversion.
//!
//! Kept in its own module with round-trip unit tests because a lossy number
//! conversion is the classic silent failure in a DynamoDB adapter: an integer
//! that comes back as `1.0`, or a float that comes back truncated, corrupts a
//! payload without erroring anywhere.
//!
//! The mapping is total in the JSON → DynamoDB direction and partial in the
//! other: DynamoDB has types JSON does not (`B`, `SS`, `NS`, `BS`), and this
//! adapter never writes them, so reading one is a storage error rather than a
//! guess.

use aws_sdk_dynamodb::types::AttributeValue;
use meshql_core::{MeshqlError, Result};
use serde_json::{Number, Value};
use std::collections::HashMap;

/// Render a JSON number as the decimal string DynamoDB's `N` type wants,
/// preserving integer-vs-float: an `i64`/`u64` stays integral, only a genuine
/// float is rendered with a fractional part.
fn number_to_string(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) => f.to_string(),
        // serde_json cannot produce a Number that is none of the three.
        None => "0".to_string(),
    }
}

/// Parse a DynamoDB `N` back into a JSON number, integral-first so a value
/// written as `7` does not come back as `7.0`.
fn string_to_number(s: &str) -> Result<Number> {
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Number::from(i));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Ok(Number::from(u));
    }
    let f: f64 = s
        .parse()
        .map_err(|_| MeshqlError::Parse(format!("not a DynamoDB number: {s:?}")))?;
    Number::from_f64(f).ok_or_else(|| MeshqlError::Parse(format!("non-finite number: {s:?}")))
}

/// JSON → `AttributeValue`.
pub fn json_to_attr(value: &Value) -> AttributeValue {
    match value {
        Value::Null => AttributeValue::Null(true),
        Value::Bool(b) => AttributeValue::Bool(*b),
        Value::Number(n) => AttributeValue::N(number_to_string(n)),
        Value::String(s) => AttributeValue::S(s.clone()),
        Value::Array(items) => AttributeValue::L(items.iter().map(json_to_attr).collect()),
        Value::Object(obj) => AttributeValue::M(
            obj.iter()
                .map(|(k, v)| (k.clone(), json_to_attr(v)))
                .collect(),
        ),
    }
}

/// `AttributeValue` → JSON.
pub fn attr_to_json(attr: &AttributeValue) -> Result<Value> {
    match attr {
        AttributeValue::Null(_) => Ok(Value::Null),
        AttributeValue::Bool(b) => Ok(Value::Bool(*b)),
        AttributeValue::N(n) => Ok(Value::Number(string_to_number(n)?)),
        AttributeValue::S(s) => Ok(Value::String(s.clone())),
        AttributeValue::L(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(attr_to_json(item)?);
            }
            Ok(Value::Array(out))
        }
        AttributeValue::M(map) => Ok(Value::Object(map_to_object(map)?)),
        other => Err(MeshqlError::Parse(format!(
            "meshql-dynamo does not write this DynamoDB type and cannot read it: {other:?}"
        ))),
    }
}

/// A DynamoDB `M` (or a whole item) → a `serde_json` object, which is what a
/// `Stash` is.
pub fn map_to_object(map: &HashMap<String, AttributeValue>) -> Result<meshql_core::Stash> {
    let mut out = meshql_core::Stash::new();
    for (k, v) in map {
        out.insert(k.clone(), attr_to_json(v)?);
    }
    Ok(out)
}

/// A `Stash` → a DynamoDB `M` body.
pub fn object_to_map(obj: &meshql_core::Stash) -> HashMap<String, AttributeValue> {
    obj.iter()
        .map(|(k, v)| (k.clone(), json_to_attr(v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A payload holding every JSON shape that has ever silently changed on the
    /// way through a DynamoDB adapter: a nested object, an array, an integer, a
    /// negative integer, a float, a bool, a null and an empty string.
    fn kitchen_sink() -> Value {
        json!({
            "name": "alpha",
            "empty": "",
            "count": 42,
            "negative": -7,
            "big": 9_007_199_254_740_993i64,
            "ratio": 1.5,
            "tiny": -0.25,
            "flag": true,
            "off": false,
            "nothing": null,
            "tags": ["a", "b", 3, null, {"deep": true}],
            "nested": {
                "inner": {"deeper": [1, 2.5, "three"]},
                "blank": "",
                "zero": 0
            },
            "empty_list": [],
            "empty_map": {}
        })
    }

    #[test]
    fn round_trips_json_through_attribute_values() {
        let original = kitchen_sink();
        let attr = json_to_attr(&original);
        let back = attr_to_json(&attr).expect("kitchen sink round-trips");
        assert_eq!(back, original);
    }

    #[test]
    fn round_trips_a_stash_through_an_item_map() {
        let original = kitchen_sink().as_object().unwrap().clone();
        let map = object_to_map(&original);
        let back = map_to_object(&map).expect("stash round-trips");
        assert_eq!(back, original);
    }

    #[test]
    fn integers_stay_integers_and_floats_stay_floats() {
        // The lossy-number failure mode: 42 must not come back as 42.0, and 1.5
        // must not come back as 1 or 2.
        let attr = json_to_attr(&json!(42));
        assert_eq!(attr, AttributeValue::N("42".to_string()));
        let back = attr_to_json(&attr).unwrap();
        assert!(back.is_i64(), "42 came back as {back}");
        assert_eq!(back, json!(42));

        let attr = json_to_attr(&json!(1.5));
        assert_eq!(attr, AttributeValue::N("1.5".to_string()));
        let back = attr_to_json(&attr).unwrap();
        assert!(back.is_f64(), "1.5 came back as {back}");
        assert_eq!(back, json!(1.5));
    }

    #[test]
    fn empty_string_survives() {
        // DynamoDB rejects an empty string in a *key* attribute but has allowed
        // it in a value since 2020; the payload is a value, so this must work
        // rather than be silently coerced to NULL.
        let attr = json_to_attr(&json!(""));
        assert_eq!(attr, AttributeValue::S(String::new()));
        assert_eq!(attr_to_json(&attr).unwrap(), json!(""));
    }

    #[test]
    fn unsupported_dynamo_types_error_rather_than_guess() {
        let attr = AttributeValue::Ss(vec!["a".to_string()]);
        assert!(attr_to_json(&attr).is_err());
    }
}

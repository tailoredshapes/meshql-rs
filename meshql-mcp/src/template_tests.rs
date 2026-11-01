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

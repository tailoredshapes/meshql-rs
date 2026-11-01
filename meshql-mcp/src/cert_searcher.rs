//! Fixture-only typed query binding. The global Handlebars contract is unchanged.
//! Literal JSON strings encode braces so data cannot become template syntax.
use meshql_core::{MeshqlError, Result, Searcher, Session, Stash};
use meshql_sqlite::SqliteSearcher;
use serde_json::Value;
use std::sync::Arc;
pub(crate) struct TypedSearcher(pub Arc<SqliteSearcher>);
fn literal(value: &Value) -> String {
    match value {
        Value::String(s) => serde_json::to_string(s)
            .unwrap()
            .replace('{', "\\u007b")
            .replace('}', "\\u007d"),
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(k, v)| format!("{}:{}", literal(&Value::String(k.clone())), literal(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(xs) => format!("[{}]", xs.iter().map(literal).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}
fn bind(template: &str, args: &Stash) -> Result<String> {
    let mut predicate: Value =
        serde_json::from_str(template).map_err(|e| MeshqlError::Parse(e.to_string()))?;
    if let Some(fields) = predicate.as_object_mut() {
        for value in fields.values_mut() {
            if let Some(name) = value
                .as_str()
                .and_then(|s| s.strip_prefix("{{"))
                .and_then(|s| s.strip_suffix("}}"))
            {
                *value = args
                    .get(name)
                    .cloned()
                    .ok_or_else(|| MeshqlError::Parse("missing typed query argument".into()))?;
            }
        }
    }
    Ok(literal(&predicate))
}
#[async_trait::async_trait]
impl Searcher for TypedSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>> {
        self.0
            .find(&bind(template, args)?, &Stash::new(), session, at)
            .await
    }
    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>> {
        self.0
            .find_all(&bind(template, args)?, &Stash::new(), session, at)
            .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literals_preserve_unicode_quotes_and_template_markers() {
        let value = serde_json::json!("café \"quoted\" \\ path\n雪 {{danger}}");
        let args = serde_json::Map::from_iter([("name".into(), value.clone())]);
        let template = bind(r#"{"payload.name":"{{name}}"}"#, &args).unwrap();
        assert!(!template.contains("{{"));
        assert_eq!(
            serde_json::from_str::<Value>(&template).unwrap()["payload.name"],
            value
        );
    }
}

//! Compatibility templates are data substitutions, never arbitrary query syntax.
use anyhow::{bail, ensure, Result};
use serde_json::Value;

fn placeholder(text: &str) -> Option<(&str, usize)> {
    let tail = text.strip_prefix('{')?;
    let end = tail.find('}')?;
    let key = &tail[..end];
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some((key, end + 2))
}
pub(crate) fn segment(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty() && value != "." && value != ".." && !value.chars().any(char::is_control),
        "invalid path argument"
    );
    Ok(
        percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
            .to_string(),
    )
}
fn render(template: &str, args: &Value, mode: u8) -> Result<String> {
    let mut out = String::new();
    let mut i = 0;
    let mut quoted = false;
    let mut escaped = false;
    while i < template.len() {
        if let Some((key, len)) = placeholder(&template[i..]) {
            ensure!(
                mode != 0 || !escaped,
                "escaped GraphQL placeholders are unsupported"
            );
            let value = args
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("missing argument {key}"))?;
            let rendered = if mode == 1 {
                segment(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("path argument must be string"))?,
                )?
            } else if let Some(s) = value.as_str() {
                if mode == 0 {
                    ensure!(
                        quoted,
                        "string placeholders require a quoted GraphQL literal"
                    );
                    let json = serde_json::to_string(s)?;
                    json[1..json.len() - 1].into()
                } else {
                    s.into()
                }
            } else if value.is_array() || value.is_object() {
                bail!("compound values require GraphQL variables")
            } else {
                value.to_string()
            };
            out.push_str(&rendered);
            i += len;
            continue;
        }
        let c = template[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
        if c == '"' && !escaped {
            quoted = !quoted;
        }
        escaped = c == '\\' && !escaped;
    }
    Ok(out)
}
pub(crate) fn substitute(template: &str, args: &Value) -> Result<String> {
    render(template, args, 0)
}
pub(crate) fn substitute_path(template: &str, args: &Value) -> Result<String> {
    render(template, args, 1)
}
pub(crate) fn substitute_value(template: &Value, args: &Value) -> Result<Value> {
    match template {
        Value::String(s) => {
            if let Some((key, len)) = placeholder(s) {
                if len == s.len() {
                    return args
                        .get(key)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing argument {key}"));
                }
            }
            Ok(Value::String(render(s, args, 2)?))
        }
        Value::Array(xs) => Ok(Value::Array(
            xs.iter()
                .map(|x| substitute_value(x, args))
                .collect::<Result<_>>()?,
        )),
        Value::Object(xs) => Ok(Value::Object(
            xs.iter()
                .map(|(k, v)| Ok((k.clone(), substitute_value(v, args)?)))
                .collect::<Result<_>>()?,
        )),
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
#[path = "template_tests.rs"]
mod tests;

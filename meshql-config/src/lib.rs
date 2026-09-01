//! Load a meshql deployment from the HOCON config the other implementations
//! already share.
//!
//! TypeScript and Java run the farm example from one byte-identical
//! `config.conf`. Rust could not read it at all, so its version of that example
//! was reimplemented in code — and diverged, quietly, from the day it was
//! written. This crate closes that: the same file, unmodified, produces the
//! same deployment.
//!
//! The config format is HOCON plus one extension. See [`include`] for what that
//! extension is and why a specification-compliant parser alone is not enough.

pub mod build;
pub mod include;
pub mod model;
pub mod schema;
pub mod storage;

use std::collections::HashMap;
use std::path::Path;

pub use model::{Deployment, GraphletteDef, ResolverDef, RestletteDef, RootConfigDef, StorageDef};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parsing {0}: {1}")]
    Hocon(String, String),
    #[error(transparent)]
    Include(#[from] include::IncludeError),
    #[error("{0}")]
    Shape(String),
}

/// Parse a config file, resolving environment substitutions from the process
/// environment and `include file(...)` relative to the config's own directory.
pub fn load(path: impl AsRef<Path>) -> Result<Deployment, ConfigError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    load_with_env(path, &env)
}

/// The same, against an explicit environment. Tests use this so a run does not
/// depend on what happens to be exported.
pub fn load_with_env(
    path: impl AsRef<Path>,
    env: &HashMap<String, String>,
) -> Result<Deployment, ConfigError> {
    let path = path.as_ref();
    let display = path.display().to_string();

    let config = hocon::Parser::new()
        .parse_file_with_env(path, env)
        .map_err(|e| ConfigError::Hocon(display.clone(), e.to_string()))?;

    // The top-level database blocks (`farmDB` and friends) are read only
    // through `${farmDB}` substitutions, which the parser has already resolved
    // into the meshlettes below. So only the four keys a deployment actually
    // needs are lifted.
    let mut json = serde_json::Map::new();
    for key in ["port", "url", "graphlettes", "restlettes"] {
        if let Some(v) = config.get(key) {
            json.insert(key.to_string(), to_json(v));
        }
    }
    let mut json = serde_json::Value::Object(json);

    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    include::resolve_all(&mut json, base_dir)?;

    model::from_json(&json).map_err(ConfigError::Shape)
}

/// Convert the parser's tree to `serde_json`, which everything downstream
/// already speaks.
///
/// A scalar carries both its raw text and the type the parser inferred. The raw
/// text is authoritative for strings, because HOCON concatenation produces
/// strings that happen to look numeric — `port` in the shared farm config
/// resolves to the string `"3030"`, and the TypeScript parser reports it the
/// same way.
fn to_json(value: &hocon::HoconValue) -> serde_json::Value {
    use hocon::{HoconValue, ScalarType};
    match value {
        HoconValue::Object(map) => {
            serde_json::Value::Object(map.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
        HoconValue::Array(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        HoconValue::Scalar(s) => match s.value_type {
            ScalarType::Boolean => serde_json::Value::Bool(s.raw == "true"),
            ScalarType::Null => serde_json::Value::Null,
            ScalarType::Number => s
                .raw
                .parse::<serde_json::Number>()
                .map(serde_json::Value::Number)
                .unwrap_or_else(|_| serde_json::Value::String(s.raw.clone())),
            ScalarType::String => serde_json::Value::String(s.raw.clone()),
            // The parser marks its own unresolved placeholders with variants
            // this crate never sees on the fused parse path.
            _ => serde_json::Value::String(s.raw.clone()),
        },
        _ => serde_json::Value::Null,
    }
}

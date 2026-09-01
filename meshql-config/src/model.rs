//! The deployment a config describes, in the shape meshql-rs already builds by
//! hand.
//!
//! Field names mirror the shared config exactly, including its camelCase, so a
//! reader can hold the file and this file side by side.

use serde::{Deserialize, Serialize};

/// Which store backs one meshlette. `type` selects the adapter; the remaining
/// fields are that adapter's own, kept as raw JSON because each needs different
/// ones and this crate has no business knowing them all.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageDef {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QueryDef {
    pub name: String,
    /// The payload field a caller passes. Defaults to `id`, matching the
    /// TypeScript loader.
    #[serde(default)]
    pub id: Option<String>,
    pub query: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolverDef {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "queryName")]
    pub query_name: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RootConfigDef {
    #[serde(default)]
    pub singletons: Vec<QueryDef>,
    #[serde(default)]
    pub vectors: Vec<QueryDef>,
    #[serde(default)]
    pub resolvers: Vec<ResolverDef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphletteDef {
    pub path: String,
    pub storage: StorageDef,
    /// The GraphQL schema text. Arrives through `include file(x.graphql)`,
    /// which resolves to a string rather than a parsed document.
    pub schema: String,
    #[serde(rename = "rootConfig", default)]
    pub root_config: RootConfigDef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestletteDef {
    pub path: String,
    pub storage: StorageDef,
    /// The JSON Schema. Arrives through `include file(x.json)`, which resolves
    /// to a parsed object rather than a string.
    pub schema: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deployment {
    /// A string, not a number. HOCON concatenation produces strings that look
    /// numeric, and the shared farm config resolves `port` to `"3030"`.
    /// TypeScript reports it the same way.
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub graphlettes: Vec<GraphletteDef>,
    #[serde(default)]
    pub restlettes: Vec<RestletteDef>,
}

impl Deployment {
    /// The port as a number, for binding. Absent or unparseable means the
    /// caller decides.
    pub fn port_number(&self) -> Option<u16> {
        self.port.as_ref()?.parse().ok()
    }
}

pub fn from_json(value: &serde_json::Value) -> Result<Deployment, String> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}

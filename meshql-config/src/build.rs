//! Turning a parsed deployment into the `ServerConfig` meshql-rs serves.
//!
//! This is where the config's vocabulary meets Rust's. Two mismatches have to
//! be resolved rather than papered over, and both are recorded here because a
//! silent accommodation is how the implementations drifted in the first place.

use crate::schema;
use crate::storage::{self, Store};
use crate::{ConfigError, Deployment, GraphletteDef, ResolverDef};
use meshql_core::{Auth, GraphletteConfig, RootConfig, ServerConfig};
use std::sync::Arc;

/// What a built deployment needs beyond its `ServerConfig`: the stores the
/// restlettes write through, paired with the paths they serve.
pub struct Built {
    pub server: ServerConfig,
    pub restlettes: Vec<(String, serde_json::Value, Store)>,
}

/// A resolver the config declares that Rust cannot yet serve.
///
/// The shared farm config carries `name = "hens.layReports"` — a resolver on a
/// *nested* type, reached through another resolver's result. TypeScript hands
/// its flat resolver list to GraphQL and lets the type system walk the path;
/// Rust's `RootConfig` attaches a resolver to one field of one root type and
/// has no notion of a path.
///
/// This is reported rather than skipped. A loader that quietly dropped the
/// resolver would produce a server that answers a federated query with `null`
/// and no explanation — which is exactly the class of divergence that let the
/// implementations grow apart unnoticed.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedResolver {
    pub graphlette: String,
    pub name: String,
    pub reason: &'static str,
}

pub const NESTED_RESOLVER: &str =
    "a resolver on a nested type. meshql-rs attaches resolvers to fields of the \
     root type only, so a dotted path has nowhere to go";

/// Build a deployment, reporting resolvers that cannot be served.
///
/// The unsupported list is returned rather than raised, so a caller can decide:
/// a test asserting full compatibility fails on a non-empty list, while an
/// operator running a config that happens not to exercise the nested resolver
/// gets a server plus a warning.
pub async fn build(
    deployment: &Deployment,
    auth: Arc<dyn Auth>,
) -> Result<(Built, Vec<UnsupportedResolver>), ConfigError> {
    let mut graphlettes = Vec::new();
    let mut unsupported = Vec::new();

    for g in &deployment.graphlettes {
        let store = storage::open(&g.storage, auth.clone()).await?;
        let (root_config, mut skipped) = root_config_for(g)?;
        unsupported.append(&mut skipped);
        graphlettes.push(GraphletteConfig {
            path: g.path.clone(),
            schema_text: g.schema.clone(),
            root_config,
            searcher: store.searcher,
        });
    }

    let mut restlettes = Vec::new();
    for r in &deployment.restlettes {
        let store = storage::open(&r.storage, auth.clone()).await?;
        restlettes.push((r.path.clone(), r.schema.clone(), store));
    }

    let server = ServerConfig {
        port: deployment.port_number().unwrap_or(3000),
        graphlettes,
        restlettes: vec![],
    };
    Ok((Built { server, restlettes }, unsupported))
}

fn root_config_for(
    g: &GraphletteDef,
) -> Result<(RootConfig, Vec<UnsupportedResolver>), ConfigError> {
    let mut b = RootConfig::builder();

    for q in &g.root_config.singletons {
        b = b.singleton(&q.name, &q.query);
    }
    for q in &g.root_config.vectors {
        b = b.vector(&q.name, &q.query);
    }

    // The config's `resolvers` list is flat; Rust needs to know which are lists.
    // The schema is the only thing that knows, so ask it.
    let root = schema::root_type(&g.schema).ok_or_else(|| {
        ConfigError::Shape(format!(
            "{}: the schema has no Query type, so the entity it serves is unknown",
            g.path
        ))
    })?;

    let mut unsupported = Vec::new();
    for r in &g.root_config.resolvers {
        if r.name.contains('.') {
            unsupported.push(UnsupportedResolver {
                graphlette: g.path.clone(),
                name: r.name.clone(),
                reason: NESTED_RESOLVER,
            });
            continue;
        }
        b = attach(b, &root, &g.schema, r, &g.path)?;
    }

    Ok((b.build(), unsupported))
}

fn attach(
    b: meshql_core::RootConfigBuilder,
    root: &str,
    schema_text: &str,
    r: &ResolverDef,
    path: &str,
) -> Result<meshql_core::RootConfigBuilder, ConfigError> {
    let is_list = schema::field_is_list(schema_text, root, &r.name).ok_or_else(|| {
        ConfigError::Shape(format!(
            "{path}: resolver \"{}\" has no matching field on type {root}",
            r.name
        ))
    })?;
    let fk = r.id.as_deref();
    Ok(if is_list {
        b.vector_resolver(&r.name, fk, &r.query_name, &r.url)
    } else {
        b.singleton_resolver(&r.name, fk, &r.query_name, &r.url)
    })
}

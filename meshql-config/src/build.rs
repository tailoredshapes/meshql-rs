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

/// A resolver the config declares that meshql-rs cannot serve.
///
/// Nested resolvers themselves are fine: `schema_builder` matches a dotted name
/// by its suffix, so `hens.layReports` attaches to `layReports` wherever that
/// field is built. What is *not* fine is a nested **singleton** resolver. The
/// suffix fallback exists on vector resolvers and internal vector resolvers
/// only; singletons match exactly, so a dotted singleton silently becomes a
/// null field with no error.
///
/// That is reported rather than skipped. A loader that let it through would
/// produce a server answering a federated query with `null` and no explanation,
/// which is the class of divergence this exercise has been about.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedResolver {
    pub graphlette: String,
    pub name: String,
    pub reason: &'static str,
}

pub const NESTED_SINGLETON: &str =
    "a nested singleton resolver. meshql-graphlette matches a dotted resolver \
     name by its suffix for vector resolvers only, so a nested singleton would \
     resolve to null with no error";

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
        // A dotted name attaches to the last segment, on the type the path
        // walks to — `hens.layReports` is `layReports` on `Hen`, not on `Coop`.
        let (owner, field) = schema::walk_path(&g.schema, &root, &r.name).ok_or_else(|| {
            ConfigError::Shape(format!(
                "{}: resolver \"{}\" names a path that does not exist in the schema",
                g.path, r.name
            ))
        })?;

        let is_list = schema::field_is_list(&g.schema, &owner, field).ok_or_else(|| {
            ConfigError::Shape(format!(
                "{}: resolver \"{}\" has no matching field on type {owner}",
                g.path, r.name
            ))
        })?;

        if r.name.contains('.') && !is_list {
            unsupported.push(UnsupportedResolver {
                graphlette: g.path.clone(),
                name: r.name.clone(),
                reason: NESTED_SINGLETON,
            });
            continue;
        }

        let fk = r.id.as_deref();
        b = if is_list {
            b.vector_resolver(&r.name, fk, &r.query_name, &r.url)
        } else {
            b.singleton_resolver(&r.name, fk, &r.query_name, &r.url)
        };
    }

    Ok((b.build(), unsupported))
}

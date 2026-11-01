//! Standard stdio entry point: catalogue from configuration, execution through
//! the running deployment's REST/GraphQL surfaces.
#[cfg(feature = "cert-fixture")]
use crate::CapabilityHandler;
use crate::{CapabilitiesBuilder, Capability, McpServerConfig, MeshqlClient, MeshqlMcpServer};
#[cfg(feature = "cert-fixture")]
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
pub fn options(args: impl IntoIterator<Item = String>) -> anyhow::Result<HashMap<String, String>> {
    let mut args = args.into_iter();
    let mut out = HashMap::new();
    while let Some(key) = args.next() {
        anyhow::ensure!(key.starts_with("--"), "expected named option");
        let value = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing option value"))?;
        anyhow::ensure!(out.insert(key, value).is_none(), "duplicate option");
    }
    Ok(out)
}
fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}
pub async fn run(args: HashMap<String, String>) -> anyhow::Result<()> {
    let config = meshql_config::load(args.get("--config").ok_or_else(|| {
        anyhow::anyhow!("usage: meshql-mcp --config FILE --backend URL [--identity USER]")
    })?)?;
    let backend = args
        .get("--backend")
        .ok_or_else(|| anyhow::anyhow!("backend URL required"))?;
    let mut builder = CapabilitiesBuilder::new();
    for graph in config.graphlettes {
        let entity = graph
            .path
            .trim_end_matches("/graph")
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("invalid graph path"))?
            .to_owned();
        builder =
            builder.auto_from_schemas(&[(leak(entity), leak(graph.path), leak(graph.schema))]);
    }
    for rest in config.restlettes {
        let entity = rest
            .path
            .trim_end_matches("/api")
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("invalid REST path"))?
            .to_owned();
        builder = builder.with_rest_schema(leak(entity), &rest.path, rest.schema);
    }
    serve(
        backend,
        args.get("--identity")
            .map(String::as_str)
            .unwrap_or("anonymous"),
        builder.try_build()?,
    )
    .await
}
pub(crate) async fn serve(
    backend: &str,
    identity: &str,
    capabilities: Vec<Capability>,
) -> anyhow::Result<()> {
    let identity = crate::client::Identity {
        user_id: (identity != "anonymous").then(|| identity.to_owned()),
        groups: None,
    };
    let client = Arc::new(MeshqlClient::new(backend).with_identity(identity));
    MeshqlMcpServer::try_new(McpServerConfig {
        server_name: "meshql-mcp".into(),
        server_version: env!("CARGO_PKG_VERSION").into(),
        client,
        capabilities,
    })?
    .serve_stdio()
    .await
}
#[cfg(feature = "cert-fixture")]
pub(crate) async fn fixture(args: &HashMap<String, String>) -> anyhow::Result<()> {
    let base = &args["--backend"];
    let dir = std::path::Path::new(&args["--fixtures"]);
    let sdl = std::fs::read_to_string(dir.join("widget.graphql"))?;
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("widget.schema.json"))?)?;
    let capabilities = CapabilitiesBuilder::new()
        .auto_from_schemas(&[("widget", "/widget/graph", leak(sdl))])
        .with_rest_schema("widget", "/widget/api", schema)
        .add(Capability {
            name: "custom_scalar",
            description: "Fixture scalar operation",
            input_schema: json!({"type":"object","additionalProperties":false}),
            handler: CapabilityHandler::Custom(Arc::new(|_, _| {
                Box::pin(async { Ok(Value::from(7)) })
            })),
        })
        .try_build()?;
    serve(
        base,
        args.get("--identity")
            .map(String::as_str)
            .unwrap_or("anonymous"),
        capabilities,
    )
    .await
}

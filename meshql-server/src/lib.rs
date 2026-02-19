use meshql_core::{Auth, NoAuth, ServerConfig};
use meshql_graphlette::{GraphletteRouter, ResolverRegistry, build_schema};
use meshql_restlette::build_restlette_router;
use axum::Router;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Build the full Axum application from a ServerConfig.
///
/// For each graphlette, it also registers the searcher in the ResolverRegistry under the
/// graphlette path so that inter-graphlette resolution works without HTTP.
pub async fn build_app(config: ServerConfig) -> anyhow::Result<Router> {
    let mut registry = ResolverRegistry::new();

    // First pass: register all graphlette searchers in the registry
    for g in &config.graphlettes {
        registry.register(&g.path, Arc::clone(&g.searcher), g.root_config.clone());
    }

    let mut app = Router::new();

    // Add graphlette routes
    for g in config.graphlettes {
        let schema = build_schema(&g.schema_text, &g.root_config, g.searcher, &registry)
            .map_err(|e| anyhow::anyhow!("Schema build error for {}: {:?}", g.path, e))?;
        let router = GraphletteRouter::build(&g.path, schema);
        app = app.merge(router);
    }

    // Add restlette routes
    let auth: Arc<dyn Auth> = Arc::new(NoAuth);
    for r in config.restlettes {
        let router = build_restlette_router(&r.path, r.repository, Arc::clone(&auth));
        app = app.merge(router);
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Ok(app.layer(cors))
}

/// Start the server on the configured port.
pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    let port = config.port;
    let app = build_app(config).await?;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    println!("meshql-rs listening on port {port}");
    axum::serve(listener, app).await?;
    Ok(())
}

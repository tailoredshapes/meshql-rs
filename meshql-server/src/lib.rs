use axum::Router;
use meshql_changes::{ChangeHub, ChangeSource, StreamSource, StreamletteConfig};
use meshql_core::{Auth, NoAuth, ServerConfig};
use meshql_graphlette::{build_schema, GraphletteRouter, ResolverRegistry};
use meshql_restlette::build_restlette_router;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

pub use meshql_restlette::{
    build_restlette_router_ext, AuthContext, PostCreateFn, SideEffectContext, ValidatorContext,
    ValidatorFn,
};

/// Build the full Axum application from a ServerConfig.
///
/// For each graphlette, it also registers the searcher in the ResolverRegistry under the
/// graphlette path so that inter-graphlette resolution works without HTTP.
///
/// Uses `NoAuth` — see `build_app_with_auth` to provide a custom authorizer.
pub async fn build_app(config: ServerConfig) -> anyhow::Result<Router> {
    build_app_with_auth(config, Arc::new(NoAuth), Router::new()).await
}

/// Build the full Axum application, merging in extra custom routes.
///
/// Uses `NoAuth` — see `build_app_with_auth` to provide a custom authorizer.
pub async fn build_app_ext(config: ServerConfig, extra: Router) -> anyhow::Result<Router> {
    build_app_with_auth(config, Arc::new(NoAuth), extra).await
}

/// Build the full Axum application with a caller-supplied `Auth` and extra routes.
///
/// The `Auth` is used by every restlette route to filter records by
/// `authorized_tokens`. Wrap your identity-extraction `Auth` (e.g.
/// `StashKeyAuth`) with `CasbinAuth` from `meshql-casbin` to plug in
/// role-based access control.
pub async fn build_app_with_auth(
    config: ServerConfig,
    auth: Arc<dyn Auth>,
    extra: Router,
) -> anyhow::Result<Router> {
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
        let router = GraphletteRouter::build_with_auth(&g.path, schema, Arc::clone(&auth));
        app = app.merge(router);
    }

    // Add restlette routes
    for r in config.restlettes {
        let router = build_restlette_router(&r.path, r.repository, Arc::clone(&auth));
        app = app.merge(router);
    }

    // Merge extra custom routes (these take priority for overlapping paths)
    app = extra.merge(app);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Ok(app.layer(cors))
}

/// Like `build_app_ext`, plus per-meshlette SSE surfaces. Each streamlette
/// gets its own hub and its own pump task (see `meshql_changes::streamlette`
/// for why this is not the shared `run_tails` round-robin).
///
/// Streamlettes are a parameter rather than a `ServerConfig` field on
/// purpose: `StreamletteConfig` lives in `meshql-changes`, which depends on
/// `meshql-core`, so naming it from `meshql-core::ServerConfig` would be a
/// dependency cycle. Keeping it here also leaves every existing
/// `ServerConfig` construction site compiling untouched.
pub async fn build_app_with_streams(
    config: ServerConfig,
    extra: Router,
    streamlettes: Vec<StreamletteConfig>,
    auth: Arc<dyn Auth>,
) -> anyhow::Result<Router> {
    let mut extra = extra;
    for sl in streamlettes {
        let hub = ChangeHub::new(sl.hub_capacity);
        let poll_interval = sl.source.poll_interval();
        // The upcast is a MOVE and there is no downcast back, so clone the
        // Arc before upcasting — the config still needs its own view.
        let source: Arc<dyn ChangeSource> = match &sl.source {
            StreamSource::Tail { source, .. } => Arc::clone(source),
            StreamSource::Seekable { source, .. } => Arc::clone(source) as Arc<dyn ChangeSource>,
        };
        tokio::spawn(meshql_changes::streamlette::run_pump(
            source,
            hub.clone(),
            poll_interval,
        ));
        extra = extra.merge(meshql_changes::streamlette::streamlette_router(
            sl,
            hub,
            Arc::clone(&auth),
        ));
    }
    // MUST be `build_app_with_auth`, NOT `build_app_ext`: `build_app_ext` is
    // `build_app_with_auth(config, Arc::new(NoAuth), extra)`, so routing
    // through it would hand the streamlettes the caller's real `Auth` while
    // every restlette and graphlette silently ran on `NoAuth` — a split-auth
    // hole. `changes_router`'s doc comment states the same requirement: the
    // stream and the lettes must share one `Arc<dyn Auth>`.
    // Covered by `tests/streamlette_integration.rs::
    // `streamlettes_do_not_downgrade_the_lettes_to_noauth`.
    build_app_with_auth(config, auth, extra).await
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

/// Start the server with extra custom routes.
pub async fn run_ext(config: ServerConfig, extra: Router) -> anyhow::Result<()> {
    let port = config.port;
    let app = build_app_ext(config, extra).await?;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    println!("meshql-rs listening on port {port}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Start the server with a caller-supplied `Auth` and extra custom routes.
pub async fn run_with_auth(
    config: ServerConfig,
    auth: Arc<dyn Auth>,
    extra: Router,
) -> anyhow::Result<()> {
    let port = config.port;
    let app = build_app_with_auth(config, auth, extra).await?;
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    println!("meshql-rs listening on port {port}");
    axum::serve(listener, app).await?;
    Ok(())
}

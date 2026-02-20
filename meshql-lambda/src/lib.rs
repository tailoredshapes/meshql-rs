use meshql_core::ServerConfig;

/// Run the meshql server inside an AWS Lambda runtime.
///
/// Takes a `ServerConfig`, builds the axum Router via `meshql_server::build_app()`,
/// and runs it inside the `lambda_http` runtime which handles API Gateway events.
pub async fn run_lambda(config: ServerConfig) -> Result<(), lambda_http::Error> {
    let app = meshql_server::build_app(config)
        .await
        .map_err(|e| lambda_http::Error::from(format!("build_app failed: {e}")))?;
    lambda_http::run(app).await
}

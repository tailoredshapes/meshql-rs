use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use meshql_core::{Auth, Envelope, Repository, Stash};
use std::sync::Arc;
use uuid::Uuid;

/// Validation result: Ok(()) to proceed, Err(message) to reject with 400.
pub type ValidatorFn =
    Arc<dyn Fn(&Stash, &ValidatorContext) -> Result<(), String> + Send + Sync + 'static>;

/// Context passed to validators for cross-service checks.
#[derive(Clone)]
pub struct ValidatorContext {
    pub http_client: reqwest::Client,
    pub service_urls: std::collections::HashMap<String, String>,
}

impl Default for ValidatorContext {
    fn default() -> Self {
        Self {
            http_client: reqwest::Client::new(),
            service_urls: std::collections::HashMap::new(),
        }
    }
}

/// Side-effect function called after successful creation.
pub type PostCreateFn = Arc<dyn Fn(serde_json::Value, SideEffectContext) + Send + Sync + 'static>;

/// Context for side-effect functions.
#[derive(Clone)]
pub struct SideEffectContext {
    pub http_client: reqwest::Client,
    pub service_urls: std::collections::HashMap<String, String>,
}

#[derive(Clone)]
struct RestletteState {
    repo: Arc<dyn Repository>,
    auth: Arc<dyn Auth>,
    defaults: Option<Stash>,
    validator: Option<ValidatorFn>,
    post_create: Option<PostCreateFn>,
    side_effect_ctx: Option<SideEffectContext>,
}

pub fn build_restlette_router(
    path: &str,
    repo: Arc<dyn Repository>,
    auth: Arc<dyn Auth>,
) -> Router {
    build_restlette_router_ext(path, repo, auth, None, None, None, None)
}

pub fn build_restlette_router_ext(
    path: &str,
    repo: Arc<dyn Repository>,
    auth: Arc<dyn Auth>,
    defaults: Option<Stash>,
    validator: Option<ValidatorFn>,
    post_create: Option<PostCreateFn>,
    side_effect_ctx: Option<SideEffectContext>,
) -> Router {
    let state = RestletteState {
        repo,
        auth,
        defaults,
        validator,
        post_create,
        side_effect_ctx,
    };
    let item_path = format!("{}/:id", path.trim_end_matches('/'));

    Router::new()
        .route(path, post(create_handler).get(list_handler))
        .route(
            &item_path,
            get(read_handler).put(update_handler).delete(delete_handler),
        )
        .with_state(state)
}

async fn create_handler(
    State(state): State<RestletteState>,
    Json(mut payload): Json<Stash>,
) -> impl IntoResponse {
    // Apply defaults for missing fields
    if let Some(defaults) = &state.defaults {
        for (k, v) in defaults {
            if !payload.contains_key(k) {
                payload.insert(k.clone(), v.clone());
            }
        }
    }

    // Run validator
    if let Some(validator) = &state.validator {
        let ctx = ValidatorContext::default();
        if let Err(msg) = validator(&payload, &ctx) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response();
        }
    }

    let id = Uuid::new_v4().to_string();
    let tokens = state.auth.get_auth_token(&Stash::new());
    let envelope = Envelope::new(id, payload, tokens.clone());
    match state.repo.create(envelope, &tokens).await {
        Ok(env) => {
            let mut payload = env.payload;
            payload.insert("id".to_string(), serde_json::Value::String(env.id));
            let result = serde_json::Value::Object(payload);

            // Fire post-create side effect
            if let (Some(post_create), Some(ctx)) = (&state.post_create, &state.side_effect_ctx) {
                let result_clone = result.clone();
                let ctx_clone = ctx.clone();
                let post_create = Arc::clone(post_create);
                tokio::spawn(async move {
                    post_create(result_clone, ctx_clone);
                });
            }

            (StatusCode::CREATED, Json(result)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_handler(State(state): State<RestletteState>) -> impl IntoResponse {
    let tokens = state.auth.get_auth_token(&Stash::new());
    match state.repo.list(&tokens).await {
        Ok(envelopes) => {
            let items: Vec<serde_json::Value> = envelopes
                .into_iter()
                .map(|env| {
                    let mut payload = env.payload;
                    payload.insert("id".to_string(), serde_json::Value::String(env.id));
                    serde_json::Value::Object(payload)
                })
                .collect();
            Json(serde_json::Value::Array(items)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn read_handler(
    State(state): State<RestletteState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tokens = state.auth.get_auth_token(&Stash::new());
    match state.repo.read(&id, &tokens, None).await {
        Ok(Some(env)) => {
            let mut payload = env.payload;
            payload.insert("id".to_string(), serde_json::Value::String(env.id));
            Json(serde_json::Value::Object(payload)).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn update_handler(
    State(state): State<RestletteState>,
    Path(id): Path<String>,
    Json(payload): Json<Stash>,
) -> impl IntoResponse {
    let tokens = state.auth.get_auth_token(&Stash::new());

    // Merge: read existing, overlay new fields
    let merged = match state.repo.read(&id, &tokens, None).await {
        Ok(Some(existing)) => {
            let mut merged = existing.payload;
            for (k, v) in payload {
                merged.insert(k, v);
            }
            merged
        }
        _ => payload,
    };

    let envelope = Envelope::new(id, merged, tokens.clone());
    match state.repo.create(envelope, &tokens).await {
        Ok(env) => {
            let mut payload = env.payload;
            payload.insert("id".to_string(), serde_json::Value::String(env.id));
            Json(serde_json::Value::Object(payload)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_handler(
    State(state): State<RestletteState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tokens = state.auth.get_auth_token(&Stash::new());
    match state.repo.remove(&id, &tokens).await {
        Ok(true) => {
            let body = serde_json::json!({"id": id, "status": "deleted"});
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

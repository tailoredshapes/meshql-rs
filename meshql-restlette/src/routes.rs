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

#[derive(Clone)]
struct RestletteState {
    repo: Arc<dyn Repository>,
    auth: Arc<dyn Auth>,
}

pub fn build_restlette_router(
    path: &str,
    repo: Arc<dyn Repository>,
    auth: Arc<dyn Auth>,
) -> Router {
    let state = RestletteState { repo, auth };
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
    Json(payload): Json<Stash>,
) -> impl IntoResponse {
    let id = Uuid::new_v4().to_string();
    let tokens = state.auth.get_auth_token(&Stash::new());
    let envelope = Envelope::new(id, payload, tokens.clone());
    match state.repo.create(envelope, &tokens).await {
        Ok(env) => (
            StatusCode::CREATED,
            Json(serde_json::Value::Object(env.payload)),
        )
            .into_response(),
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
    let envelope = Envelope::new(id, payload, tokens.clone());
    match state.repo.create(envelope, &tokens).await {
        Ok(env) => Json(serde_json::Value::Object(env.payload)).into_response(),
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

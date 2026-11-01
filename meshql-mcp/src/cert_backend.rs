//! Real SQLite/REST/GraphQL backend for the shared stdio surface tier.
use crate::cert_auth::FixtureAuth;
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, post},
    Json, Router,
};
use meshql_core::{
    AuthContext, GraphletteConfig, Repository, RestletteConfig, RootConfig, ServerConfig, WriteMode,
};
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row,
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::io::AsyncReadExt;
#[derive(Clone)]
struct Fixture {
    repo: Arc<SqliteRepository>,
    sdl: String,
    schema: Value,
    requests: Arc<Mutex<Vec<Value>>>,
    failure: Arc<Mutex<Option<String>>>,
    base: Arc<OnceLock<String>>,
}
async fn capture(State(st): State<Fixture>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/__mcp/control" {
        return next.run(req).await;
    }
    let user = req
        .headers()
        .get("X-Manifold-User-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous")
        .to_owned();
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, 1048576).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    if !parts.headers.contains_key("X-Mcp-Control-Seed") {
        st.requests.lock().unwrap().push(json!({"method":parts.method.as_str(),"target":parts.uri.path_and_query().map(|v|v.as_str()).unwrap_or("/"),"body":serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),"identity":user}));
    }
    if parts.uri.path() == "/widget/graph" {
        let failure = st.failure.lock().unwrap().take();
        match failure.as_deref() {
            Some("http-503")=>return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"fixture unavailable mcp-private-credential"}))).into_response(),
            Some("graphql-partial")=>return Json(json!({"data":{"getAll":[{"name":"partial-secret"}]},"errors":[{"message":"fixture GraphQL error mcp-private-credential"}]})).into_response(),
            _=>(),
        }
    }
    let mut request = Request::from_parts(parts, Body::from(bytes));
    let mut stash = serde_json::Map::new();
    if user != "anonymous" {
        stash.insert("user".into(), user.into());
    }
    request.extensions_mut().insert(AuthContext::new(stash));
    next.run(request).await
}
async fn inspect(st: &Fixture) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT id,created_at_ms,deleted,authorized_tokens,payload FROM envelopes ORDER BY rowid",
    )
    .fetch_all(&st.repo.pool)
    .await?;
    let mut versions = HashMap::new();
    let mut envelopes = Vec::new();
    for row in rows {
        let id: String = row.try_get("id")?;
        let version = versions.entry(id.clone()).or_insert(0);
        *version += 1;
        let payload: String = row.try_get("payload")?;
        let auth: String = row.try_get("authorized_tokens")?;
        envelopes.push(json!({"id":id,"payload":serde_json::from_str::<Value>(&payload)?,"auth":serde_json::from_str::<Value>(&auth)?,"created_at":row.try_get::<i64,_>("created_at_ms")?,"deleted":row.try_get::<i64,_>("deleted")?!=0,"version":version}));
    }
    Ok(json!({"envelopes":envelopes}))
}
async fn command(st: &Fixture, input: Value) -> anyhow::Result<Value> {
    match input["op"].as_str().unwrap_or("") {
        "catalog" => Ok(json!({"sdl":st.sdl,"schema":st.schema})),
        "capture" => {
            let mut r = st.requests.lock().unwrap();
            let old = r.clone();
            if input["clear"] == true {
                r.clear();
            }
            Ok(json!({"requests":old}))
        }
        "failure" => {
            let kind = input["kind"].as_str().unwrap_or("");
            anyhow::ensure!(
                ["http-503", "graphql-partial"].contains(&kind),
                "invalid failure"
            );
            *st.failure.lock().unwrap() = Some(kind.into());
            Ok(json!({"ok":true}))
        }
        "inspect" => inspect(st).await,
        "seed" => {
            let user = input["identity"].as_str().unwrap_or("alice");
            let payload = input
                .get("payload")
                .ok_or_else(|| anyhow::anyhow!("payload required"))?;
            let mut request = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?
                .post(format!("{}/widget/api", st.base.get().unwrap()))
                .header("X-Mcp-Control-Seed", "1")
                .json(payload);
            if user != "anonymous" {
                request = request.header("X-Manifold-User-Id", user);
            }
            let response = request.send().await?;
            anyhow::ensure!(
                response.status().is_success(),
                "seed refused by normal REST surface"
            );
            let body: Value = response.json().await?;
            let id = body["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("seed has no ID"))?;
            let envelope = st
                .repo
                .read(id, &meshql_core::SystemSession, None)
                .await?
                .ok_or_else(|| anyhow::anyhow!("seed not stored"))?;
            Ok(json!({"id":id,"created_at":envelope.created_at.timestamp_millis()}))
        }
        "cutoff" => {
            let at = chrono::Utc::now().timestamp_millis();
            while chrono::Utc::now().timestamp_millis() <= at {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Ok(json!({"at":at}))
        }
        _ => anyhow::bail!("unknown control"),
    }
}
async fn control(State(st): State<Fixture>, Json(input): Json<Value>) -> Response {
    match command(&st, input).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}
pub(crate) async fn run(args: &HashMap<String, String>) -> anyhow::Result<()> {
    let mode = args.get("--auth").map(String::as_str).unwrap_or("noauth");
    anyhow::ensure!(
        ["noauth", "owner", "refuse-reads"].contains(&mode),
        "unknown auth"
    );
    let dir = Path::new(&args["--fixtures"]);
    let sdl = std::fs::read_to_string(dir.join("widget.graphql"))?;
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("widget.schema.json"))?)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&args["--db"])
                .create_if_missing(true),
        )
        .await?;
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await?);
    let searcher = Arc::new(crate::cert_searcher::TypedSearcher(Arc::new(
        SqliteSearcher::new_with_pool(pool).await?,
    )));
    let auth = Arc::new(FixtureAuth { mode: mode.into() });
    let root = RootConfig::builder()
        .vector("getAll", "{}")
        .singleton("getById", r#"{"id":"{{id}}"}"#)
        .vector("getByName", r#"{"payload.name":"{{name}}"}"#)
        .vector("getByGroupId", r#"{"payload.group_id":"{{group_id}}"}"#)
        .build();
    let config = ServerConfig {
        port: 0,
        graphlettes: vec![GraphletteConfig {
            path: "/widget/graph".into(),
            schema_text: sdl.clone(),
            root_config: root,
            searcher,
        }],
        restlettes: vec![RestletteConfig {
            path: "/widget/api".into(),
            schema_json: schema.clone(),
            repository: repo.clone(),
        }],
    };
    let write = match args
        .get("--write-mode")
        .map(String::as_str)
        .unwrap_or("actor")
    {
        "actor" => WriteMode::Open,
        "event" => WriteMode::Event,
        "produced" => WriteMode::ProducedBy(vec!["worker".into()]),
        _ => anyhow::bail!("unknown write mode"),
    };
    let app = meshql_server::build_app_with_modes(
        config,
        auth.clone(),
        Router::new(),
        [("/widget/api".into(), write)].into(),
    )
    .await?;
    let state = Fixture {
        repo,
        sdl,
        schema,
        requests: Arc::new(Mutex::new(Vec::new())),
        failure: Arc::new(Mutex::new(None)),
        base: Arc::new(OnceLock::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    state.base.set(url.clone()).unwrap();
    let controls = Router::new()
        .route("/__mcp/control", post(control))
        .with_state(state.clone());
    let app = app
        .merge(controls)
        .route(
            "/unrelated/api/sentinel",
            any(|| async { Json(json!({"secret":"unrelated-secret"})) }),
        )
        .layer(middleware::from_fn_with_state(state, capture));
    let ready = std::path::Path::new(&args["--ready-file"]);
    let temporary = ready.with_extension("tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&json!({"base_url":url,"control_url":format!("{url}/__mcp/control")}))?,
    )?;
    std::fs::rename(temporary, ready)?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let mut byte = [0];
            let _ = tokio::io::stdin().read(&mut byte).await;
        })
        .await?;
    Ok(())
}

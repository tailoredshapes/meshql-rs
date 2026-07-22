//! examples/farm — the minimal, non-event-sourced meshql reference
//! example, partially retrofitted per
//! docs/superpowers/specs/2026-07-22-farm-event-sourcing-retrofit-design.md:
//! lay_report is a create-only domain event, hen_productivity is a new
//! projection entity, and writes are authorized per-entity via three
//! separate CasbinAuth instances (see the plan's "Decisions" section for
//! why this doesn't require any meshql-core/meshql-server changes).

use meshql_casbin::CasbinAuth;
use meshql_core::{Auth, GraphletteConfig, NoAuth, RootConfig, ServerConfig};
use meshql_mongo::{MongoRepository, MongoSearcher};
use meshql_restlette::build_restlette_router;
use std::sync::Arc;

const FARM_GRAPHQL: &str = include_str!("../config/graph/farm.graphql");
const COOP_GRAPHQL: &str = include_str!("../config/graph/coop.graphql");
const HEN_GRAPHQL: &str = include_str!("../config/graph/hen.graphql");
const LAY_REPORT_GRAPHQL: &str = include_str!("../config/graph/lay_report.graphql");
const HEN_PRODUCTIVITY_GRAPHQL: &str = include_str!("../config/graph/hen_productivity.graphql");

const CASBIN_MODEL: &str = include_str!("../config/casbin/model.conf");
const ACTOR_POLICY: &str = include_str!("../config/casbin/actor_policy.csv");
const LAY_REPORT_POLICY: &str = include_str!("../config/casbin/lay_report_policy.csv");
const HEN_PRODUCTIVITY_POLICY: &str = include_str!("../config/casbin/hen_productivity_policy.csv");

/// Build the farm ServerConfig (graphlettes only — reads stay open to
/// everyone, per the spec) plus a hand-assembled restlette Router with
/// per-entity Casbin auth, ready to pass as `run_ext`'s `extra` argument
/// (not yet mounting `/manifest` — that lands in Task 6, see `main.rs`).
///
/// Shared by `main.rs` and integration tests, so tests exercise the real
/// wiring rather than a re-implementation of it.
pub async fn build(mongo_uri: &str, db_name: &str) -> anyhow::Result<(ServerConfig, axum::Router)> {
    // Reads (GraphQL) stay open to everyone — this retrofit is about
    // write authorization, not read restriction (per spec).
    let read_auth: Arc<dyn Auth> = Arc::new(NoAuth);

    // Three separate CasbinAuth instances = the per-entity discrimination
    // mechanism. CasbinAuth::authorize_action's Casbin object is always
    // the literal "/api" (meshql-casbin/src/lib.rs), so discrimination
    // happens by *which instance* handles a restlette's requests —
    // decided here, in wiring code — not by the engine matching a
    // per-entity object string.
    let actor_auth: Arc<dyn Auth> =
        Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, ACTOR_POLICY, NoAuth).await?);
    let lay_report_auth: Arc<dyn Auth> =
        Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, LAY_REPORT_POLICY, NoAuth).await?);
    // NOTE: as wired today, no real HTTP request can ever satisfy the
    // "worker" role this policy grants create/update to. Every caller
    // resolves to identity "*" via NoAuth, and hen_productivity_policy.csv
    // deliberately has no `g, *, worker` row (that's what makes this
    // restlette deny the default caller entirely) — so hen_productivity's
    // REST writes are locked for everyone until a real deployment adds
    // identity-injection middleware (trusted-header, StashKeyAuth + a `g`
    // binding, etc. — see the retrofit plan's decision #6). The worker
    // role's grant path is proven at the unit level only, in
    // tests/auth_policy_cert.rs's worker_role_can_create_and_update_hen_productivity.
    let hen_productivity_auth: Arc<dyn Auth> =
        Arc::new(CasbinAuth::from_strings(CASBIN_MODEL, HEN_PRODUCTIVITY_POLICY, NoAuth).await?);

    // --- Repositories ---
    let farm_repo =
        Arc::new(MongoRepository::new(mongo_uri, db_name, "farms", Arc::clone(&read_auth)).await?);
    let coop_repo =
        Arc::new(MongoRepository::new(mongo_uri, db_name, "coops", Arc::clone(&read_auth)).await?);
    let hen_repo =
        Arc::new(MongoRepository::new(mongo_uri, db_name, "hens", Arc::clone(&read_auth)).await?);
    let lay_report_repo = Arc::new(
        MongoRepository::new(mongo_uri, db_name, "lay_reports", Arc::clone(&read_auth)).await?,
    );
    let hen_productivity_repo = Arc::new(
        MongoRepository::new(mongo_uri, db_name, "hen_productivities", Arc::clone(&read_auth))
            .await?,
    );

    // --- Searchers ---
    let farm_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(mongo_uri, db_name, "farms", Arc::clone(&read_auth)).await?);
    let coop_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(mongo_uri, db_name, "coops", Arc::clone(&read_auth)).await?);
    let hen_searcher: Arc<dyn meshql_core::Searcher> =
        Arc::new(MongoSearcher::new(mongo_uri, db_name, "hens", Arc::clone(&read_auth)).await?);
    let lay_report_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(mongo_uri, db_name, "lay_reports", Arc::clone(&read_auth)).await?,
    );
    let hen_productivity_searcher: Arc<dyn meshql_core::Searcher> = Arc::new(
        MongoSearcher::new(mongo_uri, db_name, "hen_productivities", Arc::clone(&read_auth))
            .await?,
    );

    // --- Root Configs ---
    let farm_config = RootConfig::builder()
        .singleton("getFarm", r#"{"id": "{{id}}"}"#)
        .vector("getFarms", r#"{"name": "{{name}}"}"#)
        .vector_resolver("coops", None, "getCoopsByFarm", "/coop/graph")
        .build();

    let coop_config = RootConfig::builder()
        .singleton("getCoop", r#"{"id": "{{id}}"}"#)
        .vector("getCoops", r#"{"name": "{{name}}"}"#)
        .vector("getCoopsByFarm", r#"{"farmId": "{{id}}"}"#)
        .singleton_resolver("farm", Some("farmId"), "getFarm", "/farm/graph")
        .vector_resolver("hens", None, "getHensByCoop", "/hen/graph")
        .build();

    let hen_config = RootConfig::builder()
        .singleton("getHen", r#"{"id": "{{id}}"}"#)
        .vector("getHens", r#"{"name": "{{name}}"}"#)
        .vector("getHensByCoop", r#"{"coopId": "{{id}}"}"#)
        .singleton_resolver("coop", Some("coopId"), "getCoop", "/coop/graph")
        .vector_resolver("layReports", None, "getLayReportsByHen", "/lay_report/graph")
        .vector_resolver("productivity", None, "getHenProductivityByHen", "/hen_productivity/graph")
        .build();

    let lay_report_config = RootConfig::builder()
        .singleton("getLayReport", r#"{"id": "{{id}}"}"#)
        .vector("getLayReports", "{}")
        .vector("getLayReportsByHen", r#"{"payload.henId": "{{id}}"}"#)
        .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
        .build();

    let hen_productivity_config = RootConfig::builder()
        .singleton("getHenProductivity", r#"{"id": "{{id}}"}"#)
        .vector("getHenProductivities", "{}")
        .vector("getHenProductivityByHen", r#"{"payload.henId": "{{id}}"}"#)
        .singleton_resolver("hen", Some("henId"), "getHen", "/hen/graph")
        .build();

    // Graphlettes only — no restlettes here. build_app_with_auth (which
    // run_ext calls) applies exactly one shared Auth to every entry in
    // config.restlettes, which can't express three different policies.
    // Restlette routers are hand-built below instead, each with the
    // Auth instance appropriate to its own write policy, and merged into
    // the `extra` Router this function returns — the same mechanism
    // examples/egg-economy already uses for its own extra routes.
    let config = ServerConfig {
        port: 3033,
        graphlettes: vec![
            GraphletteConfig {
                path: "/farm/graph".to_string(),
                schema_text: FARM_GRAPHQL.to_string(),
                root_config: farm_config,
                searcher: farm_searcher,
            },
            GraphletteConfig {
                path: "/coop/graph".to_string(),
                schema_text: COOP_GRAPHQL.to_string(),
                root_config: coop_config,
                searcher: coop_searcher,
            },
            GraphletteConfig {
                path: "/hen/graph".to_string(),
                schema_text: HEN_GRAPHQL.to_string(),
                root_config: hen_config,
                searcher: hen_searcher,
            },
            GraphletteConfig {
                path: "/lay_report/graph".to_string(),
                schema_text: LAY_REPORT_GRAPHQL.to_string(),
                root_config: lay_report_config,
                searcher: lay_report_searcher,
            },
            GraphletteConfig {
                path: "/hen_productivity/graph".to_string(),
                schema_text: HEN_PRODUCTIVITY_GRAPHQL.to_string(),
                root_config: hen_productivity_config,
                searcher: hen_productivity_searcher,
            },
        ],
        restlettes: vec![],
    };

    // Restlette routers: farm/coop/hen share actor_auth (full CRUD);
    // lay_report gets its own create-only policy; hen_productivity gets
    // its own worker-only policy that denies the default caller entirely.
    let restlette_router = axum::Router::new()
        .merge(build_restlette_router("/farm/api", farm_repo, Arc::clone(&actor_auth)))
        .merge(build_restlette_router("/coop/api", coop_repo, Arc::clone(&actor_auth)))
        .merge(build_restlette_router("/hen/api", hen_repo, Arc::clone(&actor_auth)))
        .merge(build_restlette_router(
            "/lay_report/api",
            lay_report_repo,
            Arc::clone(&lay_report_auth),
        ))
        .merge(build_restlette_router(
            "/hen_productivity/api",
            hen_productivity_repo,
            Arc::clone(&hen_productivity_auth),
        ));

    Ok((config, restlette_router))
}

//! Worker configuration — one compiled binary, pointed at any farm
//! deployment (Rust, Java, or TS) purely via env vars, matching the
//! existing MONGO_URI/PLATFORM_URL env-var pattern used across this
//! workspace's examples. `from_lookup` takes a plain key->value lookup
//! (rather than reading `std::env` directly) so tests can exercise every
//! branch without mutating real process env vars — env var mutation is
//! process-global and races across parallel `cargo test` threads.

use std::path::PathBuf;
use std::time::Duration;

/// The two GraphQL query-naming dialects the three farm retrofits landed on
/// (see the reconciliation note at the top of the merkql-worker-pipeline
/// plan for which language picked which). `EntityNamed` is what Rust's farm
/// uses; `Generic` is what Java's and TS's both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryDialect {
    EntityNamed,
    Generic,
}

impl QueryDialect {
    /// The lay_report singleton query: `getLayReport(id, at)` (entity-named)
    /// vs `getById(id, at)` (generic).
    pub fn lay_report_by_id(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getLayReport",
            QueryDialect::Generic => "getById",
        }
    }

    /// The lay_report-vector-by-hen query: `getLayReportsByHen(id, at)` vs
    /// `getByHen(id, at)`.
    pub fn lay_reports_by_hen(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getLayReportsByHen",
            QueryDialect::Generic => "getByHen",
        }
    }

    /// The hen_productivity-by-hen query: `getHenProductivityByHen(id, at)`
    /// vs `getByHen(id, at)`.
    pub fn hen_productivity_by_hen(self) -> &'static str {
        match self {
            QueryDialect::EntityNamed => "getHenProductivityByHen",
            QueryDialect::Generic => "getByHen",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub merkql_dir: PathBuf,
    pub topic: String,
    pub group_id: String,
    pub poll_interval: Duration,
    pub source_graphql_base: String,
    pub target_rest_base: String,
    pub target_graphql_base: String,
    pub auth_header: Option<String>,
    pub auth_value: String,
    pub query_dialect: QueryDialect,
}

impl WorkerConfig {
    /// Read configuration from real process env vars.
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable core: takes a lookup function instead of touching
    /// `std::env` directly.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let default_base = "http://127.0.0.1:3033".to_string();
        let source_graphql_base =
            lookup("SOURCE_GRAPHQL_URL").unwrap_or_else(|| default_base.clone());
        let target_rest_base = lookup("TARGET_REST_URL").unwrap_or_else(|| default_base.clone());
        let target_graphql_base =
            lookup("TARGET_GRAPHQL_URL").unwrap_or_else(|| target_rest_base.clone());
        let query_dialect = match lookup("QUERY_DIALECT").as_deref() {
            Some("generic") => QueryDialect::Generic,
            // Anything else (unset, "entity-named", or an unrecognized
            // value) defaults to EntityNamed — Rust's dialect, the
            // deployment this plan was drafted and end-to-end tested
            // against.
            _ => QueryDialect::EntityNamed,
        };
        Self {
            merkql_dir: lookup("MERKQL_DIR")
                .unwrap_or_else(|| "./farm-changes-log".to_string())
                .into(),
            topic: lookup("WORKER_TOPIC").unwrap_or_else(|| "lay_report".to_string()),
            group_id: lookup("WORKER_GROUP_ID")
                .unwrap_or_else(|| "hen-productivity-worker".to_string()),
            poll_interval: Duration::from_millis(
                lookup("WORKER_POLL_INTERVAL_MS")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(500),
            ),
            source_graphql_base,
            target_rest_base,
            target_graphql_base,
            // Not yet reconciled against the retrofit's actual Casbin
            // wiring (CasbinAuth resolves identity via a trusted-header ->
            // Stash key -> role chain the retrofit plans build; the exact
            // header name/value that maps to the "worker" role is decided
            // there, not here). This env-var-configurable header is a
            // deliberately generic placeholder that honors the pipeline
            // spec's requirement ("the worker authenticates as the worker
            // role") without hardcoding a mechanism not yet built — set
            // WORKER_AUTH_HEADER/WORKER_AUTH_TOKEN to whatever the landed
            // retrofit's edge middleware actually expects.
            auth_header: lookup("WORKER_AUTH_HEADER"),
            auth_value: lookup("WORKER_AUTH_TOKEN").unwrap_or_else(|| "worker".to_string()),
            query_dialect,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn from_lookup_applies_defaults_when_nothing_is_set() {
        let cfg = WorkerConfig::from_lookup(|_| None);
        assert_eq!(cfg.merkql_dir, PathBuf::from("./farm-changes-log"));
        assert_eq!(cfg.topic, "lay_report");
        assert_eq!(cfg.group_id, "hen-productivity-worker");
        assert_eq!(cfg.poll_interval, Duration::from_millis(500));
        assert_eq!(cfg.source_graphql_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.target_rest_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.target_graphql_base, "http://127.0.0.1:3033");
        assert_eq!(cfg.auth_header, None);
        assert_eq!(cfg.auth_value, "worker");
        assert_eq!(cfg.query_dialect, QueryDialect::EntityNamed);
    }

    #[test]
    fn from_lookup_honors_overrides_and_defaults_target_graphql_to_target_rest() {
        let vars: HashMap<&str, &str> = [
            ("SOURCE_GRAPHQL_URL", "http://rust-farm:3033"),
            ("TARGET_REST_URL", "http://java-farm:8080"),
            ("WORKER_AUTH_HEADER", "x-worker-token"),
            ("WORKER_POLL_INTERVAL_MS", "250"),
            ("QUERY_DIALECT", "generic"),
        ]
        .into();
        let cfg = WorkerConfig::from_lookup(|k| vars.get(k).map(|s| s.to_string()));

        assert_eq!(cfg.source_graphql_base, "http://rust-farm:3033");
        assert_eq!(cfg.target_rest_base, "http://java-farm:8080");
        // TARGET_GRAPHQL_URL was not set → defaults to TARGET_REST_URL. This
        // is what lets one worker binary point at a whole farm deployment
        // (Rust, Java, or TS) with a single URL when REST and GraphQL share
        // a base, per the spec's "purely a config change, never a rebuild."
        assert_eq!(cfg.target_graphql_base, "http://java-farm:8080");
        assert_eq!(cfg.auth_header, Some("x-worker-token".to_string()));
        assert_eq!(cfg.poll_interval, Duration::from_millis(250));
        // Java's and TS's farm retrofits both landed the generic dialect
        // (getById/getByHen) rather than Rust's entity-named one — see the
        // reconciliation note at the top of this plan. QUERY_DIALECT is
        // what lets the SAME worker binary point at either.
        assert_eq!(cfg.query_dialect, QueryDialect::Generic);
    }

    #[test]
    fn from_lookup_defaults_query_dialect_to_entity_named_on_unrecognized_value() {
        let vars: HashMap<&str, &str> = [("QUERY_DIALECT", "not-a-real-dialect")].into();
        let cfg = WorkerConfig::from_lookup(|k| vars.get(k).map(|s| s.to_string()));
        assert_eq!(cfg.query_dialect, QueryDialect::EntityNamed);
    }
}

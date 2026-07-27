use chrono::{DateTime, Utc};
use cucumber::World;
use meshql_core::{Envelope, Repository, Searcher, Stash};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

struct DebugRepo(Arc<dyn Repository>);
impl fmt::Debug for DebugRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Repository")
    }
}

struct DebugSearcher(Arc<dyn Searcher>);
impl fmt::Debug for DebugSearcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Searcher")
    }
}

#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct CertWorld {
    repo_inner: Option<DebugRepo>,
    searcher_inner: Option<DebugSearcher>,

    // Repo cert state
    pub envelopes_by_name: HashMap<String, Envelope>,
    pub last_envelopes: Vec<Envelope>,
    pub timestamps: HashMap<String, DateTime<Utc>>,
    pub last_search_result: Option<Option<Stash>>,
    pub search_results: Vec<Stash>,
    pub last_remove: bool,
    pub remove_results: HashMap<String, bool>,
    pub test_start: DateTime<Utc>,

    // Searcher templates (name → template string)
    pub templates: HashMap<String, String>,

    // Farm E2E state
    pub server_addr: Option<String>,
    pub server_b_addr: Option<String>,
    pub ids: HashMap<String, HashMap<String, String>>,
    pub first_stamp_ms: Option<i64>,
    pub farm_response: Option<serde_json::Value>,

    // End-to-end authorization cert state (see `crate::authz`)
    /// widget name → the id the restlette handed back on create
    pub authz_ids: HashMap<String, String>,
    /// names from the last list/vector read, whichever surface it came from
    pub authz_names: Vec<String>,
    /// named instants, for the temporal scenarios
    pub authz_stamps: HashMap<String, i64>,
    /// HTTP status of the last mutating request
    pub authz_status: Option<u16>,
    /// body of the last GraphQL singleton read
    pub authz_response: Option<serde_json::Value>,
}

impl Default for CertWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl CertWorld {
    pub fn new() -> Self {
        let mut world = Self {
            repo_inner: None,
            searcher_inner: None,
            envelopes_by_name: HashMap::new(),
            last_envelopes: Vec::new(),
            timestamps: HashMap::new(),
            last_search_result: None,
            search_results: Vec::new(),
            last_remove: false,
            remove_results: HashMap::new(),
            test_start: Utc::now(),
            templates: HashMap::new(),
            server_addr: None,
            server_b_addr: None,
            ids: HashMap::new(),
            first_stamp_ms: None,
            farm_response: None,
            authz_ids: HashMap::new(),
            authz_names: Vec::new(),
            authz_stamps: HashMap::new(),
            authz_status: None,
            authz_response: None,
        };
        world.init_templates();
        world
    }

    /// Set the repository for this world.
    pub fn set_repo(&mut self, repo: Arc<dyn Repository>) {
        self.repo_inner = Some(DebugRepo(repo));
    }

    /// Set the searcher for this world.
    pub fn set_searcher(&mut self, searcher: Arc<dyn Searcher>) {
        self.searcher_inner = Some(DebugSearcher(searcher));
    }

    pub fn init_templates(&mut self) {
        self.templates
            .insert("findById".into(), r#"{"id": "{{id}}"}"#.into());
        self.templates
            .insert("findByName".into(), r#"{"payload.name": "{{id}}"}"#.into());
        self.templates.insert(
            "findAllByType".into(),
            r#"{"payload.type": "{{id}}"}"#.into(),
        );
        self.templates.insert(
            "findByNameAndType".into(),
            r#"{"payload.name": "{{name}}", "payload.type": "{{type}}"}"#.into(),
        );
    }

    pub fn star() -> Vec<String> {
        vec!["*".to_string()]
    }

    /// Whether a backing repository has been supplied. The authorization cert
    /// needs one so it can inspect what the write path actually persisted.
    pub fn has_repo(&self) -> bool {
        self.repo_inner.is_some()
    }

    /// Base URL of the server under certification.
    pub fn server_addr(&self) -> &str {
        self.server_addr
            .as_deref()
            .expect("server_addr not initialized")
    }

    /// A previously captured instant, in epoch milliseconds.
    pub fn authz_stamp(&self, key: &str) -> i64 {
        *self
            .authz_stamps
            .get(key)
            .unwrap_or_else(|| panic!("no instant was captured as '{key}'"))
    }

    /// Clear per-scenario authorization state. Call from the runner's
    /// before-hook alongside handing the world a fresh server.
    pub fn reset_authz(&mut self) {
        self.authz_ids.clear();
        self.authz_names.clear();
        self.authz_stamps.clear();
        self.authz_status = None;
        self.authz_response = None;
    }

    pub fn repo(&self) -> &dyn Repository {
        self.repo_inner
            .as_ref()
            .expect("repo not initialized")
            .0
            .as_ref()
    }

    pub fn searcher(&self) -> &dyn Searcher {
        self.searcher_inner
            .as_ref()
            .expect("searcher not initialized")
            .0
            .as_ref()
    }
}

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
    pub ids: HashMap<String, HashMap<String, String>>,
    pub first_stamp_ms: Option<i64>,
    pub farm_response: Option<serde_json::Value>,
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
            ids: HashMap::new(),
            first_stamp_ms: None,
            farm_response: None,
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

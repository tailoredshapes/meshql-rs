use crate::{Repository, Searcher};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct QueryConfig {
    pub name: String,
    pub template: String,
    pub is_singleton: bool,
}

#[derive(Debug, Clone)]
pub struct SingletonResolverConfig {
    pub field_name: String,
    pub foreign_key: Option<String>,
    pub query_name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct VectorResolverConfig {
    pub field_name: String,
    pub foreign_key: Option<String>,
    pub query_name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct InternalSingletonResolverConfig {
    pub field_name: String,
    pub foreign_key: Option<String>,
    pub query_name: String,
    pub graphlette_path: String,
}

#[derive(Debug, Clone)]
pub struct InternalVectorResolverConfig {
    pub field_name: String,
    pub foreign_key: Option<String>,
    pub query_name: String,
    pub graphlette_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct RootConfig {
    pub queries: Vec<QueryConfig>,
    pub singleton_resolvers: Vec<SingletonResolverConfig>,
    pub vector_resolvers: Vec<VectorResolverConfig>,
    pub internal_singleton_resolvers: Vec<InternalSingletonResolverConfig>,
    pub internal_vector_resolvers: Vec<InternalVectorResolverConfig>,
}

impl RootConfig {
    pub fn builder() -> RootConfigBuilder {
        RootConfigBuilder::default()
    }

    pub fn get_template(&self, query_name: &str) -> Option<&str> {
        self.queries
            .iter()
            .find(|q| q.name == query_name)
            .map(|q| q.template.as_str())
    }
}

#[derive(Default)]
pub struct RootConfigBuilder {
    config: RootConfig,
}

impl RootConfigBuilder {
    pub fn singleton(mut self, name: impl Into<String>, template: impl Into<String>) -> Self {
        self.config.queries.push(QueryConfig {
            name: name.into(),
            template: template.into(),
            is_singleton: true,
        });
        self
    }

    pub fn vector(mut self, name: impl Into<String>, template: impl Into<String>) -> Self {
        self.config.queries.push(QueryConfig {
            name: name.into(),
            template: template.into(),
            is_singleton: false,
        });
        self
    }

    pub fn singleton_resolver(
        mut self,
        field_name: impl Into<String>,
        foreign_key: Option<&str>,
        query_name: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        self.config
            .singleton_resolvers
            .push(SingletonResolverConfig {
                field_name: field_name.into(),
                foreign_key: foreign_key.map(String::from),
                query_name: query_name.into(),
                url: url.into(),
            });
        self
    }

    pub fn vector_resolver(
        mut self,
        field_name: impl Into<String>,
        foreign_key: Option<&str>,
        query_name: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        self.config.vector_resolvers.push(VectorResolverConfig {
            field_name: field_name.into(),
            foreign_key: foreign_key.map(String::from),
            query_name: query_name.into(),
            url: url.into(),
        });
        self
    }

    pub fn internal_singleton_resolver(
        mut self,
        field_name: impl Into<String>,
        foreign_key: Option<&str>,
        query_name: impl Into<String>,
        graphlette_path: impl Into<String>,
    ) -> Self {
        self.config
            .internal_singleton_resolvers
            .push(InternalSingletonResolverConfig {
                field_name: field_name.into(),
                foreign_key: foreign_key.map(String::from),
                query_name: query_name.into(),
                graphlette_path: graphlette_path.into(),
            });
        self
    }

    pub fn internal_vector_resolver(
        mut self,
        field_name: impl Into<String>,
        foreign_key: Option<&str>,
        query_name: impl Into<String>,
        graphlette_path: impl Into<String>,
    ) -> Self {
        self.config
            .internal_vector_resolvers
            .push(InternalVectorResolverConfig {
                field_name: field_name.into(),
                foreign_key: foreign_key.map(String::from),
                query_name: query_name.into(),
                graphlette_path: graphlette_path.into(),
            });
        self
    }

    pub fn build(self) -> RootConfig {
        self.config
    }
}

pub struct GraphletteConfig {
    pub path: String,
    pub schema_text: String,
    pub root_config: RootConfig,
    pub searcher: Arc<dyn Searcher>,
}

pub struct RestletteConfig {
    pub path: String,
    pub schema_json: serde_json::Value,
    pub repository: Arc<dyn Repository>,
}

pub struct ServerConfig {
    pub port: u16,
    pub graphlettes: Vec<GraphletteConfig>,
    pub restlettes: Vec<RestletteConfig>,
}

/// What a restlette lets the HTTP surface do to its entity. Authorization of
/// *who* stays with the auth plugin; this says what *kind* of entity it is.
///
/// Declared per restlette path to `meshql_server::build_app_with_modes`. A
/// path with no entry is `Open`. The names follow meshql-fib's
/// `(:event)` and `(:produced-by ..)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WriteMode {
    /// An ordinary entity: create, replace and delete, as the auth plugin allows.
    #[default]
    Open,
    /// An event: a fact that happened. `POST` creates it; `PUT` and `DELETE`
    /// answer 405. Nothing about who may read or create it changes.
    Event,
    /// A projection: written only by the named producers (its workers). A
    /// `POST`, `PUT` or `DELETE` from a session that is none of them answers 403
    /// (`Session::is_principal`); reads are as for any entity.
    ProducedBy(Vec<String>),
}

/// Restlette path (`"/lay_report/api"`) to its [`WriteMode`].
pub type WriteModes = std::collections::HashMap<String, WriteMode>;

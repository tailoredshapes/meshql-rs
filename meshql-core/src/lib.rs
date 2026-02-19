pub mod auth;
pub mod config;
pub mod error;
pub mod testing;

pub use auth::{Auth, NoAuth};
pub use config::{
    GraphletteConfig, InternalSingletonResolverConfig, InternalVectorResolverConfig, QueryConfig,
    RestletteConfig, RootConfig, RootConfigBuilder, ServerConfig, SingletonResolverConfig,
    VectorResolverConfig,
};
pub use error::{MeshqlError, Result};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type Stash = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub payload: Stash,
    pub created_at: DateTime<Utc>,
    pub deleted: bool,
    pub authorized_tokens: Vec<String>,
}

impl Envelope {
    pub fn new(id: impl Into<String>, payload: Stash, tokens: Vec<String>) -> Self {
        Self {
            id: id.into(),
            payload,
            created_at: Utc::now(),
            deleted: false,
            authorized_tokens: tokens,
        }
    }
}

#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope>;
    async fn read(
        &self,
        id: &str,
        tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>>;
    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool>;
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>>;
    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove_many(
        &self,
        ids: &[String],
        tokens: &[String],
    ) -> Result<HashMap<String, bool>>;
}

#[async_trait::async_trait]
pub trait Searcher: Send + Sync {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>>;
    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>>;
}

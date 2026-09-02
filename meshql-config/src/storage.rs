//! Turning a config's `storage` block into a repository and a searcher.
//!
//! The `type` field names the adapter and the rest of the block is that
//! adapter's own. A type this build was not compiled with is an error naming
//! the feature to enable, rather than a silent fallback — a deployment that
//! quietly runs on the wrong store is worse than one that refuses to start.

use crate::{ConfigError, StorageDef};
use meshql_core::{Auth, Repository, Searcher};
use std::sync::Arc;

/// One meshlette's store.
pub struct Store {
    pub repository: Arc<dyn Repository>,
    pub searcher: Arc<dyn Searcher>,
}

fn need<'a>(def: &'a StorageDef, key: &str) -> Result<&'a str, ConfigError> {
    def.settings
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ConfigError::Shape(format!(
                "storage type \"{}\" needs a string \"{key}\"",
                def.kind
            ))
        })
}

/// Build the store a `storage` block describes.
pub async fn open(def: &StorageDef, auth: Arc<dyn Auth>) -> Result<Store, ConfigError> {
    let _ = &auth;
    match def.kind.as_str() {
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            // `uri` matches the SQL family's wording in the shared configs.
            let uri = need(def, "uri")?;
            let repo = meshql_sqlite::SqliteRepository::new(uri)
                .await
                .map_err(|e| ConfigError::Shape(e.to_string()))?;
            let searcher = meshql_sqlite::SqliteSearcher::new(uri)
                .await
                .map_err(|e| ConfigError::Shape(e.to_string()))?;
            Ok(Store {
                repository: Arc::new(repo),
                searcher: Arc::new(searcher),
            })
        }

        #[cfg(feature = "mongo")]
        "mongo" => {
            let uri = need(def, "uri")?;
            let db = need(def, "db")?;
            let collection = need(def, "collection")?;
            let repo = meshql_mongo::MongoRepository::new(uri, db, collection, auth.clone())
                .await
                .map_err(|e| ConfigError::Shape(e.to_string()))?;
            let searcher = meshql_mongo::MongoSearcher::new(uri, db, collection, auth)
                .await
                .map_err(|e| ConfigError::Shape(e.to_string()))?;
            Ok(Store {
                repository: Arc::new(repo),
                searcher: Arc::new(searcher),
            })
        }

        other => Err(ConfigError::Shape(format!(
            "storage type \"{other}\" is not available in this build. \
             Enable the matching feature on meshql-config, or use one of: {}",
            available().join(", ")
        ))),
    }
}

/// Which storage types this build can open. Reported in the error so an
/// operator learns what is possible rather than only what failed.
// Each push is cfg-gated, so `vec![..]` cannot express this: which elements
// exist is decided at compile time, not at the literal.
#[allow(clippy::vec_init_then_push)]
pub fn available() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut v: Vec<&'static str> = Vec::new();
    #[cfg(feature = "sqlite")]
    v.push("sqlite");
    #[cfg(feature = "mongo")]
    v.push("mongo");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(kind: &str) -> StorageDef {
        StorageDef {
            kind: kind.into(),
            settings: serde_json::Map::new(),
        }
    }

    /// An unknown type refuses and says what it could have been. A deployment
    /// silently running on the wrong store is worse than one that will not boot.
    #[tokio::test]
    async fn an_unavailable_storage_type_refuses_and_names_the_alternatives() {
        let auth: Arc<dyn Auth> = Arc::new(meshql_core::NoAuth);
        let msg = match open(&def("cassandra"), auth).await {
            Ok(_) => panic!("an unknown storage type must refuse"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("cassandra"), "{msg}");
        for kind in available() {
            assert!(msg.contains(kind), "the error should list {kind}: {msg}");
        }
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_missing_required_setting_names_the_key() {
        let auth: Arc<dyn Auth> = Arc::new(meshql_core::NoAuth);
        let msg = match open(&def("sqlite"), auth).await {
            Ok(_) => panic!("a missing uri must refuse"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("uri"), "{msg}");
    }
}

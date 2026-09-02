use crate::converters::{document_to_envelope, envelope_to_document};
use bson::{doc, Bson, Document};
use chrono::{DateTime, Utc};
use meshql_core::versions::{version_order, version_token, VersionRef};
use meshql_core::{
    Auth, Envelope, MeshqlError, Operation, Repository, Result, Session, SystemSession,
};
use mongodb::Collection;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MongoRepository {
    collection: Collection<Document>,
    #[allow(dead_code)]
    auth: Arc<dyn Auth>,
}

impl MongoRepository {
    /// Every version of one document, ordered by the shared `version_order`.
    ///
    /// Sorting happens in Rust rather than in the pipeline. `createdAt` is
    /// millisecond precision, so a `$sort` on it alone leaves ties in an order
    /// Mongo does not define — which is the bug this ordering exists to fix.
    async fn all_versions(&self, id: &str) -> Result<Vec<Envelope>> {
        let mut cursor = self
            .collection
            .aggregate(vec![doc! { "$match": { "id": id } }])
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut out = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            if let Some(env) = document_to_envelope(&doc) {
                out.push(env);
            }
        }
        out.sort_by(version_order);
        Ok(out)
    }
    pub async fn new(
        uri: &str,
        db_name: &str,
        collection_name: &str,
        auth: Arc<dyn Auth>,
    ) -> Result<Self> {
        let client = mongodb::Client::with_uri_str(uri)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let db = client.database(db_name);
        let collection = db.collection::<Document>(collection_name);
        Ok(Self { collection, auth })
    }
}

#[async_trait::async_trait]
impl Repository for MongoRepository {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope> {
        // The plugin owns the mark. Storage hands it the envelope and
        // persists whatever comes back, verbatim, in the same write as the
        // payload — so authorization can never become a dual write.
        let mut envelope = session.stamp(envelope);
        if envelope.id.is_empty() {
            envelope.id = uuid::Uuid::new_v4().to_string();
        }

        let doc = envelope_to_document(&envelope);
        self.collection
            .insert_one(doc)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(envelope)
    }

    async fn read(
        &self,
        id: &str,
        session: &dyn Session,
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let at_bson = bson::DateTime::from_chrono(at.unwrap_or_else(Utc::now));

        // Authorization is applied in Rust, by asking the session, to
        // the resolved version, below — matching on authorizedTokens here
        // would both mis-state the convention (empty tokens are public, "*" is
        // visible to everyone) and resurface an older visible version of a
        // now-restricted envelope.
        let pipeline = vec![
            doc! {
                "$match": {
                    "id": id,
                    "createdAt": { "$lte": at_bson },
                }
            },
            doc! { "$sort": { "createdAt": -1 } },
            doc! { "$limit": 1 },
        ];

        let mut cursor = self
            .collection
            .aggregate(pipeline)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        if cursor
            .advance()
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let env = document_to_envelope(&doc);
            Ok(env.filter(|e| !e.deleted && session.is_authorized(Operation::Read, e)))
        } else {
            Ok(None)
        }
    }

    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>> {
        let now = bson::DateTime::now();

        let pipeline = vec![
            doc! {
                "$match": {
                    "createdAt": { "$lte": now },
                }
            },
            doc! { "$sort": { "id": 1, "createdAt": -1 } },
            doc! {
                "$group": {
                    "_id": "$id",
                    "doc": { "$first": "$$ROOT" }
                }
            },
            doc! { "$replaceRoot": { "newRoot": "$doc" } },
            doc! { "$match": { "deleted": { "$ne": true } } },
        ];

        let mut cursor = self
            .collection
            .aggregate(pipeline)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            if let Some(env) = document_to_envelope(&doc) {
                // Visibility applied after $group, on the latest version only.
                if session.is_authorized(Operation::Read, &env) {
                    results.push(env);
                }
            }
        }

        Ok(results)
    }

    async fn remove(&self, id: &str, session: &dyn Session) -> Result<bool> {
        // Resolve the record first, then ask the plugin about `Remove`
        // specifically. Reusing the authorized `read` would silently make
        // remove a synonym for read.
        let current = self.read(id, &SystemSession, None).await?;
        match current {
            None => Ok(false),
            Some(env) if !session.is_authorized(Operation::Remove, &env) => Ok(false),
            Some(mut env) => {
                env.deleted = true;
                env.created_at = Utc::now();
                let doc = envelope_to_document(&env);
                self.collection
                    .insert_one(doc)
                    .await
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                Ok(true)
            }
        }
    }

    /// Writes the whole batch with `insert_many` instead of one round trip per
    /// envelope. A bulk load of millions of rows over a network pays for those
    /// round trips and nothing else. No manual chunking: the driver already
    /// splits a run past the server's `maxWriteBatchSize` / message-size limits.
    ///
    /// **Unordered.** A meshql collection is an append-only log with no unique
    /// index, so the documents in a batch are independent — stopping at the
    /// first failure buys nothing, because the caller's rule is
    /// append-then-commit-the-position and the whole batch replays either way.
    /// Unordered lets the server keep going and report *every* offending
    /// document rather than only the first, which is the difference between one
    /// diagnosable failure and a queue of them. It does not affect reads:
    /// `read` and `list` resolve versions by `createdAt`, never by insertion
    /// order, so ordered inserts would not make a same-millisecond tie
    /// deterministic anyway.
    ///
    /// No transaction: atomicity is not required (see above) and a session
    /// would restrict this to replica sets for nothing.
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        session: &dyn Session,
    ) -> Result<Vec<Envelope>> {
        // `insert_many` rejects an empty document list outright. An empty batch
        // is a no-op, not a failure.
        if envelopes.is_empty() {
            return Ok(Vec::new());
        }

        // Same assignments `create` makes: the plugin's stamp, then a
        // generated id for an empty one.
        let mut prepared = Vec::with_capacity(envelopes.len());
        for env in envelopes {
            let mut env = session.stamp(env);
            if env.id.is_empty() {
                env.id = uuid::Uuid::new_v4().to_string();
            }
            prepared.push(env);
        }

        let docs: Vec<Document> = prepared.iter().map(envelope_to_document).collect();

        let result = self
            .collection
            .insert_many(docs)
            .ordered(false)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        // The driver already turns per-document write errors into an `Err`, so
        // this only fires if a future driver ever reports a short write as
        // success. Reporting a partial batch as complete is the one failure
        // mode the caller cannot recover from: the position commits, and the
        // missing envelopes are a permanent gap.
        if result.inserted_ids.len() != prepared.len() {
            return Err(MeshqlError::Storage(format!(
                "insert_many reported {} of {} documents inserted",
                result.inserted_ids.len(),
                prepared.len()
            )));
        }

        // Input order, with the assignments the documents were written with.
        Ok(prepared)
    }

    async fn read_many(&self, ids: &[String], session: &dyn Session) -> Result<Vec<Envelope>> {
        let bson_ids: Vec<Bson> = ids.iter().map(|s| Bson::String(s.clone())).collect();
        let now = bson::DateTime::now();

        let pipeline = vec![
            doc! {
                "$match": {
                    "id": { "$in": bson_ids },
                    "createdAt": { "$lte": now },
                }
            },
            doc! { "$sort": { "id": 1, "createdAt": -1 } },
            doc! {
                "$group": {
                    "_id": "$id",
                    "doc": { "$first": "$$ROOT" }
                }
            },
            doc! { "$replaceRoot": { "newRoot": "$doc" } },
            doc! { "$match": { "deleted": { "$ne": true } } },
        ];

        let mut cursor = self
            .collection
            .aggregate(pipeline)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?
        {
            let doc = cursor
                .deserialize_current()
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            if let Some(env) = document_to_envelope(&doc) {
                // Visibility applied after $group, on the latest version only.
                if session.is_authorized(Operation::Read, &env) {
                    results.push(env);
                }
            }
        }

        Ok(results)
    }

    async fn remove_many(
        &self,
        ids: &[String],
        session: &dyn Session,
    ) -> Result<HashMap<String, bool>> {
        let mut results = HashMap::new();
        for id in ids {
            let deleted = self.remove(id, session).await?;
            results.insert(id.clone(), deleted);
        }
        Ok(results)
    }
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>> {
        let envelopes = self.all_versions(id).await?;
        Ok(envelopes
            .iter()
            .map(|e| {
                if session.is_authorized(Operation::Read, e) {
                    VersionRef::visible(e)
                } else {
                    VersionRef::tombstone(e)
                }
            })
            .collect())
    }

    async fn read_version(
        &self,
        id: &str,
        token: &str,
        session: &dyn Session,
    ) -> Result<Option<Envelope>> {
        for env in self.all_versions(id).await? {
            if version_token(&env) != token {
                continue;
            }
            // Unauthorized is not absent: the listing already reported it.
            if !session.is_authorized(Operation::Read, &env) {
                return Err(MeshqlError::Unauthorized);
            }
            return Ok(Some(env));
        }
        Ok(None)
    }
}

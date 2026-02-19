use crate::converters::{document_to_envelope, envelope_to_document};
use bson::{doc, Bson, Document};
use chrono::{DateTime, Utc};
use meshql_core::{Auth, Envelope, MeshqlError, Repository, Result};
use mongodb::Collection;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MongoRepository {
    collection: Collection<Document>,
    #[allow(dead_code)]
    auth: Arc<dyn Auth>,
}

impl MongoRepository {
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
    async fn create(&self, mut envelope: Envelope, tokens: &[String]) -> Result<Envelope> {
        if envelope.id.is_empty() {
            envelope.id = uuid::Uuid::new_v4().to_string();
        }
        envelope.authorized_tokens = tokens.to_vec();

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
        tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let at_bson = bson::DateTime::from_chrono(at.unwrap_or_else(Utc::now));
        let bson_tokens: Vec<Bson> = tokens.iter().map(|s| Bson::String(s.clone())).collect();

        let pipeline = vec![
            doc! {
                "$match": {
                    "id": id,
                    "createdAt": { "$lte": at_bson },
                    "authorizedTokens": { "$in": bson_tokens },
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
            Ok(env.filter(|e| !e.deleted))
        } else {
            Ok(None)
        }
    }

    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>> {
        let now = bson::DateTime::now();
        let bson_tokens: Vec<Bson> = tokens.iter().map(|s| Bson::String(s.clone())).collect();

        let pipeline = vec![
            doc! {
                "$match": {
                    "createdAt": { "$lte": now },
                    "authorizedTokens": { "$in": bson_tokens },
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
                results.push(env);
            }
        }

        Ok(results)
    }

    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool> {
        let current = self.read(id, tokens, None).await?;
        match current {
            None => Ok(false),
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

    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>> {
        let mut results = Vec::with_capacity(envelopes.len());
        for env in envelopes {
            results.push(self.create(env, tokens).await?);
        }
        Ok(results)
    }

    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>> {
        let bson_tokens: Vec<Bson> = tokens.iter().map(|s| Bson::String(s.clone())).collect();
        let bson_ids: Vec<Bson> = ids.iter().map(|s| Bson::String(s.clone())).collect();
        let now = bson::DateTime::now();

        let pipeline = vec![
            doc! {
                "$match": {
                    "id": { "$in": bson_ids },
                    "createdAt": { "$lte": now },
                    "authorizedTokens": { "$in": bson_tokens },
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
                results.push(env);
            }
        }

        Ok(results)
    }

    async fn remove_many(
        &self,
        ids: &[String],
        tokens: &[String],
    ) -> Result<HashMap<String, bool>> {
        let mut results = HashMap::new();
        for id in ids {
            let deleted = self.remove(id, tokens).await?;
            results.insert(id.clone(), deleted);
        }
        Ok(results)
    }
}

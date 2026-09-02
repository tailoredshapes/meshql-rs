use crate::converters::{document_to_envelope, document_to_result_stash, stash_to_doc};
use bson::{doc, Document};
use handlebars::Handlebars;
use meshql_core::{Auth, MeshqlError, Operation, Result, Searcher, Session, Stash};
use mongodb::Collection;
use std::sync::Arc;

pub struct MongoSearcher {
    collection: Collection<Document>,
    #[allow(dead_code)]
    auth: Arc<dyn Auth>,
    handlebars: Handlebars<'static>,
}

impl MongoSearcher {
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
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        Ok(Self {
            collection,
            auth,
            handlebars,
        })
    }

    fn render_template(&self, template: &str, args: &Stash) -> Result<String> {
        self.handlebars
            .render_template(template, &serde_json::Value::Object(args.clone()))
            .map_err(|e| MeshqlError::Template(e.to_string()))
    }

    fn build_pipeline(&self, query_json: &str, at: i64) -> Result<Vec<Document>> {
        let at_bson = bson::DateTime::from_millis(at);

        let json_val: serde_json::Value =
            serde_json::from_str(query_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let obj = json_val
            .as_object()
            .ok_or_else(|| MeshqlError::Parse("Query must be a JSON object".to_string()))?;
        let mut query_doc = stash_to_doc(obj);

        query_doc.insert("createdAt", doc! { "$lte": at_bson });

        let mut pipeline = vec![
            doc! { "$match": &query_doc },
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

        // No authorization stage. The mark is opaque, so it cannot become a
        // `$match`; the plugin is asked about each resolved envelope after the
        // fetch, and the limit is applied to what it authorized.

        // Canonical result ordering (meshql_core::envelope_order): the resolved
        // version's createdAt, then the envelope id. $group emits its buckets in
        // an unspecified order, so without this stage the result set — and any
        // $limit applied to it — is arbitrary. Mongo compares strings by byte,
        // matching the other adapters.
        pipeline.push(doc! { "$sort": { "createdAt": 1, "id": 1 } });

        Ok(pipeline)
    }

    /// Run one pipeline, ask the plugin about every envelope it resolved, and
    /// truncate to `limit` *afterwards* — so a limit truncates authorized rows
    /// rather than being consumed by rows the caller never gets to see.
    async fn authorized_rows(
        &self,
        query_json: &str,
        session: &dyn Session,
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Stash>> {
        let pipeline = self.build_pipeline(query_json, at)?;

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
            let Some(envelope) = document_to_envelope(&doc) else {
                continue;
            };
            if !session.is_authorized(Operation::Read, &envelope) {
                continue;
            }
            if let Some(stash) = document_to_result_stash(&doc) {
                results.push(stash);
            }
            if let Some(l) = limit {
                if results.len() as i64 >= l.max(0) {
                    break;
                }
            }
        }

        Ok(results)
    }
}

#[async_trait::async_trait]
impl Searcher for MongoSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>> {
        let query_json = self.render_template(template, args)?;
        let mut rows = self
            .authorized_rows(&query_json, session, at, Some(1))
            .await?;
        Ok(rows.pop())
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;
        let limit = args.get("limit").and_then(|v| v.as_i64());
        self.authorized_rows(&query_json, session, at, limit).await
    }
}

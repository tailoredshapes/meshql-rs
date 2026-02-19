use crate::converters::{document_to_result_stash, stash_to_doc};
use bson::{Bson, Document, doc};
use handlebars::Handlebars;
use meshql_core::{Auth, MeshqlError, Result, Searcher, Stash};
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

    fn build_pipeline(
        &self,
        query_json: &str,
        creds: &[String],
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Document>> {
        let at_bson = bson::DateTime::from_millis(at);
        let bson_tokens: Vec<Bson> = creds.iter().map(|s| Bson::String(s.clone())).collect();

        let json_val: serde_json::Value = serde_json::from_str(query_json)
            .map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let obj = json_val
            .as_object()
            .ok_or_else(|| MeshqlError::Parse("Query must be a JSON object".to_string()))?;
        let mut query_doc = stash_to_doc(obj);

        query_doc.insert("createdAt", doc! { "$lte": at_bson });
        query_doc.insert("authorizedTokens", doc! { "$in": bson_tokens });

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

        if let Some(l) = limit {
            pipeline.push(doc! { "$limit": l });
        }

        Ok(pipeline)
    }
}

#[async_trait::async_trait]
impl Searcher for MongoSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let query_json = self.render_template(template, args)?;
        let pipeline = self.build_pipeline(&query_json, creds, at, Some(1))?;

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
            Ok(document_to_result_stash(&doc))
        } else {
            Ok(None)
        }
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;
        let limit = args.get("limit").and_then(|v| v.as_i64());
        let pipeline = self.build_pipeline(&query_json, creds, at, limit)?;

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
            if let Some(stash) = document_to_result_stash(&doc) {
                results.push(stash);
            }
        }

        Ok(results)
    }
}

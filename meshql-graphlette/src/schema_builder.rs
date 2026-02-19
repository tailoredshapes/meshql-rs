use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputValue, Object, Scalar, Schema, TypeRef,
};
use async_graphql_parser::parse_schema;
use async_graphql_parser::types as pt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use chrono::Utc;
use meshql_core::{
    InternalSingletonResolverConfig, InternalVectorResolverConfig, RootConfig, Searcher,
    SingletonResolverConfig, Stash, VectorResolverConfig,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Maps graphlette path → searcher + root config for inter-graphlette resolution.
#[derive(Clone, Default)]
pub struct ResolverRegistry {
    entries: HashMap<String, RegistryEntry>,
}

#[derive(Clone)]
pub struct RegistryEntry {
    pub searcher: Arc<dyn Searcher>,
    pub root_config: RootConfig,
}

impl ResolverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        path: impl Into<String>,
        searcher: Arc<dyn Searcher>,
        root_config: RootConfig,
    ) {
        self.entries.insert(
            path.into(),
            RegistryEntry {
                searcher,
                root_config,
            },
        );
    }

    /// Given a URL like "http://localhost:3033/coop/graph" or just "/coop/graph", extract path.
    pub fn get_for_url(&self, url: &str) -> Option<&RegistryEntry> {
        let path = if let Ok(parsed) = url::Url::parse(url) {
            parsed.path().to_string()
        } else {
            url.to_string()
        };
        self.entries.get(&path)
    }
}

/// Convert parser Type (struct with base+nullable) to dynamic TypeRef.
fn convert_type(ty: &pt::Type) -> TypeRef {
    match (&ty.base, ty.nullable) {
        (pt::BaseType::Named(name), true) => TypeRef::named(name.as_ref()),
        (pt::BaseType::Named(name), false) => TypeRef::named_nn(name.as_ref()),
        (pt::BaseType::List(inner), nullable) => {
            let inner_nn = !inner.nullable;
            let (inner_name, inner_is_list) = match &inner.base {
                pt::BaseType::Named(n) => (n.as_ref().to_string(), false),
                pt::BaseType::List(_) => ("String".to_string(), true), // nested list — simplify
            };
            let _ = inner_is_list;
            match (nullable, inner_nn) {
                (true, false) => TypeRef::named_list(&inner_name),
                (true, true) => TypeRef::named_list_nn(&inner_name),
                (false, false) => TypeRef::named_nn_list(&inner_name),
                (false, true) => TypeRef::named_nn_list_nn(&inner_name),
            }
        }
    }
}

fn is_scalar(type_name: &str) -> bool {
    matches!(
        type_name,
        "String" | "Int" | "Float" | "Boolean" | "ID" | "Date"
    )
}

/// Get the base type name (unwrapping List wrappers).
fn base_type_name(ty: &pt::Type) -> &str {
    match &ty.base {
        pt::BaseType::Named(n) => n.as_ref(),
        pt::BaseType::List(inner) => base_type_name(inner),
    }
}

/// Convert async-graphql ConstValue to serde_json Value.
fn gql_value_to_json(v: &async_graphql::Value) -> serde_json::Value {
    match v {
        async_graphql::Value::Null => serde_json::Value::Null,
        async_graphql::Value::Boolean(b) => serde_json::Value::Bool(*b),
        async_graphql::Value::Number(n) => serde_json::Value::Number(n.clone()),
        async_graphql::Value::String(s) => serde_json::Value::String(s.clone()),
        async_graphql::Value::List(list) => {
            serde_json::Value::Array(list.iter().map(gql_value_to_json).collect())
        }
        async_graphql::Value::Object(obj) => serde_json::Value::Object(
            obj.iter()
                .map(|(k, v)| (k.to_string(), gql_value_to_json(v)))
                .collect(),
        ),
        async_graphql::Value::Enum(name) => serde_json::Value::String(name.to_string()),
        async_graphql::Value::Binary(b) => serde_json::Value::String(base64_encode(b)),
    }
}

fn base64_encode(data: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        result.push(alphabet[b0 >> 2] as char);
        result.push(alphabet[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            result.push(alphabet[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(alphabet[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Scalar field: extract value from parent Stash and convert to GraphQL value.
fn scalar_field(field_name: String, type_ref: TypeRef) -> Field {
    Field::new(field_name.clone(), type_ref, move |ctx| {
        let fname = field_name.clone();
        FieldFuture::new(async move {
            let stash = ctx.parent_value.try_downcast_ref::<Stash>()?;
            Ok(stash.get(&fname).cloned().map(|v| {
                let gql_val = async_graphql::to_value(v).unwrap_or(async_graphql::Value::Null);
                FieldValue::value(gql_val)
            }))
        })
    })
}

/// Singleton relation field: look up foreign key in parent, call target searcher.
fn singleton_resolver_field(
    field_name: String,
    type_ref: TypeRef,
    resolver: &SingletonResolverConfig,
    registry: &ResolverRegistry,
) -> Option<Field> {
    let entry = registry.get_for_url(&resolver.url)?;
    let searcher = Arc::clone(&entry.searcher);
    let template = entry
        .root_config
        .get_template(&resolver.query_name)?
        .to_string();
    let fk = resolver
        .foreign_key
        .clone()
        .unwrap_or_else(|| "id".to_string());

    Some(Field::new(field_name, type_ref, move |ctx| {
        let s = Arc::clone(&searcher);
        let tmpl = template.clone();
        let fk = fk.clone();
        FieldFuture::new(async move {
            let parent = ctx.parent_value.try_downcast_ref::<Stash>()?;
            let id_val = parent.get(&fk).and_then(|v| v.as_str()).unwrap_or("");
            if id_val.is_empty() {
                return Ok(FieldValue::NONE);
            }
            let mut args = Stash::new();
            args.insert(
                "id".to_string(),
                serde_json::Value::String(id_val.to_string()),
            );
            let at = Utc::now().timestamp_millis();
            match s.find(&tmpl, &args, &["*".to_string()], at).await {
                Ok(Some(stash)) => Ok(Some(FieldValue::owned_any(stash))),
                Ok(None) => Ok(FieldValue::NONE),
                Err(e) => Err(async_graphql::Error::new(e.to_string())),
            }
        })
    }))
}

/// Vector relation field: look up id in parent, call target searcher for list.
fn vector_resolver_field(
    field_name: String,
    type_ref: TypeRef,
    resolver: &VectorResolverConfig,
    registry: &ResolverRegistry,
) -> Option<Field> {
    let entry = registry.get_for_url(&resolver.url)?;
    let searcher = Arc::clone(&entry.searcher);
    let template = entry
        .root_config
        .get_template(&resolver.query_name)?
        .to_string();
    let fk = resolver.foreign_key.clone();

    Some(Field::new(field_name, type_ref, move |ctx| {
        let s = Arc::clone(&searcher);
        let tmpl = template.clone();
        let fk = fk.clone();
        FieldFuture::new(async move {
            let parent = ctx.parent_value.try_downcast_ref::<Stash>()?;
            let id_val = match &fk {
                Some(key) => parent.get(key).and_then(|v| v.as_str()).unwrap_or(""),
                None => parent.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            };
            let mut args = Stash::new();
            args.insert(
                "id".to_string(),
                serde_json::Value::String(id_val.to_string()),
            );
            let at = Utc::now().timestamp_millis();
            match s.find_all(&tmpl, &args, &["*".to_string()], at).await {
                Ok(stashes) => {
                    let items: Vec<FieldValue> =
                        stashes.into_iter().map(FieldValue::owned_any).collect();
                    Ok(Some(FieldValue::list(items)))
                }
                Err(e) => Err(async_graphql::Error::new(e.to_string())),
            }
        })
    }))
}

/// Internal singleton relation field: look up foreign key in parent, call target searcher via registry.
fn internal_singleton_resolver_field(
    field_name: String,
    type_ref: TypeRef,
    resolver: &InternalSingletonResolverConfig,
    registry: &ResolverRegistry,
) -> Option<Field> {
    let entry = registry.get_for_url(&resolver.graphlette_path)?;
    let searcher = Arc::clone(&entry.searcher);
    let template = entry
        .root_config
        .get_template(&resolver.query_name)?
        .to_string();
    let fk = resolver
        .foreign_key
        .clone()
        .unwrap_or_else(|| "id".to_string());

    Some(Field::new(field_name, type_ref, move |ctx| {
        let s = Arc::clone(&searcher);
        let tmpl = template.clone();
        let fk = fk.clone();
        FieldFuture::new(async move {
            let parent = ctx.parent_value.try_downcast_ref::<Stash>()?;
            let id_val = parent.get(&fk).and_then(|v| v.as_str()).unwrap_or("");
            if id_val.is_empty() {
                return Ok(FieldValue::NONE);
            }
            let mut args = Stash::new();
            args.insert(
                "id".to_string(),
                serde_json::Value::String(id_val.to_string()),
            );
            let at = Utc::now().timestamp_millis();
            match s.find(&tmpl, &args, &["*".to_string()], at).await {
                Ok(Some(stash)) => Ok(Some(FieldValue::owned_any(stash))),
                Ok(None) => Ok(FieldValue::NONE),
                Err(e) => Err(async_graphql::Error::new(e.to_string())),
            }
        })
    }))
}

/// Internal vector relation field: look up id in parent, call target searcher for list via registry.
fn internal_vector_resolver_field(
    field_name: String,
    type_ref: TypeRef,
    resolver: &InternalVectorResolverConfig,
    registry: &ResolverRegistry,
) -> Option<Field> {
    let entry = registry.get_for_url(&resolver.graphlette_path)?;
    let searcher = Arc::clone(&entry.searcher);
    let template = entry
        .root_config
        .get_template(&resolver.query_name)?
        .to_string();
    let fk = resolver.foreign_key.clone();

    Some(Field::new(field_name, type_ref, move |ctx| {
        let s = Arc::clone(&searcher);
        let tmpl = template.clone();
        let fk = fk.clone();
        FieldFuture::new(async move {
            let parent = ctx.parent_value.try_downcast_ref::<Stash>()?;
            let id_val = match &fk {
                Some(key) => parent.get(key).and_then(|v| v.as_str()).unwrap_or(""),
                None => parent.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            };
            let mut args = Stash::new();
            args.insert(
                "id".to_string(),
                serde_json::Value::String(id_val.to_string()),
            );
            let at = Utc::now().timestamp_millis();
            match s.find_all(&tmpl, &args, &["*".to_string()], at).await {
                Ok(stashes) => {
                    let items: Vec<FieldValue> =
                        stashes.into_iter().map(FieldValue::owned_any).collect();
                    Ok(Some(FieldValue::list(items)))
                }
                Err(e) => Err(async_graphql::Error::new(e.to_string())),
            }
        })
    }))
}

/// Null field: returns None (for relation fields with no registered resolver).
fn null_field(field_name: String, type_ref: TypeRef) -> Field {
    Field::new(field_name, type_ref, |_ctx| {
        FieldFuture::new(async move { Ok(FieldValue::NONE) })
    })
}

/// Build a complete dynamic Schema from a GraphQL SDL + RootConfig + Searcher.
pub fn build_schema(
    schema_text: &str,
    root_config: &RootConfig,
    searcher: Arc<dyn Searcher>,
    registry: &ResolverRegistry,
) -> async_graphql::Result<Schema> {
    let service_doc = parse_schema(schema_text)
        .map_err(|e| async_graphql::Error::new(format!("Schema parse error: {e}")))?;

    // Collect object type definitions keyed by name
    let mut object_types: HashMap<String, Vec<pt::FieldDefinition>> = HashMap::new();
    for def in &service_doc.definitions {
        if let pt::TypeSystemDefinition::Type(td) = def {
            let type_def = &td.node;
            if let pt::TypeKind::Object(obj) = &type_def.kind {
                let name = type_def.name.node.to_string();
                let fields: Vec<pt::FieldDefinition> =
                    obj.fields.iter().map(|f| f.node.clone()).collect();
                object_types.insert(name, fields);
            }
        }
    }

    let mut schema_builder = Schema::build("Query", None, None);
    schema_builder = schema_builder.register(Scalar::new("Date"));

    // Build Query type
    if let Some(query_fields) = object_types.get("Query") {
        let mut query_obj = Object::new("Query");

        for field_def in query_fields {
            let field_name = field_def.name.node.to_string();
            let field_type = convert_type(&field_def.ty.node);

            if let Some(qc) = root_config.queries.iter().find(|q| q.name == field_name) {
                let template = qc.template.clone();
                let is_singleton = qc.is_singleton;
                let s = Arc::clone(&searcher);

                let mut gql_field = Field::new(field_name.clone(), field_type, move |ctx| {
                    let s = Arc::clone(&s);
                    let tmpl = template.clone();
                    FieldFuture::new(async move {
                        let at = ctx
                            .args
                            .get("at")
                            .and_then(|v| {
                                if let async_graphql::Value::Number(n) = v.as_value() {
                                    n.as_i64()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| Utc::now().timestamp_millis());

                        let mut args = Stash::new();
                        for (k, v) in ctx.args.iter() {
                            if k.as_str() != "at" {
                                let json_val = gql_value_to_json(v.as_value());
                                args.insert(k.to_string(), json_val);
                            }
                        }

                        let creds = &["*".to_string()];
                        if is_singleton {
                            match s.find(&tmpl, &args, creds, at).await {
                                Ok(Some(stash)) => Ok(Some(FieldValue::owned_any(stash))),
                                Ok(None) => Ok(FieldValue::NONE),
                                Err(e) => Err(async_graphql::Error::new(e.to_string())),
                            }
                        } else {
                            match s.find_all(&tmpl, &args, creds, at).await {
                                Ok(stashes) => {
                                    let items: Vec<FieldValue> =
                                        stashes.into_iter().map(FieldValue::owned_any).collect();
                                    Ok(Some(FieldValue::list(items)))
                                }
                                Err(e) => Err(async_graphql::Error::new(e.to_string())),
                            }
                        }
                    })
                });

                // Add arguments from the field definition
                for arg_def in &field_def.arguments {
                    let arg_name = arg_def.node.name.node.to_string();
                    let arg_type = convert_type(&arg_def.node.ty.node);
                    gql_field = gql_field.argument(InputValue::new(arg_name, arg_type));
                }

                query_obj = query_obj.field(gql_field);
            }
        }

        schema_builder = schema_builder.register(query_obj);
    }

    // Build entity types
    for (type_name, fields) in &object_types {
        if type_name == "Query" {
            continue;
        }

        let mut entity_obj = Object::new(type_name.as_str());

        for field_def in fields {
            let field_name = field_def.name.node.to_string();
            let field_type = convert_type(&field_def.ty.node);
            let base_name = base_type_name(&field_def.ty.node).to_string();

            if is_scalar(&base_name) {
                entity_obj = entity_obj.field(scalar_field(field_name, field_type));
            } else {
                // Check singleton resolvers (exact field name match)
                let singleton = root_config
                    .singleton_resolvers
                    .iter()
                    .find(|r| r.field_name == field_name);

                // Check internal singleton resolvers
                let internal_singleton = root_config
                    .internal_singleton_resolvers
                    .iter()
                    .find(|r| r.field_name == field_name);

                // Check vector resolvers (exact match or nested path, e.g. "hens.layReports")
                let vector = root_config.vector_resolvers.iter().find(|r| {
                    r.field_name == field_name
                        || r.field_name
                            .rsplit_once('.')
                            .map(|(_, suffix)| suffix == field_name)
                            .unwrap_or(false)
                });

                // Check internal vector resolvers
                let internal_vector = root_config.internal_vector_resolvers.iter().find(|r| {
                    r.field_name == field_name
                        || r.field_name
                            .rsplit_once('.')
                            .map(|(_, suffix)| suffix == field_name)
                            .unwrap_or(false)
                });

                let field = if let Some(r) = singleton {
                    singleton_resolver_field(field_name.clone(), field_type.clone(), r, registry)
                        .unwrap_or_else(|| null_field(field_name, field_type))
                } else if let Some(r) = internal_singleton {
                    internal_singleton_resolver_field(
                        field_name.clone(),
                        field_type.clone(),
                        r,
                        registry,
                    )
                    .unwrap_or_else(|| null_field(field_name, field_type))
                } else if let Some(r) = vector {
                    vector_resolver_field(field_name.clone(), field_type.clone(), r, registry)
                        .unwrap_or_else(|| null_field(field_name, field_type))
                } else if let Some(r) = internal_vector {
                    internal_vector_resolver_field(
                        field_name.clone(),
                        field_type.clone(),
                        r,
                        registry,
                    )
                    .unwrap_or_else(|| null_field(field_name, field_type))
                } else {
                    null_field(field_name, field_type)
                };

                entity_obj = entity_obj.field(field);
            }
        }

        schema_builder = schema_builder.register(entity_obj);
    }

    schema_builder
        .finish()
        .map_err(|e| async_graphql::Error::new(e.to_string()))
}

/// Axum Router serving a GraphQL schema at the given path.
pub struct GraphletteRouter;

impl GraphletteRouter {
    pub fn build(path: &str, schema: Schema) -> Router {
        let schema = Arc::new(schema);
        Router::new().route(
            path,
            post(move |body: axum::body::Bytes| {
                let schema = Arc::clone(&schema);
                async move {
                    let request: async_graphql::Request = match serde_json::from_slice(&body) {
                        Ok(r) => r,
                        Err(e) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(serde_json::json!({
                                    "errors": [{"message": e.to_string()}]
                                })),
                            )
                                .into_response();
                        }
                    };
                    let response = schema.execute(request).await;
                    let body = serde_json::json!({
                        "data": response.data,
                        "errors": if response.errors.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::to_value(&response.errors).unwrap_or(serde_json::Value::Null)
                        },
                    });
                    axum::Json(body).into_response()
                }
            }),
        )
    }
}

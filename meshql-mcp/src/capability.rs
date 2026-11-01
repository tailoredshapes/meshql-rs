//! Declarative tools. Reads use GraphQL variables; writes retain REST authority.
use crate::catalogue;
use crate::template::{substitute, substitute_path, substitute_value};
use crate::{
    client::MeshqlClient,
    tool::{Tool, ToolFuture, ToolHandler},
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct Capability {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub handler: CapabilityHandler,
}
#[derive(Clone)]
pub enum CapabilityHandler {
    GraphQuery {
        path: String,
        query_template: String,
    },
    GraphQueryVariables {
        path: String,
        query: String,
    },
    RestGet {
        path_template: String,
    },
    RestPost {
        path_template: String,
        body_template: Option<Value>,
    },
    EntityCreate {
        api_path: String,
    },
    EntityUpdate {
        api_path: String,
    },
    EntityDelete {
        api_path: String,
    },
    Custom(ToolHandler),
}
impl Capability {
    pub fn with_description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }
    pub(crate) fn into_tool(self, _: Arc<MeshqlClient>) -> Tool {
        let Capability {
            name,
            description,
            input_schema,
            handler,
        } = self;
        let dispatcher: ToolHandler = Arc::new(move |client, args| -> ToolFuture {
            let handler = handler.clone();
            Box::pin(async move { dispatch(handler, client, args).await })
        });
        Tool {
            name,
            description,
            input_schema,
            handler: dispatcher,
        }
    }
}
fn id(args: &Value) -> anyhow::Result<String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("id must be a string"))?;
    crate::template::segment(id)
}
fn safe_graph_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<crate::client::BackendStatus>() {
        error
    } else {
        anyhow::anyhow!("GraphQL backend request failed")
    }
}
async fn dispatch(
    h: CapabilityHandler,
    client: Arc<MeshqlClient>,
    args: Value,
) -> anyhow::Result<Value> {
    match h {
        CapabilityHandler::GraphQuery {
            path,
            query_template,
        } => client
            .gql(&path, &substitute(&query_template, &args)?)
            .await
            .map_err(safe_graph_error),
        CapabilityHandler::GraphQueryVariables { path, query } => client
            .gql_variables(&path, &query, args)
            .await
            .map_err(safe_graph_error),
        CapabilityHandler::RestGet { path_template } => {
            client
                .get_path(&substitute_path(&path_template, &args)?)
                .await
        }
        CapabilityHandler::RestPost {
            path_template,
            body_template,
        } => {
            let body = match body_template {
                Some(t) => substitute_value(&t, &args)?,
                None => serde_json::json!({}),
            };
            client
                .post_path(&substitute_path(&path_template, &args)?, &body)
                .await
        }
        CapabilityHandler::EntityCreate { api_path } => {
            client
                .post_path(
                    &api_path,
                    args.get("payload")
                        .ok_or_else(|| anyhow::anyhow!("payload required"))?,
                )
                .await
        }
        CapabilityHandler::EntityUpdate { api_path } => {
            client
                .put_path(
                    &format!("{api_path}/{}", id(&args)?),
                    args.get("patch")
                        .ok_or_else(|| anyhow::anyhow!("patch required"))?,
                )
                .await
        }
        CapabilityHandler::EntityDelete { api_path } => {
            client
                .delete_path(&format!("{api_path}/{}", id(&args)?))
                .await
        }
        CapabilityHandler::Custom(handler) => handler(client, args).await,
    }
}
#[derive(Default)]
pub struct CapabilitiesBuilder {
    capabilities: Vec<Capability>,
    errors: Vec<String>,
}
impl CapabilitiesBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    /// Read discovery only. Write schemas must come from actual restlettes.
    pub fn auto_from_schemas(
        mut self,
        schemas: &[(&'static str, &'static str, &'static str)],
    ) -> Self {
        for (entity, path, text) in schemas {
            match catalogue::reads(entity, path, text) {
                Ok(capabilities) => {
                    for cap in capabilities {
                        self = self.add(cap)
                    }
                }
                Err(e) => self.errors.push(e.to_string()),
            }
        }
        self
    }
    pub fn with_rest_schema(mut self, entity: &'static str, path: &str, schema: Value) -> Self {
        match catalogue::writes(entity, path, schema) {
            Ok(capabilities) => {
                for cap in capabilities {
                    self = self.add(cap)
                }
            }
            Err(e) => self.errors.push(e.to_string()),
        }
        self
    }
    pub fn describe(mut self, name: &'static str, description: &'static str) -> Self {
        if let Some(cap) = self.capabilities.iter_mut().find(|c| c.name == name) {
            cap.description = description;
        } else {
            eprintln!("[meshql-mcp] describe: no capability named {name}");
        }
        self
    }
    // Preserve the established fluent builder API.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, capability: Capability) -> Self {
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|c| c.name == capability.name)
        {
            *existing = capability;
        } else {
            self.capabilities.push(capability);
        }
        self
    }
    pub fn try_build(self) -> anyhow::Result<Vec<Capability>> {
        anyhow::ensure!(
            self.errors.is_empty(),
            "invalid MCP catalogue: {}",
            self.errors.join("; ")
        );
        Ok(self.capabilities)
    }
    pub fn build(self) -> Vec<Capability> {
        self.try_build().expect("invalid MCP catalogue")
    }
}
#[cfg(test)]
#[path = "capability_tests.rs"]
mod tests;

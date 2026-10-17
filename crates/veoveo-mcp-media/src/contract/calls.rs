//! Media as a provider of calls between servers. Each tool's request names its tool and
//! its typed result, so a server that declares uses of Media's permissions calls Media
//! through the Hub with the SDK's `HubClient`; `run` starts a Task whose handle
//! resolves with the generation's result.
use veoveo_hub_sdk::calls::{ProviderContract, TaskToolCall, ToolCall};
use veoveo_types::ServerSlug;

use super::{
    GenerationResult, ModelCatalogPage, ModelSchema, ModelSchemaRequest, ModelsRequest, RunRequest,
};

/// The Media server, the provider these calls reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaProvider;

impl ProviderContract for MediaProvider {
    fn slug() -> ServerSlug {
        crate::server()
    }
}

impl ToolCall for ModelsRequest {
    type Provider = MediaProvider;
    type Output = ModelCatalogPage;
    const TOOL: &'static str = "models";
}

impl ToolCall for ModelSchemaRequest {
    type Provider = MediaProvider;
    type Output = ModelSchema;
    const TOOL: &'static str = "model_schema";
}

impl ToolCall for RunRequest {
    type Provider = MediaProvider;
    type Output = GenerationResult;
    const TOOL: &'static str = "run";
}

impl TaskToolCall for RunRequest {}

/// Every tool Media serves, as its typed calls name them.
pub fn served_tools() -> [&'static str; 3] {
    [
        <ModelsRequest as ToolCall>::TOOL,
        <ModelSchemaRequest as ToolCall>::TOOL,
        <RunRequest as ToolCall>::TOOL,
    ]
}

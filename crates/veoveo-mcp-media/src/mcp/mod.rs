//! Media's MCP surface. Each tool checks its permission against the requirement table;
//! `models` and `model_schema` answer from the catalog snapshot, and `run` starts a
//! durable Task through the SDK's adapter.
mod declarations;
mod errors;
pub mod identity;
mod prompts;
mod requirements;
mod resources;
mod setup;
mod subscriptions;
mod tasks;

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, CompleteRequestParams, CompleteResult, GetPromptRequestParams,
        GetPromptResult, Prompt, ReadResourceRequestParams, Reference,
    },
    service::RequestContext,
    tool, tool_router,
};
use veoveo_sdk_rust::{
    hosting::{
        DomainAddress, DomainRead, DomainServer, completion, rank_completions, structured_result,
        unknown_prompt,
    },
    server_contract::McpServerSetup,
};

use crate::{
    catalog::Catalog,
    contract::{
        GenerationResult, ModelCatalogPage, ModelSchema, ModelSchemaRequest, ModelsRequest,
        RunRequest,
    },
    predictions::Predictions,
};

pub use declarations::{REGISTRATION, registration};
pub use prompts::MediaPrompt;
pub use requirements::AUTHORIZATION;
pub use setup::MediaContract;
pub use subscriptions::{MediaSubscriptions, channel, watch_changes};
pub use tasks::MediaTasks;

use errors::catalog_error;
use requirements::{Target, require};
use setup::SERVER_SETUP;

/// The template whose `model_id` the server completes.
const MODEL_TEMPLATE: &str = "media://model/{+model_id}";

/// Build and check the MCP setup, so a bad declaration stops startup.
pub fn force_setup() {
    std::sync::LazyLock::force(&SERVER_SETUP);
}

/// What every handler shares: the caller's predictions and the model catalog.
pub struct MediaApp {
    pub predictions: Predictions,
    pub catalog: Arc<Catalog>,
}

#[derive(Clone)]
pub struct MediaMcp {
    app: Arc<MediaApp>,
    tool_router: ToolRouter<MediaMcp>,
}

#[tool_router]
impl MediaMcp {
    pub fn new(app: Arc<MediaApp>) -> Self {
        Self {
            app,
            tool_router: Self::tools(),
        }
    }

    /// Every tool Media serves.
    pub(crate) fn tools() -> ToolRouter<Self> {
        Self::tool_router()
    }

    #[tool(
        title = "List models",
        description = "A page of models in identity order. Filter with `query` (searched in a model's identity, name, type and description) or `type`, and read the next page by passing the page's `nextCursor` as `cursor` with the same filters and limit.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<ModelCatalogPage>(),
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn models(
        &self,
        Parameters(request): Parameters<ModelsRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        require(&context, Target::Tool("models"))?;
        let snapshot = self.app.catalog.snapshot().map_err(catalog_error)?;
        let page = snapshot.page(&request).map_err(catalog_error)?;
        structured_result(
            format!("{} of {} models", page.models.len(), page.total_available),
            &page,
        )
    }

    #[tool(
        title = "Read a model's input schema",
        description = "One model with the JSON Schema `run` checks its input against.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<ModelSchema>(),
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn model_schema(
        &self,
        Parameters(request): Parameters<ModelSchemaRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        require(&context, Target::Tool("model_schema"))?;
        let snapshot = self.app.catalog.snapshot().map_err(catalog_error)?;
        let schema = snapshot.schema(&request.model).map_err(catalog_error)?;
        structured_result(format!("model {}", schema.model_id), &schema)
    }

    #[tool(
        title = "Run a model",
        description = "Generate with a model as a Task. The input must satisfy the model's schema at `media://model/{+model_id}`. The finished Task returns the Artifact address of each output and carries the run's cost in its `ai.veoveo/usage` metadata. Call it with Task support.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<GenerationResult>(),
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn run(
        &self,
        Parameters(_request): Parameters<RunRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        require(&context, Target::Tool("run"))?;
        Err(McpError::invalid_request(
            "run runs as a Task; call it with Task support",
            None,
        ))
    }
}

impl DomainServer for MediaMcp {
    type Contract = MediaContract;

    fn setup() -> &'static McpServerSetup<MediaContract> {
        &SERVER_SETUP
    }

    fn tool_router(&self) -> &ToolRouter<Self> {
        &self.tool_router
    }

    async fn read(
        &self,
        address: DomainAddress<MediaContract>,
        request: &ReadResourceRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<DomainRead, McpError> {
        resources::read(&self.app, address, request, context).await
    }

    fn prompts(&self) -> Vec<Prompt> {
        MediaPrompt::ALL
            .into_iter()
            .map(MediaPrompt::definition)
            .collect()
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        require(&context, Target::Prompt(&request.name))?;
        MediaPrompt::by_name(&request.name)
            .ok_or_else(|| unknown_prompt(&request.name))?
            .render(request.arguments)
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        let Reference::Resource(reference) = &request.r#ref else {
            return Ok(CompleteResult::default());
        };
        if reference.uri != MODEL_TEMPLATE || request.argument.name != "model_id" {
            return Ok(CompleteResult::default());
        }
        require(&context, Target::Resource("media://models"))?;
        let snapshot = self.app.catalog.snapshot().map_err(catalog_error)?;
        let models = snapshot.models().map(|model| model.model_id.as_str());
        completion(rank_completions(models, &request.argument.value))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_setup_and_documents_admit() {
        std::sync::LazyLock::force(&super::SERVER_SETUP);
        assert!(super::setup::SERVER_DOCS.agent_manual().is_some());
    }
}

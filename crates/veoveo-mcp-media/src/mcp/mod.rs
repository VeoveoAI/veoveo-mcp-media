//! Typed routes bind catalog pages, model schemas and durable runs to current policies.
mod declarations;
mod errors;
pub mod identity;
mod prompts;
mod requirements;
mod resources;
mod setup;
mod subscriptions;
mod tasks;

use std::sync::{Arc, LazyLock};

use rmcp::{
    ErrorData as McpError, RoleServer,
    model::{ClientCapabilities, CompleteRequestParams, CompleteResult, Reference},
    service::RequestContext,
};
use veoveo_hub_sdk::{
    definition::{List, Product},
    hosting::{DomainServer, RoutePolicy, Routes, completion, hub_identity, rank_completions},
    server_contract::McpServerSetup,
};
use veoveo_types::Page;

use crate::{
    catalog::Catalog,
    contract::{
        GenerationResult, ModelCatalog, ModelSchema, ModelSchemaRequest, ModelsTool, RunRequest,
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

const MODEL_TEMPLATE: &str = "media://model/{+model_id}";

pub fn force_setup() {
    LazyLock::force(&SERVER_SETUP);
}

pub struct MediaApp {
    pub predictions: Predictions,
    pub catalog: Arc<Catalog>,
}

#[derive(Clone)]
pub struct MediaMcp {
    app: Arc<MediaApp>,
}
impl MediaMcp {
    pub fn new(app: Arc<MediaApp>) -> Self {
        Self { app }
    }

    async fn models(
        &self,
        request: List<ModelsTool>,
        context: RequestContext<RoleServer>,
    ) -> Result<Page<ModelCatalog>, McpError> {
        hub_identity(&context)?;
        self.app
            .catalog
            .snapshot()
            .map_err(catalog_error)?
            .page(request.selection())
            .map_err(catalog_error)
    }

    async fn model_schema(
        &self,
        request: ModelSchemaRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<ModelSchema, McpError> {
        hub_identity(&context)?;
        self.app
            .catalog
            .snapshot()
            .map_err(catalog_error)?
            .schema(&request.model)
            .map_err(catalog_error)
    }

    async fn run(
        &self,
        _: RunRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<Product<GenerationResult>, McpError> {
        Err(McpError::missing_required_client_capability(
            ClientCapabilities::builder().enable_tasks().build(),
        ))
    }
}

static ROUTES: LazyLock<Routes<MediaMcp>> = LazyLock::new(|| {
    use crate::contract::MediaPermission::*;
    use veoveo_mcp_artifact::contract::ArtifactPermission;
    let policy = |permission| RoutePolicy::permission(permission).expect("Media permission");
    let routes = Routes::<MediaMcp>::builder(policy(ModelsRead))
        .uses(ArtifactPermission::ArtifactsRead.name())
        .uses(ArtifactPermission::ArtifactsWrite.name())
        .collection::<ModelsTool, _, _>(
            "A page of models in identity order. Select `filters.query` or `filters.type`; continue with the same filters and limit using `nextCursor` as `cursor`.",
            policy(ModelsRead),
            |domain, request, context| async move { domain.models(request, context).await },
        )
        .tool::<ModelSchemaRequest, _, _>(
            "One model with the JSON Schema `run` checks its input against.",
            policy(ModelsRead),
            |domain, request, context| async move { domain.model_schema(request, context).await.map_err(Into::into) },
        )
        .task::<RunRequest, _, _>(
            "Generate with a model as a Task. Input must satisfy its schema at `media://model/{+model_id}`. The finished Task returns Artifact addresses and carries cost in `ai.veoveo/usage`. Call with Task support.",
            policy(GenerationsRun),
            |domain, request, context| async move { domain.run(request, context).await.map_err(Into::into) },
        );
    let mut routes = resources::register(routes);
    for prompt in MediaPrompt::ALL {
        let permission = match prompt {
            MediaPrompt::TaskReview => PredictionsRead,
            _ => ModelsRead,
        };
        routes = routes.prompt(
            prompt.definition(),
            policy(permission),
            move |_, request, _| async move { prompt.render(request.arguments) },
        );
    }
    routes.build()
});

impl DomainServer for MediaMcp {
    type Contract = MediaContract;
    fn setup() -> &'static McpServerSetup<MediaContract> {
        &SERVER_SETUP
    }
    fn routes() -> &'static Routes<Self> {
        &ROUTES
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
        completion(rank_completions(
            snapshot.models().map(|model| model.model_id.as_str()),
            &request.argument.value,
        ))
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

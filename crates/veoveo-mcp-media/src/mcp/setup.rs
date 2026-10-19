//! Media's checked contract: slug, scheme, scope, documents, the fixed resources it lists
//! and the templates it declares.
use std::sync::LazyLock;

use rmcp::model::{ServerCapabilities, ServerConfig};
use veoveo_hub_sdk::{
    docs::ServerDocs,
    server_contract::{McpServerContract, McpServerSetup},
};
use veoveo_types::ResourceScheme;

use crate::contract::{MediaProvider, MediaScope};

/// The crate documents embedded at build time and served under `media://docs`,
/// `media://docs/{doc_id}`, `media://contract` and the admin document routes.
pub(crate) static SERVER_DOCS: LazyLock<ServerDocs> =
    LazyLock::new(|| veoveo_hub_sdk::server_docs!("media"));

pub struct MediaContract;

pub(crate) static SERVER_SETUP: LazyLock<McpServerSetup<MediaContract>> =
    LazyLock::new(|| McpServerSetup::new().expect("Media MCP contract setup"));

impl McpServerContract for MediaContract {
    type Scope = MediaScope;
    type Server = super::MediaMcp;
    type Provider = MediaProvider;

    fn scheme() -> ResourceScheme {
        ResourceScheme::parse("media").expect("declared Media scheme")
    }
    fn scopes() -> &'static [MediaScope] {
        <MediaScope as veoveo_types::Vocabulary>::ALL
    }
    fn documents() -> &'static ServerDocs {
        &SERVER_DOCS
    }
    fn server_config() -> ServerConfig {
        server_config()
    }
}

fn server_config() -> ServerConfig {
    let mut capabilities = ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_prompts()
        .enable_completions()
        .build();
    // `run` runs as a Task, so Media declares the Tasks extension.
    capabilities.extensions.get_or_insert_default().insert(
        rmcp::model::TASKS_EXTENSION_ID.to_owned(),
        rmcp::model::JsonObject::new(),
    );
    if let Some(registration) = super::declarations::REGISTRATION.get() {
        veoveo_hub_sdk::registration::mcp::advertise(
            &mut capabilities,
            registration,
            &super::requirements::AUTHORIZATION,
        );
    }
    let mut info = ServerConfig::default();
    info.capabilities = capabilities;
    info.server_info = rmcp::model::Implementation::new("media", env!("CARGO_PKG_VERSION"));
    info.instructions = Some(
        "Generate images, video and audio with provider models. Browse `media://models` or \
         call `models`, read a model's input schema at `media://model/{+model_id}` or with \
         `model_schema`, then call `run` as a Task with the model and an input that satisfies \
         the schema. A finished run returns the Artifact addresses of its outputs; read them \
         through Artifact. Each run's cost appears on its Task."
            .to_owned(),
    );
    info
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_setup_round_trips_every_resource_and_template() {
        let setup = &*super::SERVER_SETUP;
        assert_eq!(setup.resources().len(), 6);
        assert_eq!(setup.resource_templates().len(), 6);
    }
}

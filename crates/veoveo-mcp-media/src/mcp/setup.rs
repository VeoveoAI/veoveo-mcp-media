//! Media's checked contract: slug, scheme, scope, documents, the fixed resources it lists
//! and the templates it declares.
use std::sync::LazyLock;

use rmcp::model::{Resource, ResourceTemplate, ServerCapabilities, ServerConfig};
use veoveo_sdk_rust::{
    ServerSlug,
    docs::ServerDocs,
    server_contract::{
        McpResource, McpResourceTemplate, McpServerContract, McpServerSetup, McpSetupError,
    },
};
use veoveo_types::{ResourceScheme, ResourceTemplateUri};

use crate::contract::{MediaDocument, MediaResource, MediaScope};

/// The crate documents embedded at build time and served under `media://docs`,
/// `media://docs/{doc_id}`, `media://contract` and the admin document routes.
pub(crate) static SERVER_DOCS: LazyLock<ServerDocs> =
    LazyLock::new(|| veoveo_sdk_rust::server_docs!("media"));

pub struct MediaContract;

pub(crate) static SERVER_SETUP: LazyLock<McpServerSetup<MediaContract>> =
    LazyLock::new(|| McpServerSetup::new().expect("Media MCP contract setup"));

impl McpServerContract for MediaContract {
    type Scope = MediaScope;
    type Resource = MediaResource;

    fn slug() -> ServerSlug {
        crate::server()
    }
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
    fn resources() -> Result<Vec<McpResource<MediaResource>>, McpSetupError> {
        resources()
    }
    fn resource_templates() -> Result<Vec<McpResourceTemplate>, McpSetupError> {
        resource_templates()
    }
}

fn described(
    address: MediaResource,
    title: &str,
    description: &str,
    mime: &str,
) -> Result<McpResource<MediaResource>, McpSetupError> {
    McpResource::new(address, |uri| {
        Resource::new(uri, title)
            .with_title(title)
            .with_description(description)
            .with_mime_type(mime)
    })
}

fn resources() -> Result<Vec<McpResource<MediaResource>>, McpSetupError> {
    let mut resources = vec![described(
        MediaResource::Docs,
        "Server documents",
        "Index of the crate documents embedded at build time.",
        "application/json",
    )?];
    for (document, title) in [
        (MediaDocument::Agents, "Agent work manual"),
        (MediaDocument::Design, "Domain design"),
    ] {
        resources.push(described(
            MediaResource::Document(document),
            title,
            "Crate document embedded at build time.",
            "text/markdown",
        )?);
    }
    resources.push(described(
        MediaResource::Contract,
        "Contract declaration",
        "Machine-readable contract revision and compliance declaration.",
        "application/json",
    )?);
    resources.push(described(
        MediaResource::models(),
        "Models",
        "The first page of 20 models in identity order. Filter with `query` or `type`, set \
         `limit` up to 100, and read a later page by adding the page's `nextCursor` as `cursor`.",
        "application/json",
    )?);
    resources.push(described(
        MediaResource::predictions(),
        "Your predictions",
        "The first page of up to 100 of your runs, in the order they started.",
        "application/json",
    )?);
    Ok(resources)
}

fn resource_templates() -> Result<Vec<McpResourceTemplate>, McpSetupError> {
    [
        (
            "media://docs/{doc_id}",
            "Server document",
            "Embedded crate document body.",
            "text/markdown",
        ),
        (
            "media://models{?query,type,limit,cursor}",
            "Models",
            "A page of models matching a query or a type.",
            "application/json",
        ),
        (
            "media://model/{+model_id}",
            "Model",
            "One model with the JSON Schema its run input must satisfy.",
            "application/json",
        ),
        (
            "media://predictions{?cursor}",
            "Your predictions",
            "A page of your runs, in the order they started.",
            "application/json",
        ),
        (
            "media://prediction/{prediction_id}",
            "Prediction",
            "One of your runs by the provider's prediction.",
            "application/json",
        ),
        (
            "media://prediction/{prediction_id}/result",
            "Generation result",
            "A finished run's result: its prediction and the Artifact addresses of its outputs.",
            "application/json",
        ),
    ]
    .into_iter()
    .map(|(uri, title, description, mime)| {
        let template = ResourceTemplateUri::new(uri).map_err(|_| McpSetupError::InvalidTemplate)?;
        McpResourceTemplate::new(template, |uri| {
            ResourceTemplate::new(uri, title)
                .with_title(title)
                .with_description(description)
                .with_mime_type(mime)
        })
    })
    .collect()
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
        veoveo_sdk_rust::registration::mcp::advertise(
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

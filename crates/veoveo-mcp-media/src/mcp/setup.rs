//! Media's checked contract: slug, scheme, scope, documents, skills, the fixed resources
//! it lists and the templates it declares.
use std::sync::LazyLock;

use rmcp::model::{ServerCapabilities, ServerConfig};
use veoveo_hub_sdk::{
    docs::ServerDocs,
    server_contract::{McpServerContract, McpServerSetup},
    skills::ServerSkills,
};
use veoveo_types::ResourceScheme;

use crate::contract::{MediaProvider, MediaScope};

/// The crate documents embedded at build time and served under `media://docs`,
/// `media://docs/{doc_id}`, `media://contract` and the admin document routes.
pub(crate) static SERVER_DOCS: LazyLock<ServerDocs> =
    LazyLock::new(|| veoveo_hub_sdk::server_docs!("media"));

/// The skills under `skills/`, embedded at build time and served at `skill://media/…`.
pub(crate) static SERVER_SKILLS: LazyLock<ServerSkills> =
    LazyLock::new(|| veoveo_hub_sdk::server_skills!("media"));

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
    fn skills() -> &'static ServerSkills {
        &SERVER_SKILLS
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
    info.instructions = Some(INSTRUCTIONS.to_owned());
    info
}

/// What the server is and which skills say how to use it.
const INSTRUCTIONS: &str = "Media generates images, video, audio and 3D with provider \
     models, for a cost, and stores each output as an Artifact of the person. Its skills \
     say how: skill://media/generate-media/SKILL.md and skill://media/review-a-run/SKILL.md.";

#[cfg(test)]
mod tests {
    #[test]
    fn the_instructions_name_every_skill_the_server_serves() {
        let skills = &*super::SERVER_SKILLS;
        assert_eq!(skills.skills().len(), 2);
        for skill in skills.skills() {
            assert!(super::INSTRUCTIONS.contains(&skill.uri), "{}", skill.uri);
        }
    }

    #[test]
    fn the_setup_round_trips_every_resource_and_template() {
        let setup = &*super::SERVER_SETUP;
        assert_eq!(setup.resources().len(), 6);
        assert_eq!(setup.resource_templates().len(), 6);
    }
}

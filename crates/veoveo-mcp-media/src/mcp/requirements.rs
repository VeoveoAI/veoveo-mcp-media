//! The one table that says which permission each of Media's targets requires. It builds
//! the `ai.veoveo/authorization` declaration the Hub evaluates, with the permissions
//! Media uses on Artifact, and every handler checks the same declaration against the
//! permissions the Hub granted.
use std::sync::LazyLock;

use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use veoveo_hub_sdk::{
    HubInternalIdentity, LocalToolName, PromptName,
    hosting::hub_identity,
    registration::{
        AuthorizationBuilder, AuthorizationDeclaration, DeclarationError, RequirementTarget,
    },
    server_contract::McpServerContract,
};
use veoveo_mcp_artifact::contract::ArtifactPermission;
use veoveo_types::{PermissionName, ResourceTemplateUri, ResourceUri};

use super::setup::MediaContract;
use crate::contract::MediaPermission::{self, *};

/// Every tool and the permission it requires.
pub(crate) const TOOLS: &[(&str, MediaPermission)] = &[
    ("models", ModelsRead),
    ("model_schema", ModelsRead),
    ("run", GenerationsRun),
];

/// Every fixed resource.
pub(crate) const RESOURCES: &[(&str, MediaPermission)] = &[
    ("media://docs", ModelsRead),
    ("media://contract", ModelsRead),
    ("media://models", ModelsRead),
    ("media://predictions", PredictionsRead),
];

/// Every parameterized resource, as simple templates that match the concrete URIs the
/// server reads. A trailing variable takes the rest of the URI, so
/// `media://model/{model_id}` covers every model and `media://prediction/{prediction_id}`
/// also covers its result.
pub(crate) const RESOURCE_TEMPLATES: &[(&str, MediaPermission)] = &[
    ("media://docs/{doc_id}", ModelsRead),
    ("media://models?{query}", ModelsRead),
    ("media://model/{model_id}", ModelsRead),
    ("media://predictions?{query}", PredictionsRead),
    ("media://prediction/{prediction_id}", PredictionsRead),
];

/// Every prompt and the permission it requires: reviewing a run reads predictions, and
/// drafting a run reads the catalog.
pub(crate) const PROMPTS: &[(&str, MediaPermission)] = &[
    ("media-model-select", ModelsRead),
    ("media-image-edit", ModelsRead),
    ("media-video-generate", ModelsRead),
    ("media-task-review", PredictionsRead),
];

/// The permissions Media uses on Artifact to store and verify a run's outputs.
pub(crate) const USES: &[ArtifactPermission] = &[
    ArtifactPermission::ArtifactsRead,
    ArtifactPermission::ArtifactsWrite,
];

/// Media's authorization declaration, checked against its namespace as the Hub
/// checks it.
pub static AUTHORIZATION: LazyLock<AuthorizationDeclaration> =
    LazyLock::new(|| declaration().expect("Media's requirement table forms a valid declaration"));

pub(crate) fn declaration() -> Result<AuthorizationDeclaration, DeclarationError> {
    let invalid = |field: &'static str| DeclarationError::Invalid {
        field,
        reason: "invalid Media target spelling",
    };
    let mut builder = AuthorizationBuilder::<MediaPermission>::new();
    for (tool, permission) in TOOLS {
        builder = builder.tool(
            LocalToolName::parse(*tool).map_err(|_| invalid("tool"))?,
            *permission,
        );
    }
    for (uri, permission) in RESOURCES {
        builder = builder.resource(
            ResourceUri::new(*uri).map_err(|_| invalid("resource"))?,
            *permission,
        );
    }
    for (template, permission) in RESOURCE_TEMPLATES {
        builder = builder.resource_template(
            ResourceTemplateUri::new(*template).map_err(|_| invalid("resource template"))?,
            *permission,
        );
    }
    for (prompt, permission) in PROMPTS {
        builder = builder.prompt(
            PromptName::parse(*prompt).map_err(|_| invalid("prompt"))?,
            *permission,
        );
    }
    for permission in USES {
        builder = builder.uses(*permission);
    }
    builder.build(&MediaContract::slug(), &MediaContract::scheme())
}

/// A request target whose required permission comes from the declaration.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Target<'a> {
    Tool(&'a str),
    Resource(&'a str),
    Prompt(&'a str),
}

/// The verified Hub identity, when it grants the permission `target` requires.
pub(crate) fn require(
    context: &RequestContext<RoleServer>,
    target: Target<'_>,
) -> Result<HubInternalIdentity, McpError> {
    let identity = hub_identity(context)?;
    authorize(&identity, target)?;
    Ok(identity)
}

/// Check the permission `target` requires against the Hub's grants. A target the
/// declaration omits is denied.
pub(crate) fn authorize(
    identity: &HubInternalIdentity,
    target: Target<'_>,
) -> Result<(), McpError> {
    let permission = required(target)?;
    identity.grants(&permission).then_some(()).ok_or_else(|| {
        McpError::invalid_request(
            format!(
                "You don't have permission to make this request. Missing permission `{permission}`."
            ),
            None,
        )
    })
}

pub(crate) fn required(target: Target<'_>) -> Result<PermissionName, McpError> {
    let undeclared = || {
        McpError::invalid_request(
            "This request names no target Media declares a permission for.".to_owned(),
            None,
        )
    };
    let declared = match target {
        Target::Tool(tool) => {
            let tool = LocalToolName::parse(tool).map_err(|_| undeclared())?;
            AUTHORIZATION
                .required(RequirementTarget::Tool(&tool))
                .cloned()
        }
        Target::Resource(uri) => {
            let uri = ResourceUri::new(uri).map_err(|_| undeclared())?;
            AUTHORIZATION
                .required(RequirementTarget::Resource(&uri))
                .cloned()
        }
        Target::Prompt(prompt) => {
            let prompt = PromptName::parse(prompt).map_err(|_| undeclared())?;
            AUTHORIZATION
                .required(RequirementTarget::Prompt(&prompt))
                .cloned()
        }
    };
    declared
        .map(|declared| declared.permission)
        .ok_or_else(undeclared)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn the_declaration_declares_every_permission_and_uses_artifact() {
        let declaration = declaration().unwrap();
        let declared: BTreeSet<String> = declaration
            .permissions
            .iter()
            .map(|permission| permission.permission.as_str().to_owned())
            .collect();
        let expected: BTreeSet<String> = MediaPermission::ALL
            .iter()
            .map(|permission| permission.as_str().to_owned())
            .collect();
        assert_eq!(declared, expected);
        let uses: BTreeSet<String> = declaration.uses.iter().map(ToString::to_string).collect();
        assert_eq!(
            uses,
            BTreeSet::from([
                "artifact.artifacts.read".to_owned(),
                "artifact.artifacts.write".to_owned()
            ])
        );
    }

    #[test]
    fn every_tool_the_server_lists_has_exactly_one_requirement() {
        let listed: BTreeSet<String> = super::super::MediaMcp::tools()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let required: BTreeSet<String> = TOOLS.iter().map(|(tool, _)| (*tool).to_owned()).collect();
        assert_eq!(listed, required);
        assert_eq!(
            required,
            crate::contract::served_tools()
                .map(str::to_owned)
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn concrete_addresses_resolve_and_undeclared_targets_are_denied() {
        for (concrete, permission) in [
            ("media://docs/agents", ModelsRead),
            ("media://models", ModelsRead),
            ("media://models?query=flux&cursor=abc", ModelsRead),
            ("media://model/owner/model/edit", ModelsRead),
            ("media://predictions", PredictionsRead),
            ("media://predictions?cursor=abc", PredictionsRead),
            ("media://prediction/p-1", PredictionsRead),
            ("media://prediction/p-1/result", PredictionsRead),
        ] {
            assert_eq!(
                required(Target::Resource(concrete)).unwrap(),
                permission.name(),
                "{concrete}"
            );
        }
        for (prompt, _) in PROMPTS {
            assert!(required(Target::Prompt(prompt)).is_ok());
        }
        assert!(required(Target::Tool("artifact")).is_err());
        assert!(required(Target::Resource("media://usage")).is_err());
        assert!(required(Target::Resource("media://artifact/x")).is_err());
    }

    #[test]
    fn tool_annotations_agree_with_the_declaration() {
        let tools = super::super::MediaMcp::tools().list_all();
        veoveo_hub_sdk::registration::mcp::check_tool_annotations(&AUTHORIZATION, &tools).unwrap();
    }
}

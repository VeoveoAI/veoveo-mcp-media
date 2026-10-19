//! Registration and operational admission use the same ready routes as dispatch.
use super::setup::MediaContract;
use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use std::sync::LazyLock;
use veoveo_hub_sdk::{
    HubInternalIdentity, LocalToolName,
    hosting::hub_identity,
    registration::{AuthorizationDeclaration, RequirementTarget, RouteAccess},
    server_contract::McpServerContract,
};
use veoveo_types::ResourceUri;

pub static AUTHORIZATION: LazyLock<AuthorizationDeclaration> = LazyLock::new(|| {
    let declaration = super::ROUTES.authorization();
    declaration
        .check(&crate::server(), &MediaContract::scheme())
        .expect("Media route authorization");
    declaration
});

#[derive(Debug, Clone, Copy)]
pub(crate) enum Target<'a> {
    Tool(&'a str),
    Resource(&'a str),
}

pub(crate) fn require(
    context: &RequestContext<RoleServer>,
    target: Target<'_>,
) -> Result<HubInternalIdentity, McpError> {
    let identity = hub_identity(context)?;
    authorize(&identity, target)?;
    Ok(identity)
}

pub(crate) fn authorize(
    identity: &HubInternalIdentity,
    target: Target<'_>,
) -> Result<(), McpError> {
    let undeclared = || McpError::invalid_request("the target is not a declared Media route", None);
    let access = match target {
        Target::Tool(name) => AUTHORIZATION
            .access(RequirementTarget::Tool(
                &LocalToolName::parse(name).map_err(|_| undeclared())?,
            ))
            .cloned(),
        Target::Resource(uri) => AUTHORIZATION
            .access(RequirementTarget::Resource(
                &ResourceUri::new(uri).map_err(|_| undeclared())?,
            ))
            .cloned(),
    }
    .ok_or_else(undeclared)?;
    match access {
        RouteAccess::Authenticated => Ok(()),
        RouteAccess::Permission(permission) if identity.grants(&permission) => Ok(()),
        RouteAccess::Permission(permission) => Err(McpError::invalid_request(
            format!("required route permission `{permission}` was not granted"),
            None,
        )),
    }
}

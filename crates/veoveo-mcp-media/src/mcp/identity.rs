//! The caller facts Media decides on, taken from the identity the Gateway forwards: the
//! run owner its reads filter by, and the Task owner a run starts for.
use rmcp::ErrorData as McpError;
use veoveo_sdk_rust::GatewayInternalIdentity;
use veoveo_task_runtime::{TaskOwner, TaskSurface};

use crate::store::RunOwner;

/// The reader of runs: the caller's tenant, principal and data labels.
pub fn reader(identity: &GatewayInternalIdentity) -> Result<RunOwner, McpError> {
    RunOwner::new(
        &identity.authority.tenant,
        &identity.actor.id,
        identity.actor.data_labels.clone(),
    )
    .map_err(|_| McpError::invalid_request("the forwarded identity names no stored tenant", None))
}

/// The owner of a Task the caller's call starts: its principal, authority and labels on
/// the Gateway profile the call arrived through.
pub fn task_owner(identity: &GatewayInternalIdentity) -> Result<TaskOwner, McpError> {
    let actor = &identity.actor;
    Ok(TaskOwner {
        principal_key: actor.id.clone(),
        principal_kind: match actor.kind {
            veoveo_sdk_rust::PrincipalKind::User => veoveo_task_runtime::PrincipalKind::User,
            veoveo_sdk_rust::PrincipalKind::Service => veoveo_task_runtime::PrincipalKind::Service,
        },
        issuer: actor.issuer.clone(),
        subject: actor.subject.clone(),
        surface: TaskSurface::parse(identity.profile.as_str())
            .map_err(|_| McpError::invalid_request("the Gateway profile names no surface", None))?,
        tenant_key: actor.tenant.clone(),
        data_labels: actor.data_labels.clone(),
        authority: identity.authority.clone(),
    })
}

//! The caller facts Media decides on, taken from the identity the Hub forwards: the
//! Task owner a run starts for and every read of the caller's runs selects by, and the
//! stored tenant of Media's rows.
use rmcp::ErrorData as McpError;
use veoveo_hub_sdk::HubInternalIdentity;
use veoveo_store::TenantId as StoredTenant;
use veoveo_task::{TaskOwner, TaskSurface};

/// The stored tenant of the caller's runs.
pub fn tenant(identity: &HubInternalIdentity) -> Result<StoredTenant, McpError> {
    crate::store::stored_tenant(&identity.authority.tenant).map_err(|_| {
        McpError::invalid_request("the forwarded identity names no stored tenant", None)
    })
}

/// The owner of a Task the caller's call starts, and the selection of the caller's own
/// Tasks: its principal, authority and labels on the Hub profile the call arrived
/// through.
pub fn task_owner(identity: &HubInternalIdentity) -> Result<TaskOwner, McpError> {
    let actor = &identity.actor;
    Ok(TaskOwner {
        principal_key: actor.id.clone(),
        principal_kind: match actor.kind {
            veoveo_hub_sdk::PrincipalKind::User => veoveo_task::PrincipalKind::User,
            veoveo_hub_sdk::PrincipalKind::Service => veoveo_task::PrincipalKind::Service,
        },
        issuer: actor.issuer.clone(),
        subject: actor.subject.clone(),
        surface: TaskSurface::parse(identity.profile.as_str())
            .map_err(|_| McpError::invalid_request("the Hub profile names no surface", None))?,
        tenant_key: actor.tenant.clone(),
        data_labels: actor.data_labels.clone(),
        authority: identity.authority.clone(),
    })
}

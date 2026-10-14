//! Resource subscriptions. A run changes when its Task does, which core's Task runtime
//! announces on the server's Task channel, or when Media's own row does, which the
//! prediction table's trigger announces on the `media` channel; both carry the Task and
//! its tenant. Every replica's watcher reads the run's provider prediction and tells the
//! listeners that the prediction and the prediction listing changed. A listener admits
//! only predictions its caller may read.
use std::sync::Arc;

use futures::StreamExt as _;
use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use veoveo_platform_store::{ChangeSignal, ChangeStream, Channel, ServerSchema};
use veoveo_sdk_rust::{
    hosting::{ResourceSubscriptions, gateway_identity},
    subscriptions::SubscriptionHub,
};
use veoveo_task_runtime::TaskRuntime;
use veoveo_types::{ResourceAddress, TaskId};

use crate::{contract::MediaResource, predictions::Predictions, store::MediaStore};

use super::{
    errors::prediction_error,
    identity,
    requirements::{Target, authorize},
};

/// The channel Media's tables notify: its schema's name.
pub fn channel() -> Channel {
    ServerSchema::new(&crate::server())
        .expect("Media's slug names a server schema")
        .channel()
}

/// Media's subscribable resources and the hub that announces their changes.
#[derive(Clone)]
pub struct MediaSubscriptions {
    predictions: Predictions,
    hub: Arc<SubscriptionHub>,
}

impl MediaSubscriptions {
    pub fn new(predictions: Predictions, hub: Arc<SubscriptionHub>) -> Self {
        Self { predictions, hub }
    }
}

impl ResourceSubscriptions for MediaSubscriptions {
    type Address = MediaResource;

    async fn authorize(
        &self,
        addresses: Vec<MediaResource>,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let identity = gateway_identity(context)?;
        let owner = identity::task_owner(&identity)?;
        for address in addresses {
            if !address.is_subscribable() {
                return Err(McpError::invalid_params(
                    "the resource accepts no subscription",
                    None,
                ));
            }
            let uri = address
                .to_uri()
                .map_err(|_| McpError::invalid_params("invalid Media resource address", None))?;
            authorize(&identity, Target::Resource(uri.as_str()))?;
            if let MediaResource::Prediction(prediction) = &address {
                self.predictions
                    .prediction(&owner, prediction)
                    .await
                    .map_err(prediction_error)?
                    .ok_or_else(|| McpError::invalid_params("no such prediction", None))?;
            }
        }
        Ok(())
    }

    fn hub(&self) -> &SubscriptionHub {
        &self.hub
    }
}

/// Announce every change of a run, on Media's channel or on the server's Task channel,
/// until the stream ends: the changed run's prediction, once the provider named it, and
/// the prediction listing.
pub async fn watch_changes(store: MediaStore, tasks: TaskRuntime, hub: Arc<SubscriptionHub>) {
    let listing = MediaResource::predictions()
        .to_uri()
        .expect("the listing's address")
        .to_string();
    let mut channels = vec![channel()];
    match tasks.channel() {
        Ok(task_channel) => channels.push(task_channel),
        Err(error) => tracing::warn!(%error, "the Media change listener hears no Task changes"),
    }
    let mut changes: ChangeStream = store.platform().listen(channels);
    while let Some(signal) = changes.next().await {
        match signal {
            Ok(ChangeSignal::Reconcile) => hub.notify_resource_contents_changed().await,
            Ok(ChangeSignal::Changed(change)) => {
                let (Some(tenant), Some(task)) = (
                    change.tenant,
                    change
                        .key
                        .as_deref()
                        .and_then(|key| key.parse::<TaskId>().ok()),
                ) else {
                    continue;
                };
                if let Ok(Some(row)) = store.run(tenant, task).await
                    && let Ok(Some(prediction)) = row.prediction_id()
                    && let Ok(uri) = MediaResource::Prediction(prediction).to_uri()
                {
                    hub.notify_resource_updated(uri.to_string()).await;
                }
                hub.notify_resource_updated(listing.clone()).await;
            }
            Err(error) => tracing::warn!(%error, "the Media change listener reconnects"),
        }
    }
}

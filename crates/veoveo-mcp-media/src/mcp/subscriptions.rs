//! Resource subscriptions. The prediction table's trigger notifies the `media` channel
//! with each changed run's Task and tenant; every replica's watcher reads the run's
//! provider prediction and tells the listeners that the prediction and the prediction
//! listing changed. A listener admits only predictions its caller may read.
use std::sync::Arc;

use futures::StreamExt as _;
use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use veoveo_platform_store::{ChangeSignal, ChangeStream, Channel, ServerSchema};
use veoveo_sdk_rust::{
    hosting::{ResourceSubscriptions, gateway_identity},
    subscriptions::SubscriptionHub,
};
use veoveo_types::{ResourceAddress, TaskId};

use crate::{contract::MediaResource, store::MediaStore};

use super::{
    errors::store_error,
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
    store: MediaStore,
    hub: Arc<SubscriptionHub>,
}

impl MediaSubscriptions {
    pub fn new(store: MediaStore, hub: Arc<SubscriptionHub>) -> Self {
        Self { store, hub }
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
        let reader = identity::reader(&identity)?;
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
                self.store
                    .prediction(&reader, prediction)
                    .await
                    .map_err(store_error)?
                    .ok_or_else(|| McpError::invalid_params("no such prediction", None))?;
            }
        }
        Ok(())
    }

    fn hub(&self) -> &SubscriptionHub {
        &self.hub
    }
}

/// Announce every change on the `media` channel until the stream ends: the changed run's
/// prediction, once the provider named it, and the prediction listing.
pub async fn watch_changes(store: MediaStore, hub: Arc<SubscriptionHub>) {
    let listing = MediaResource::predictions()
        .to_uri()
        .expect("the listing's address")
        .to_string();
    let mut changes: ChangeStream = store.platform().listen(vec![channel()]);
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

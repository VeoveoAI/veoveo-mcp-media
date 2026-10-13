//! The provider's side of Media's operations: its signed webhooks, read on the SDK's
//! callback route, and the calls a wait makes for Media, a billing check and a
//! cancellation.
use std::sync::Arc;

use anyhow::{Context, anyhow};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use veoveo_sdk_rust::hosting::operations::{
    ProviderEvent, ProviderEvents, ProviderRejection, ProviderRequest,
};
use veoveo_task_runtime::{
    CheckObservation, ExternalJobId, Operation, OperationEventId, OperationKey, ProviderName,
};

use super::{
    client::ProviderClient,
    webhook::{ID_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER, verify},
    wire::{BillingRecord, Prediction},
};
use crate::contract::MediaPredictionId;

/// How far a webhook's signing time may lie from now.
pub const WEBHOOK_TOLERANCE_SECS: u64 = 300;

/// The operation that runs a generation at the provider and settles through its webhook.
pub fn generation_key() -> OperationKey {
    OperationKey::parse("generate").expect("a valid operation key")
}

/// The provider of generation operations; their jobs are the provider's predictions.
pub fn generation_provider() -> ProviderName {
    ProviderName::parse("generation").expect("a valid provider name")
}

/// The operation that reconciles a finished generation's billing through bounded checks.
pub fn billing_key() -> OperationKey {
    OperationKey::parse("billing").expect("a valid operation key")
}

/// The provider of billing operations: the same provider's billing records, scoped apart
/// so a billing job and its generation share the prediction's identity.
pub fn billing_provider() -> ProviderName {
    ProviderName::parse("billing").expect("a valid provider name")
}

/// What a billing check found: the prediction's deductions and refunds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingOutcome {
    pub records: Vec<BillingRecord>,
}

impl BillingOutcome {
    /// The net amount billed in US dollars, when any record names one.
    pub fn net_usd(&self) -> Option<f64> {
        let amounts: Vec<f64> = self
            .records
            .iter()
            .filter_map(BillingRecord::signed_amount)
            .collect();
        (!amounts.is_empty()).then(|| amounts.into_iter().sum())
    }
}

/// Reads the provider's webhooks: checks the signature with the webhook secret and the
/// signing time, then hands the prediction to the runtime as an event of its job.
#[derive(Clone)]
pub struct ProviderWebhooks {
    secret: Arc<SecretString>,
}

impl ProviderWebhooks {
    pub fn new(secret: SecretString) -> Self {
        Self {
            secret: Arc::new(secret),
        }
    }
}

fn header<'a>(request: &'a ProviderRequest<'_>, name: &str) -> Result<&'a str, ProviderRejection> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or(ProviderRejection::Unauthenticated)
}

impl ProviderEvents for ProviderWebhooks {
    async fn parse(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<ProviderEvent, ProviderRejection> {
        let id = header(&request, ID_HEADER)?;
        let timestamp = header(&request, TIMESTAMP_HEADER)?;
        let signatures = header(&request, SIGNATURE_HEADER)?;
        verify(
            self.secret.expose_secret(),
            id,
            timestamp,
            request.body,
            signatures,
            chrono::Utc::now().timestamp(),
            WEBHOOK_TOLERANCE_SECS,
        )
        .map_err(|error| {
            tracing::debug!(%error, "refused a provider webhook");
            ProviderRejection::Unauthenticated
        })?;
        let event_id = OperationEventId::parse(id).map_err(|_| ProviderRejection::Malformed)?;
        let prediction: Prediction =
            serde_json::from_slice(request.body).map_err(|_| ProviderRejection::Malformed)?;
        Ok(ProviderEvent {
            job: ExternalJobId::parse(prediction.id.as_str()).ok(),
            event_id,
            is_final: prediction.outcome().is_some(),
            payload: serde_json::to_value(&prediction).map_err(|_| ProviderRejection::Malformed)?,
        })
    }
}

/// A billing check: the prediction's deductions and refunds, final once any names an
/// amount. Billing records appear once the provider bills a finished prediction.
pub async fn check_billing(
    client: &ProviderClient,
    prediction: &MediaPredictionId,
) -> anyhow::Result<CheckObservation> {
    let records = client
        .billing(prediction)
        .await
        .context("searching the prediction's billing records")?;
    let outcome = BillingOutcome { records };
    Ok(CheckObservation {
        event_id: None,
        is_final: outcome.net_usd().is_some(),
        payload: serde_json::to_value(&outcome)?,
    })
}

/// A status check of a generation at the provider's status endpoint, final once the
/// prediction reached one of the provider's final statuses.
pub async fn check_status(
    client: &ProviderClient,
    status_url: &str,
) -> anyhow::Result<CheckObservation> {
    let prediction = client
        .status(status_url)
        .await
        .context("reading the prediction's status")?;
    Ok(CheckObservation {
        event_id: None,
        is_final: prediction.outcome().is_some(),
        payload: serde_json::to_value(&prediction)?,
    })
}

/// The provider's prediction an operation names as its job.
pub fn prediction_of(operation: &Operation) -> anyhow::Result<MediaPredictionId> {
    let job = operation
        .external_id
        .as_ref()
        .context("the operation names no provider prediction")?;
    MediaPredictionId::parse(job.as_str()).map_err(|_| anyhow!("the provider job is no prediction"))
}

/// Carry a run's cancellation to the provider and return its receipt. A run whose
/// prediction was never recorded has nothing to delete.
pub async fn cancel_prediction(
    client: &ProviderClient,
    operation: &Operation,
) -> anyhow::Result<Value> {
    let Ok(prediction) = prediction_of(operation) else {
        return Ok(json!({ "deleted": false, "reason": "no provider prediction was recorded" }));
    };
    let receipt = client
        .cancel(&prediction)
        .await
        .context("cancelling the prediction at the provider")?;
    Ok(json!({ "deleted": receipt.deleted_count > 0, "deletedCount": receipt.deleted_count }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_net_bill_sums_deductions_and_refunds() {
        let record = |kind: &str, price: f64| BillingRecord {
            uuid: format!("{kind}-{price}"),
            billing_type: kind.into(),
            price: Some(price),
            created_at: None,
        };
        assert_eq!(BillingOutcome::default().net_usd(), None);
        let outcome = BillingOutcome {
            records: vec![
                record("deduct", 0.25),
                record("refund", 0.05),
                record("other", 9.0),
            ],
        };
        assert!((outcome.net_usd().unwrap() - 0.20).abs() < 1e-9);
    }
}

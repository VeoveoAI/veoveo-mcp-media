//! The generation provider's wire format: its response envelope, its predictions, its
//! model registry and its billing records. Only this module and the client know the
//! provider's field spellings; everything else sees Media's own types.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::{
    MediaModelId, MediaPredictionId, MediaResource, ModelCatalogItem, ModelSchema,
};

/// Every response the provider sends is wrapped in `{code, message, data}`, and `code`
/// 200 means success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    pub data: T,
}

impl<T> Envelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            code: 200,
            message: "success".into(),
            data,
        }
    }
}

/// Where the provider serves one prediction's current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictionUrls {
    pub get: String,
}

/// A prediction as the provider returns it from a submission and its status endpoint,
/// and posts it to the webhook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub id: MediaPredictionId,
    pub model: MediaModelId,
    #[serde(default)]
    pub outputs: Vec<String>,
    /// The prediction's authoritative status endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urls: Option<PredictionUrls>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(
        rename = "executionTime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_time: Option<f64>,
}

/// A documented outcome of a prediction. Any other status settles nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Deleted,
}

impl Prediction {
    /// The prediction's outcome, when its status is one of the provider's final ones.
    pub fn outcome(&self) -> Option<Outcome> {
        match self.status.as_str() {
            "completed" => Some(Outcome::Completed),
            "failed" => Some(Outcome::Failed),
            "cancelled" => Some(Outcome::Cancelled),
            "timeout" => Some(Outcome::TimedOut),
            "deleted" => Some(Outcome::Deleted),
            _ => None,
        }
    }
}

/// One model of the provider's registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub model_id: MediaModelId,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub model_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_schema: Option<Value>,
}

impl ModelEntry {
    /// The JSON Schema of a run's input, when the provider publishes one.
    pub fn request_schema(&self) -> Option<&Value> {
        self.api_schema
            .as_ref()?
            .get("api_schemas")?
            .as_array()?
            .iter()
            .find(|schema| schema.get("type").and_then(Value::as_str) == Some("model_run"))?
            .get("request_schema")
    }

    pub fn catalog_item(&self) -> ModelCatalogItem {
        ModelCatalogItem {
            model_id: self.model_id.clone(),
            name: self.name.clone(),
            model_type: self.model_type.clone(),
            description: self.description.clone(),
            base_price: self.base_price,
            schema_uri: MediaResource::Model(self.model_id.clone()),
        }
    }

    pub fn schema(&self) -> ModelSchema {
        ModelSchema {
            model_id: self.model_id.clone(),
            name: self.name.clone(),
            model_type: self.model_type.clone(),
            description: self.description.clone(),
            base_price: self.base_price,
            formula: self.formula.clone(),
            schema_uri: MediaResource::Model(self.model_id.clone()),
            request_schema: self.request_schema().cloned(),
        }
    }
}

/// The body of a price quote: the model and the run's input, which the provider's
/// pricing formula reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceRequest {
    pub model_id: MediaModelId,
    pub inputs: serde_json::Map<String, Value>,
}

/// The provider's quote for one run: its price in `currency` before any discount, and
/// the discounted price the provider names beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceQuote {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<MediaModelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discounted_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// A page of billing records the provider found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillingSearch {
    #[serde(default)]
    pub items: Vec<BillingRecord>,
}

/// One billing record: a deduction for a prediction, or a refund of one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingRecord {
    pub uuid: String,
    pub billing_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

impl BillingRecord {
    /// The record's amount in US dollars: positive for a deduction, negative for a
    /// refund, and `None` for any other kind of record.
    pub fn signed_amount(&self) -> Option<f64> {
        let price = self.price.filter(|price| price.is_finite())?;
        match self.billing_type.as_str() {
            "deduct" => Some(price),
            "refund" => Some(-price),
            _ => None,
        }
    }
}

/// The provider's answer to a cancellation: how many predictions it deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationReceipt {
    pub deleted_count: u64,
}

/// The body of a cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancellationRequest {
    pub ids: Vec<MediaPredictionId>,
}

/// The body of a billing search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingQuery {
    pub page: u32,
    pub page_size: u32,
    pub prediction_uuids: Vec<MediaPredictionId>,
    pub sort: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_documented_statuses_settle() {
        for (status, outcome) in [
            ("completed", Some(Outcome::Completed)),
            ("failed", Some(Outcome::Failed)),
            ("cancelled", Some(Outcome::Cancelled)),
            ("timeout", Some(Outcome::TimedOut)),
            ("deleted", Some(Outcome::Deleted)),
            ("created", None),
            ("processing", None),
            ("Completed", None),
            ("", None),
        ] {
            let prediction: Prediction = serde_json::from_value(serde_json::json!({
                "id": "p-1", "model": "m/x", "status": status
            }))
            .unwrap();
            assert_eq!(prediction.outcome(), outcome, "{status:?}");
        }
    }

    #[test]
    fn a_registry_entry_keeps_its_provider_bytes_and_finds_its_run_schema() {
        let wire = serde_json::json!({
            "model_id": "owner/model", "name": "model", "type": "text-to-image",
            "description": "open", "base_price": 0.25,
            "api_schema": {"api_schemas": [
                {"type": "other", "request_schema": {"title": "no"}},
                {"type": "model_run", "request_schema": {"type": "object"}}
            ]}
        });
        let entry: ModelEntry = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), wire);
        assert_eq!(
            entry.request_schema(),
            Some(&serde_json::json!({"type": "object"}))
        );
        assert_eq!(
            entry.catalog_item().schema_uri.to_string(),
            "media://model/owner/model"
        );
    }

    #[test]
    fn billing_amounts_sign_deductions_and_refunds_only() {
        let record = |kind: &str, price: Option<f64>| BillingRecord {
            uuid: "b".into(),
            billing_type: kind.into(),
            price,
            created_at: None,
        };
        assert_eq!(record("deduct", Some(0.5)).signed_amount(), Some(0.5));
        assert_eq!(record("refund", Some(0.5)).signed_amount(), Some(-0.5));
        assert_eq!(record("topup", Some(0.5)).signed_amount(), None);
        assert_eq!(record("deduct", Some(f64::NAN)).signed_amount(), None);
        assert_eq!(record("deduct", None).signed_amount(), None);
    }
}

//! The model catalog as Media serves it: pages of models and one model's input schema,
//! in provider-neutral fields.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CatalogFilter, CatalogLimit, ListCursor, MediaModelId, MediaResource};

/// The arguments of `models`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelsRequest {
    /// Case-insensitive search over a model's identity, name, type and description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<CatalogFilter>,
    /// An exact model type, such as `text-to-image`, `image-to-image` or `text-to-video`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub model_type: Option<CatalogFilter>,
    /// How many models the page holds; 20 by default and at most 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<CatalogLimit>,
    /// The previous page's `nextCursor`, with the same filters and limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ListCursor>,
}

/// The arguments of `model_schema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSchemaRequest {
    /// The exact model, such as `owner/model`.
    pub model: MediaModelId,
}

/// One model in a catalog page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalogItem {
    pub model_id: MediaModelId,
    pub name: String,
    #[serde(rename = "type")]
    pub model_type: String,
    pub description: String,
    /// The provider's base price of one run in US dollars, when it publishes one: what a
    /// run costs before its input enters the provider's pricing formula. A run's input
    /// and its defaults can raise its price above this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_price: Option<f64>,
    /// The model's address, `media://model/{+model_id}`, which holds its input schema.
    pub schema_uri: MediaResource,
}

/// One page of the model catalog, in model identity order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalogPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<CatalogFilter>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub model_type: Option<CatalogFilter>,
    /// How many models match the filters in the whole catalog.
    pub total_available: u64,
    pub limit: CatalogLimit,
    pub models: Vec<ModelCatalogItem>,
    /// The cursor of the next page, or `null` on the last page.
    pub next_cursor: Option<ListCursor>,
}

/// One model with the JSON Schema its run input must satisfy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSchema {
    pub model_id: MediaModelId,
    pub name: String,
    #[serde(rename = "type")]
    pub model_type: String,
    pub description: String,
    /// The provider's base price of one run in US dollars, before the run's input enters
    /// the provider's pricing formula.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_price: Option<f64>,
    /// How the provider prices a run, as it publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    pub schema_uri: MediaResource,
    /// The JSON Schema of `run`'s `input` for this model. Its shape is the provider's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_schema: Option<Value>,
}

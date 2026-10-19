//! The model catalog as Media serves it: pages of models and one model's input schema,
//! in provider-neutral fields.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CatalogFilter, MediaModelId, MediaResource};

/// The client-controlled selection of a model catalog page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelFilters {
    /// Case-insensitive search over identity, name, type and description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<CatalogFilter>,
    /// An exact model type, such as `text-to-image`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub model_type: Option<CatalogFilter>,
}

/// The `models` collection tool; its request is `List<ModelsTool>`.
#[veoveo_hub_sdk::collection_tool(
    provider = super::MediaProvider,
    collection = ModelCatalog,
    name = "models",
    default_limit = 20
)]
pub struct ModelsTool;

/// A model registry collection in unique model identity order.
pub struct ModelCatalog;
impl veoveo_types::Collection for ModelCatalog {
    type Item = ModelCatalogItem;
    type Filters = ModelFilters;
    type Key = MediaModelId;
    type Limit = veoveo_types::PageSize<100>;
    type Policy = veoveo_types::RevisionBound;
    const NAME: &'static str = "media.models";
    fn key(item: &ModelCatalogItem) -> MediaModelId {
        item.model_id.clone()
    }
}

/// The arguments of `model_schema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[veoveo_hub_sdk::tool(provider = super::MediaProvider, output = ModelSchema, name = "model_schema")]
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

/// One model with the JSON Schema its run input must satisfy.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, veoveo_hub_sdk::SingleOutput,
)]
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

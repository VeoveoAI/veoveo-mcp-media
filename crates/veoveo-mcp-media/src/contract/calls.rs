//! Media's typed calls and resource routes, shared by clients and hosting.
use veoveo_hub_sdk::{calls::ProviderContract, definition::NoFilters};
use veoveo_types::ServerSlug;

use super::{
    GenerationResult, MediaModelId, MediaPredictionId, MediaResource, ModelCatalog, ModelFilters,
    ModelSchema, PredictionCollection, PredictionView,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaProvider;
impl ProviderContract for MediaProvider {
    fn slug() -> ServerSlug {
        crate::server()
    }
}

#[derive(Debug, Clone)]
#[veoveo_hub_sdk::collection_resource(
    provider = MediaProvider, address_type = MediaResource, collection = ModelCatalog,
    template = "media://models{?query,type}", default_limit = 20,
    address = MediaResource::Models { query: self.filters.query.clone(), model_type: self.filters.model_type.clone() },
    filters = self.filters.clone(),
    parse = |address| match address {
        MediaResource::Models { query, model_type } => Some(Self { filters: ModelFilters {query: query.clone(), model_type: model_type.clone()} }),
        _ => None
    }
)]
pub struct ReadModels {
    pub filters: ModelFilters,
}

#[derive(Debug, Clone)]
#[veoveo_hub_sdk::resource_read(
    provider = MediaProvider, address_type = MediaResource, content = ModelSchema,
    template = "media://model/{+model_id}", address = MediaResource::Model(self.model.clone()),
    parse = |address| match address { MediaResource::Model(model) => Some(Self {model: model.clone()}), _ => None }
)]
pub struct ReadModel {
    pub model: MediaModelId,
}

#[derive(Debug, Clone)]
#[veoveo_hub_sdk::collection_resource(
    provider = MediaProvider, address_type = MediaResource, collection = PredictionCollection,
    template = "media://predictions", address = MediaResource::Predictions,
    filters = NoFilters {},
    parse = |address| match address { MediaResource::Predictions => Some(Self), _ => None }
)]
pub struct ReadPredictions;

#[derive(Debug, Clone)]
#[veoveo_hub_sdk::resource_read(
    provider = MediaProvider, address_type = MediaResource, content = PredictionView,
    template = "media://prediction/{prediction_id}", address = MediaResource::Prediction(self.prediction.clone()),
    parse = |address| match address { MediaResource::Prediction(prediction) => Some(Self {prediction: prediction.clone()}), _ => None }
)]
pub struct ReadPrediction {
    pub prediction: MediaPredictionId,
}

#[derive(Debug, Clone)]
#[veoveo_hub_sdk::resource_read(
    provider = MediaProvider, address_type = MediaResource, content = GenerationResult,
    template = "media://prediction/{prediction_id}/result", address = MediaResource::Result(self.prediction.clone()),
    parse = |address| match address { MediaResource::Result(prediction) => Some(Self {prediction: prediction.clone()}), _ => None }
)]
pub struct ReadResult {
    pub prediction: MediaPredictionId,
}

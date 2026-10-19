//! Typed readers preserve current owner and label admission on every request.
use rmcp::{ErrorData as McpError, RoleServer, service::RequestContext};
use veoveo_hub_sdk::{
    definition::ReadCollection,
    hosting::{ReadCache, ResourceBuilder, RoutePolicy, RoutesBuilder, hub_identity},
};
use veoveo_types::Page;

use super::{
    MediaMcp,
    errors::{catalog_error, prediction_error, store_error},
    identity,
};
use crate::contract::{
    GENERATION_FORMAT, GenerationResult, MediaPermission, ModelCatalog, ModelFilters, ModelSchema,
    PredictionCollection, PredictionStatus, PredictionView, ReadModel, ReadModels, ReadPrediction,
    ReadPredictions, ReadResult,
};

pub(super) fn register(routes: RoutesBuilder<MediaMcp>) -> RoutesBuilder<MediaMcp> {
    let models = || RoutePolicy::permission(MediaPermission::ModelsRead).expect("Media permission");
    let predictions =
        || RoutePolicy::permission(MediaPermission::PredictionsRead).expect("Media permission");
    routes
        .collection_resource::<ReadModels, _, _>(
            Some(ReadModels {
                filters: ModelFilters::default(),
            }),
            ResourceBuilder::new("Models")
                .reader(read_models)
                .policy(models())
                .cache(ReadCache::NoStore),
        )
        .template::<ReadModel, _, _>(
            ResourceBuilder::new("Model")
                .reader(read_model)
                .policy(models())
                .cache(ReadCache::NoStore),
        )
        .collection_resource::<ReadPredictions, _, _>(
            Some(ReadPredictions),
            ResourceBuilder::new("Your predictions")
                .reader(read_predictions)
                .policy(predictions())
                .cache(ReadCache::NoStore),
        )
        .template::<ReadPrediction, _, _>(
            ResourceBuilder::new("Prediction")
                .reader(read_prediction)
                .policy(predictions())
                .cache(ReadCache::NoStore),
        )
        .template::<ReadResult, _, _>(
            ResourceBuilder::new("Generation result")
                .reader(read_result)
                .policy(predictions())
                .cache(ReadCache::NoStore),
        )
}

async fn read_models(
    domain: MediaMcp,
    read: ReadCollection<ReadModels>,
    context: RequestContext<RoleServer>,
) -> Result<Page<ModelCatalog>, McpError> {
    hub_identity(&context)?;
    domain
        .app
        .catalog
        .snapshot()
        .map_err(catalog_error)?
        .page(read.selection())
        .map_err(catalog_error)
}
async fn read_model(
    domain: MediaMcp,
    read: ReadModel,
    context: RequestContext<RoleServer>,
) -> Result<ModelSchema, McpError> {
    hub_identity(&context)?;
    domain
        .app
        .catalog
        .snapshot()
        .map_err(catalog_error)?
        .schema(&read.model)
        .map_err(catalog_error)
}
async fn read_predictions(
    domain: MediaMcp,
    read: ReadCollection<ReadPredictions>,
    context: RequestContext<RoleServer>,
) -> Result<Page<PredictionCollection>, McpError> {
    let owner = identity::task_owner(&hub_identity(&context)?)?;
    domain
        .app
        .predictions
        .page(&owner, read.selection())
        .await
        .map_err(prediction_error)
}
async fn read_prediction(
    domain: MediaMcp,
    read: ReadPrediction,
    context: RequestContext<RoleServer>,
) -> Result<PredictionView, McpError> {
    let owner = identity::task_owner(&hub_identity(&context)?)?;
    let found = domain
        .app
        .predictions
        .prediction(&owner, &read.prediction)
        .await
        .map_err(prediction_error)?
        .ok_or_else(missing)?;
    let prediction = found.summary().map_err(store_error)?;
    let result_uri = (prediction.status == PredictionStatus::Succeeded)
        .then_some(crate::contract::MediaResource::Result(read.prediction));
    Ok(PredictionView {
        prediction,
        result_uri,
    })
}
async fn read_result(
    domain: MediaMcp,
    read: ReadResult,
    context: RequestContext<RoleServer>,
) -> Result<GenerationResult, McpError> {
    let owner = identity::task_owner(&hub_identity(&context)?)?;
    let found = domain
        .app
        .predictions
        .prediction(&owner, &read.prediction)
        .await
        .map_err(prediction_error)?
        .ok_or_else(missing)?;
    let prediction = found.summary().map_err(store_error)?;
    if prediction.status != PredictionStatus::Succeeded {
        return Err(McpError::invalid_params(
            format!(
                "the run is {}, and only a succeeded run has a result",
                prediction.status
            ),
            None,
        ));
    }
    Ok(GenerationResult {
        format: GENERATION_FORMAT.into(),
        task_id: prediction.task_id,
        artifacts: found.outputs().to_vec(),
        prediction,
    })
}
fn missing() -> McpError {
    McpError::invalid_params("no such prediction", None)
}

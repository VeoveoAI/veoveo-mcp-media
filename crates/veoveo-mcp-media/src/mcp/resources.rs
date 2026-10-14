//! Reads of Media's resources. The catalog reads come from the current registry snapshot;
//! every prediction read is private to its caller, because the run's Task decides who
//! may see it.
use rmcp::{
    ErrorData as McpError, RoleServer, model::ReadResourceRequestParams, service::RequestContext,
};
use veoveo_sdk_rust::hosting::{DomainRead, json_read, served_by_host};

use crate::{
    contract::{
        GENERATION_FORMAT, GenerationResult, MediaResource, ModelsRequest, PredictionEntry,
        PredictionListing, PredictionStatus, PredictionView,
    },
    predictions::Prediction,
};

use super::{
    MediaApp,
    errors::{catalog_error, prediction_error, store_error},
    identity,
    requirements::{Target, require},
};

fn missing() -> McpError {
    McpError::invalid_params("no such prediction", None)
}

pub(super) async fn read(
    app: &MediaApp,
    address: MediaResource,
    request: &ReadResourceRequestParams,
    context: &RequestContext<RoleServer>,
) -> Result<DomainRead, McpError> {
    let uri = request.uri.as_str();
    let result = match address {
        MediaResource::Docs | MediaResource::Document(_) | MediaResource::Contract => {
            return Err(served_by_host());
        }
        MediaResource::Models {
            query,
            model_type,
            limit,
            cursor,
        } => {
            require(context, Target::Resource(uri))?;
            let snapshot = app.catalog.snapshot().map_err(catalog_error)?;
            let page = snapshot
                .page(&ModelsRequest {
                    query,
                    model_type,
                    limit,
                    cursor,
                })
                .map_err(catalog_error)?;
            json_read(uri, &page)?
        }
        MediaResource::Model(model) => {
            require(context, Target::Resource(uri))?;
            let snapshot = app.catalog.snapshot().map_err(catalog_error)?;
            json_read(uri, &snapshot.schema(&model).map_err(catalog_error)?)?
        }
        MediaResource::Predictions { cursor } => {
            let identity = require(context, Target::Resource(uri))?;
            let page = app
                .predictions
                .page(&identity::task_owner(&identity)?, cursor.as_ref())
                .await
                .map_err(prediction_error)?;
            let predictions = page.items.iter().map(entry).collect::<Result<_, _>>()?;
            json_read(
                uri,
                &PredictionListing {
                    predictions,
                    next_cursor: page.next,
                },
            )?
        }
        MediaResource::Prediction(prediction) => {
            let identity = require(context, Target::Resource(uri))?;
            let found = app
                .predictions
                .prediction(&identity::task_owner(&identity)?, &prediction)
                .await
                .map_err(prediction_error)?
                .ok_or_else(missing)?;
            let summary = found.summary().map_err(store_error)?;
            let result_uri = (summary.status == PredictionStatus::Succeeded)
                .then(|| MediaResource::Result(prediction.clone()));
            json_read(
                uri,
                &PredictionView {
                    prediction: summary,
                    result_uri,
                },
            )?
        }
        MediaResource::Result(prediction) => {
            let identity = require(context, Target::Resource(uri))?;
            let found = app
                .predictions
                .prediction(&identity::task_owner(&identity)?, &prediction)
                .await
                .map_err(prediction_error)?
                .ok_or_else(missing)?;
            let summary = found.summary().map_err(store_error)?;
            if summary.status != PredictionStatus::Succeeded {
                return Err(McpError::invalid_params(
                    format!(
                        "the run is {}, and only a succeeded run has a result",
                        summary.status
                    ),
                    None,
                ));
            }
            json_read(
                uri,
                &GenerationResult {
                    format: GENERATION_FORMAT.into(),
                    task_id: summary.task_id,
                    result_uri: MediaResource::Result(prediction),
                    artifacts: found.outputs().to_vec(),
                    prediction: summary,
                },
            )?
        }
    };
    Ok(DomainRead::no_store(result))
}

fn entry(found: &Prediction) -> Result<PredictionEntry, McpError> {
    let prediction = found.summary().map_err(store_error)?;
    Ok(PredictionEntry {
        prediction_uri: prediction
            .prediction_id
            .clone()
            .map(MediaResource::Prediction),
        prediction,
    })
}

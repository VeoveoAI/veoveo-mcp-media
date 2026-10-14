//! How a refusal or failure reaches the caller. A refusal names its rule; a failure of
//! the database or the Task runtime is logged and reported without detail.
use rmcp::ErrorData as McpError;

use crate::{catalog::CatalogError, predictions::PredictionError, store::MediaStoreError};

pub(crate) fn catalog_error(error: CatalogError) -> McpError {
    match error {
        CatalogError::UnknownModel(_) | CatalogError::StaleCursor => {
            McpError::invalid_params(error.to_string(), None)
        }
        CatalogError::Unavailable => McpError::invalid_request(error.to_string(), None),
        failure @ (CatalogError::InvalidRegistry(_) | CatalogError::Provider(_)) => {
            internal(&failure)
        }
    }
}

pub(crate) fn store_error(error: MediaStoreError) -> McpError {
    match error {
        MediaStoreError::InvalidTenant => McpError::invalid_request(error.to_string(), None),
        failure @ (MediaStoreError::Corrupt(_)
        | MediaStoreError::PredictionConflict(_)
        | MediaStoreError::Database(_)) => internal(&failure),
    }
}

pub(crate) fn prediction_error(error: PredictionError) -> McpError {
    match error {
        PredictionError::StaleCursor => McpError::invalid_params(error.to_string(), None),
        PredictionError::Store(error) => store_error(error),
        PredictionError::Tasks(error) => internal(&error),
    }
}

pub(crate) fn internal(failure: &dyn std::fmt::Display) -> McpError {
    tracing::error!(%failure, "a Media request failed");
    McpError::internal_error("Media could not complete the request", None)
}

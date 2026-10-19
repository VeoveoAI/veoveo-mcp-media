//! Media's public types. Consumers select `default-features = false,
//! features = ["contract"]`, which builds without a database, runtime or provider
//! dependency.

mod calls;
mod catalog;
mod collections;
mod generation;
mod ids;
mod permissions;
mod resource;

pub use calls::{
    MediaProvider, ReadModel, ReadModels, ReadPrediction, ReadPredictions, ReadResult,
};
pub use catalog::{
    ModelCatalog, ModelCatalogItem, ModelFilters, ModelSchema, ModelSchemaRequest, ModelsTool,
};
pub use collections::{PredictionCollection, PredictionKeyset, PredictionPosition};
pub use generation::{
    CompletionMode, GENERATION_COMPLETED, GENERATION_FORMAT, GeneratedArtifact, GenerationResult,
    PredictionEntry, PredictionStatus, PredictionSummary, PredictionView, RunFailureCode,
    RunRequest,
};
pub use ids::{
    MAX_MODEL_ID_BYTES, MAX_PREDICTION_ID_BYTES, MediaIdError, MediaModelId, MediaPredictionId,
};
pub use permissions::{MediaPermission, MediaScope};
pub use resource::{
    CatalogFilter, MAX_FILTER_BYTES, MediaDocument, MediaResource, MediaResourceError,
};

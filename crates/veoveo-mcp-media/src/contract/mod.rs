//! Media's public types. Consumers select `default-features = false,
//! features = ["contract"]`, which builds without a database, runtime or provider
//! dependency.

mod calls;
mod catalog;
mod generation;
mod ids;
mod permissions;
mod resource;

pub use calls::{MediaProvider, served_tools};
pub use catalog::{
    ModelCatalogItem, ModelCatalogPage, ModelSchema, ModelSchemaRequest, ModelsRequest,
};
pub use generation::{
    GENERATION_COMPLETED, GENERATION_FORMAT, GeneratedArtifact, GenerationResult, PredictionEntry,
    PredictionListing, PredictionStatus, PredictionSummary, PredictionView, RunRequest,
};
pub use ids::{
    ListCursor, MAX_CURSOR_BYTES, MAX_MODEL_ID_BYTES, MAX_PREDICTION_ID_BYTES, MediaIdError,
    MediaModelId, MediaPredictionId,
};
pub use permissions::{MediaPermission, MediaScope};
pub use resource::{
    CatalogFilter, CatalogLimit, DEFAULT_MODELS_PER_PAGE, MAX_FILTER_BYTES, MAX_MODELS_PER_PAGE,
    MediaDocument, MediaResource, MediaResourceError,
};
